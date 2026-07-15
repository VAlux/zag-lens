use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hooks::{self, HookOwnership};
use crate::model::{
    ApplyReport, Component, Conflict, FileChange, InstallError, InstallPlan, Notice, Operation,
    PlanContext, Selection,
};
use crate::paths::InstallPaths;
use crate::zellij::{self, ZellijOwnership};

const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct ManifestComponents {
    #[serde(skip_serializing_if = "Option::is_none")]
    zellij: Option<ZellijOwnership>,
    #[serde(skip_serializing_if = "Option::is_none")]
    codex: Option<HookOwnership>,
    #[serde(skip_serializing_if = "Option::is_none")]
    claude: Option<HookOwnership>,
}

impl ManifestComponents {
    fn is_empty(&self) -> bool {
        self.zellij.is_none() && self.codex.is_none() && self.claude.is_none()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct InstallManifest {
    schema_version: u32,
    installed_at: String,
    components: ManifestComponents,
}

impl InstallManifest {
    fn empty(timestamp: &str) -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            installed_at: timestamp.to_owned(),
            components: ManifestComponents::default(),
        }
    }
}

/// Plans idempotent setup and uninstall operations for resolved user paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Installer {
    paths: InstallPaths,
}

impl Installer {
    #[must_use]
    pub fn new(paths: InstallPaths) -> Self {
        Self { paths }
    }

    /// Resolves the installer from the current process environment.
    ///
    /// # Errors
    ///
    /// Returns [`InstallError::MissingHome`] when `HOME` is unset or empty.
    pub fn from_current_environment() -> Result<Self, InstallError> {
        Ok(Self::new(InstallPaths::from_current_environment()?))
    }

    #[must_use]
    pub fn paths(&self) -> &InstallPaths {
        &self.paths
    }

    /// Reads configuration and returns a complete setup plan without writing.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable or invalid configuration, unsupported
    /// manifests, or configuration entries owned by another installation.
    #[allow(clippy::too_many_lines)]
    pub fn plan_setup(
        &self,
        selection: &Selection,
        context: &PlanContext,
    ) -> Result<InstallPlan, InstallError> {
        if selection.is_empty() {
            return Ok(empty_plan());
        }

        let manifest_bytes = read_optional(&self.paths.manifest)?;
        let original_manifest = parse_manifest(manifest_bytes.as_deref(), &self.paths.manifest)?;
        let mut manifest = original_manifest
            .clone()
            .unwrap_or_else(|| InstallManifest::empty(&context.timestamp));
        let mut changes = Vec::new();
        let mut notices = Vec::new();
        let mut conflicts = Vec::new();

        if selection.contains(Component::Zellij) {
            let current = read_optional(&self.paths.zellij_config)?;
            match zellij::setup(
                current.as_deref(),
                &self.paths.zellij_config,
                &self.paths.plugin,
                &self.paths.binary,
                manifest.components.zellij.as_ref(),
            ) {
                Ok(setup) => {
                    if setup.changed {
                        changes.push(write_change(
                            Component::Zellij,
                            self.paths.zellij_config.clone(),
                            current,
                            setup.bytes,
                            context,
                            "register the Zellij plugin alias and background plugin",
                        ));
                    }
                    manifest.components.zellij = Some(setup.ownership);
                }
                Err(InstallError::Conflicts(mut found)) => conflicts.append(&mut found),
                Err(error) => return Err(error),
            }
        }

        if selection.contains(Component::Codex) {
            let current = read_optional(&self.paths.codex_hooks)?;
            match hooks::setup(
                current.as_deref(),
                &self.paths.codex_hooks,
                &self.paths.binary,
                Component::Codex,
                manifest.components.codex.as_ref(),
            ) {
                Ok(setup) => {
                    if setup.changed {
                        changes.push(write_change(
                            Component::Codex,
                            self.paths.codex_hooks.clone(),
                            current,
                            setup.bytes,
                            context,
                            "register observational Codex lifecycle hooks",
                        ));
                    }
                    manifest.components.codex = Some(setup.ownership);
                    notices.push(Notice {
                        component: Component::Codex,
                        message: "Review the installed Zag Lens hook definitions and trust them using the Codex /hooks workflow before expecting events.".to_owned(),
                    });
                }
                Err(InstallError::Conflicts(mut found)) => conflicts.append(&mut found),
                Err(error) => return Err(error),
            }
        }

        if selection.contains(Component::Claude) {
            let current = read_optional(&self.paths.claude_settings)?;
            match hooks::setup(
                current.as_deref(),
                &self.paths.claude_settings,
                &self.paths.binary,
                Component::Claude,
                manifest.components.claude.as_ref(),
            ) {
                Ok(setup) => {
                    if setup.changed {
                        changes.push(write_change(
                            Component::Claude,
                            self.paths.claude_settings.clone(),
                            current,
                            setup.bytes,
                            context,
                            "register observational Claude Code lifecycle hooks",
                        ));
                    }
                    manifest.components.claude = Some(setup.ownership);
                }
                Err(InstallError::Conflicts(mut found)) => conflicts.append(&mut found),
                Err(error) => return Err(error),
            }
        }

        if !conflicts.is_empty() {
            return Err(InstallError::Conflicts(conflicts));
        }

        let manifest_changed = original_manifest.as_ref() != Some(&manifest);
        if manifest_changed {
            manifest.installed_at.clone_from(&context.timestamp);
            let replacement = encode_manifest(&manifest)?;
            changes.push(FileChange {
                component: None,
                path: self.paths.manifest.clone(),
                operation: Operation::Write,
                backup_path: None,
                description: "record Zag Lens-owned configuration entries".to_owned(),
                original: manifest_bytes,
                replacement: Some(replacement),
            });
        }

        Ok(InstallPlan { changes, notices })
    }

    /// Reads configuration and plans removal of only manifest-owned entries.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable or invalid configuration, unsupported
    /// manifests, or externally modified entries recorded as installer-owned.
    pub fn plan_uninstall(
        &self,
        selection: &Selection,
        context: &PlanContext,
    ) -> Result<InstallPlan, InstallError> {
        if selection.is_empty() {
            return Ok(empty_plan());
        }
        let manifest_bytes = read_optional(&self.paths.manifest)?;
        let Some(original_manifest) =
            parse_manifest(manifest_bytes.as_deref(), &self.paths.manifest)?
        else {
            return Ok(empty_plan());
        };
        let mut manifest = original_manifest.clone();
        let mut changes = Vec::new();
        let mut conflicts = Vec::new();

        if selection.contains(Component::Zellij)
            && let Some(ownership) = manifest.components.zellij.as_ref()
        {
            let current = read_optional(&self.paths.zellij_config)?;
            match zellij::uninstall(current.as_deref(), &self.paths.zellij_config, ownership) {
                Ok(uninstall) => {
                    if uninstall.changed {
                        changes.push(write_change(
                            Component::Zellij,
                            self.paths.zellij_config.clone(),
                            current,
                            uninstall.bytes,
                            context,
                            "remove owned Zellij plugin entries",
                        ));
                    }
                    manifest.components.zellij = None;
                }
                Err(InstallError::Conflicts(mut found)) => conflicts.append(&mut found),
                Err(error) => return Err(error),
            }
        }

        Self::plan_hook_uninstall(
            selection,
            Component::Codex,
            &self.paths.codex_hooks,
            manifest.components.codex.as_ref(),
            context,
            &mut changes,
            &mut conflicts,
        )?;
        if selection.contains(Component::Codex)
            && !has_component_conflict(&conflicts, Component::Codex)
        {
            manifest.components.codex = None;
        }

        Self::plan_hook_uninstall(
            selection,
            Component::Claude,
            &self.paths.claude_settings,
            manifest.components.claude.as_ref(),
            context,
            &mut changes,
            &mut conflicts,
        )?;
        if selection.contains(Component::Claude)
            && !has_component_conflict(&conflicts, Component::Claude)
        {
            manifest.components.claude = None;
        }

        if !conflicts.is_empty() {
            return Err(InstallError::Conflicts(conflicts));
        }

        if manifest != original_manifest {
            if manifest.components.is_empty() {
                changes.push(FileChange {
                    component: None,
                    path: self.paths.manifest.clone(),
                    operation: Operation::Remove,
                    backup_path: None,
                    description: "remove the empty Zag Lens ownership manifest".to_owned(),
                    original: manifest_bytes,
                    replacement: None,
                });
            } else {
                let replacement = encode_manifest(&manifest)?;
                changes.push(FileChange {
                    component: None,
                    path: self.paths.manifest.clone(),
                    operation: Operation::Write,
                    backup_path: None,
                    description: "update the Zag Lens ownership manifest".to_owned(),
                    original: manifest_bytes,
                    replacement: Some(replacement),
                });
            }
        }

        Ok(InstallPlan {
            changes,
            notices: Vec::new(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_hook_uninstall(
        selection: &Selection,
        component: Component,
        path: &Path,
        ownership: Option<&HookOwnership>,
        context: &PlanContext,
        changes: &mut Vec<FileChange>,
        conflicts: &mut Vec<Conflict>,
    ) -> Result<(), InstallError> {
        if !selection.contains(component) {
            return Ok(());
        }
        let Some(ownership) = ownership else {
            return Ok(());
        };
        let current = read_optional(path)?;
        match hooks::uninstall(current.as_deref(), path, component, ownership) {
            Ok(uninstall) => {
                if uninstall.changed {
                    changes.push(write_change(
                        component,
                        path.to_path_buf(),
                        current,
                        uninstall.bytes,
                        context,
                        "remove owned lifecycle hook commands",
                    ));
                }
            }
            Err(InstallError::Conflicts(mut found)) => conflicts.append(&mut found),
            Err(error) => return Err(error),
        }
        Ok(())
    }
}

fn has_component_conflict(conflicts: &[Conflict], component: Component) -> bool {
    conflicts
        .iter()
        .any(|conflict| conflict.component == component)
}

fn empty_plan() -> InstallPlan {
    InstallPlan {
        changes: Vec::new(),
        notices: Vec::new(),
    }
}

fn parse_manifest(
    bytes: Option<&[u8]>,
    path: &Path,
) -> Result<Option<InstallManifest>, InstallError> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let manifest: InstallManifest =
        serde_json::from_slice(bytes).map_err(|error| InstallError::InvalidConfig {
            component: Component::Zellij,
            path: path.to_path_buf(),
            message: format!("invalid ownership manifest: {error}"),
        })?;
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(InstallError::UnsupportedManifest {
            path: path.to_path_buf(),
            schema_version: manifest.schema_version,
        });
    }
    Ok(Some(manifest))
}

fn encode_manifest(manifest: &InstallManifest) -> Result<Vec<u8>, InstallError> {
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| InstallError::Serialization(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_change(
    component: Component,
    path: PathBuf,
    original: Option<Vec<u8>>,
    replacement: Vec<u8>,
    context: &PlanContext,
    description: &str,
) -> FileChange {
    let backup_path = original
        .as_ref()
        .map(|_| next_backup_path(&path, &context.backup_label));
    FileChange {
        component: Some(component),
        path,
        operation: Operation::Write,
        backup_path,
        description: description.to_owned(),
        original,
        replacement: Some(replacement),
    }
}

fn next_backup_path(path: &Path, label: &str) -> PathBuf {
    let file_name = path.file_name().map_or_else(
        || "config".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    for suffix in 0_u32.. {
        let suffix = if suffix == 0 {
            String::new()
        } else {
            format!("-{suffix}")
        };
        let candidate = path.with_file_name(format!("{file_name}.zag-lens-backup-{label}{suffix}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("u32 backup suffix space exhausted")
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, InstallError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(InstallError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub(crate) fn apply_plan(plan: &InstallPlan, dry_run: bool) -> Result<ApplyReport, InstallError> {
    let changed_paths = plan
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect::<Vec<_>>();
    let backups = plan
        .changes
        .iter()
        .filter_map(|change| change.backup_path.clone())
        .collect::<Vec<_>>();
    if dry_run {
        return Ok(ApplyReport {
            dry_run: true,
            changed_paths,
            backups,
            notices: plan.notices.clone(),
        });
    }

    for change in &plan.changes {
        if read_optional(&change.path)? != change.original {
            return Err(InstallError::ConcurrentModification(change.path.clone()));
        }
    }

    for change in &plan.changes {
        if let (Some(backup_path), Some(original)) = (&change.backup_path, &change.original) {
            create_backup(backup_path, original)?;
        }
    }

    let mut applied: Vec<usize> = Vec::new();
    for (index, change) in plan.changes.iter().enumerate() {
        let result = match &change.replacement {
            Some(replacement) => atomic_write(&change.path, replacement, index),
            None => remove_file_if_present(&change.path),
        };
        if let Err(error) = result {
            for rollback_index in applied.iter().rev().copied() {
                let rollback = &plan.changes[rollback_index];
                let _ = match &rollback.original {
                    Some(original) => atomic_write(&rollback.path, original, rollback_index),
                    None => remove_file_if_present(&rollback.path),
                };
            }
            return Err(error);
        }
        applied.push(index);
    }

    Ok(ApplyReport {
        dry_run: false,
        changed_paths,
        backups,
        notices: plan.notices.clone(),
    })
}

fn create_backup(path: &Path, contents: &[u8]) -> Result<(), InstallError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| InstallError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| InstallError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(contents).map_err(|source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn atomic_write(path: &Path, contents: &[u8], index: usize) -> Result<(), InstallError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| InstallError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let name = path.file_name().map_or_else(
        || "config".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temporary = parent.join(format!(
        ".{name}.zag-lens.tmp-{}-{index}",
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|source| InstallError::Io {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(contents)
            .and_then(|()| file.sync_all())
            .map_err(|source| InstallError::Io {
                path: temporary.clone(),
                source,
            })?;
        if let Ok(metadata) = fs::metadata(path) {
            fs::set_permissions(&temporary, metadata.permissions()).map_err(|source| {
                InstallError::Io {
                    path: temporary.clone(),
                    source,
                }
            })?;
        }
        fs::rename(&temporary, path).map_err(|source| InstallError::Io {
            path: path.to_path_buf(),
            source,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn remove_file_if_present(path: &Path) -> Result<(), InstallError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(InstallError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Restores a selected backup using the same atomic write path as setup.
///
/// # Errors
///
/// Returns an error when the backup cannot be read or the destination cannot
/// be replaced atomically.
pub fn restore_backup(backup: &Path, destination: &Path) -> Result<(), InstallError> {
    let contents = fs::read(backup).map_err(|source| InstallError::Io {
        path: backup.to_path_buf(),
        source,
    })?;
    atomic_write(destination, &contents, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::PathEnvironment;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Installer, PlanContext) {
        let temporary = TempDir::new().expect("temporary directory");
        let environment = PathEnvironment {
            home: temporary.path().to_path_buf(),
            xdg_bin_home: None,
            xdg_config_home: None,
            xdg_data_home: None,
            codex_home: None,
            claude_config_dir: None,
        };
        let installer = Installer::new(InstallPaths::resolve(&environment));
        let context =
            PlanContext::new("2026-07-13T12:00:00Z", "20260713T120000Z").expect("valid context");
        (temporary, installer, context)
    }

    #[test]
    fn setup_is_idempotent() {
        let (_temporary, installer, context) = fixture();
        let first = installer
            .plan_setup(&Selection::all(), &context)
            .expect("first setup plan");
        assert!(!first.is_empty());
        first.apply(false).expect("first setup applies");

        let second = installer
            .plan_setup(&Selection::all(), &context)
            .expect("second setup plan");
        assert!(second.is_empty());
    }

    #[test]
    fn dry_run_does_not_touch_files() {
        let (_temporary, installer, context) = fixture();
        let plan = installer
            .plan_setup(&Selection::all(), &context)
            .expect("setup plan");
        let report = plan.apply(true).expect("dry run succeeds");
        assert!(report.dry_run);
        assert!(!installer.paths().manifest.exists());
        assert!(!installer.paths().zellij_config.exists());
    }

    #[test]
    fn uninstall_preserves_unrelated_configuration() {
        let (_temporary, installer, context) = fixture();
        fs::create_dir_all(
            installer
                .paths()
                .zellij_config
                .parent()
                .expect("config parent"),
        )
        .expect("create config parent");
        fs::write(
            &installer.paths().zellij_config,
            "plugins {\n    status-bar location=\"zellij:status-bar\"\n}\ntheme \"dracula\"\n",
        )
        .expect("seed Zellij config");
        fs::create_dir_all(
            installer
                .paths()
                .codex_hooks
                .parent()
                .expect("hooks parent"),
        )
        .expect("create hooks parent");
        fs::write(
            &installer.paths().codex_hooks,
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"mine"}]}]},"theme":"dark"}"#,
        )
        .expect("seed Codex hooks");

        installer
            .plan_setup(&Selection::all(), &context)
            .expect("setup plan")
            .apply(false)
            .expect("setup applies");
        installer
            .plan_uninstall(&Selection::all(), &context)
            .expect("uninstall plan")
            .apply(false)
            .expect("uninstall applies");

        let zellij =
            fs::read_to_string(&installer.paths().zellij_config).expect("read Zellij config");
        assert!(zellij.contains("status-bar"));
        assert!(zellij.contains("theme \"dracula\""));
        assert!(!zellij.contains("zag-lens"));
        let codex: serde_json::Value = serde_json::from_slice(
            &fs::read(&installer.paths().codex_hooks).expect("read Codex hooks"),
        )
        .expect("valid Codex hooks");
        assert_eq!(codex["theme"], "dark");
        assert_eq!(codex["hooks"]["Stop"][0]["hooks"][0]["command"], "mine");
        assert!(!installer.paths().manifest.exists());
    }

    #[test]
    fn uninstall_does_not_remove_matching_preexisting_hook() {
        let (_temporary, installer, context) = fixture();
        let path = &installer.paths().codex_hooks;
        fs::create_dir_all(path.parent().expect("hooks parent")).expect("create hooks parent");
        let command = format!(
            "'{}' hook --harness codex --event Stop",
            installer.paths().binary.display()
        );
        let existing = serde_json::json!({
            "hooks": {
                "Stop": [{
                    "hooks": [{"type": "command", "command": command.clone()}]
                }]
            }
        });
        fs::write(
            path,
            serde_json::to_vec_pretty(&existing).expect("serialize fixture"),
        )
        .expect("seed hooks");

        let codex = Selection::from_components([Component::Codex]);
        installer
            .plan_setup(&codex, &context)
            .expect("setup plan")
            .apply(false)
            .expect("setup applies");
        installer
            .plan_uninstall(&codex, &context)
            .expect("uninstall plan")
            .apply(false)
            .expect("uninstall applies");

        let hooks: serde_json::Value =
            serde_json::from_slice(&fs::read(path).expect("read hooks")).expect("valid hooks");
        assert_eq!(hooks["hooks"]["Stop"][0]["hooks"][0]["command"], command);
    }

    #[test]
    fn existing_files_are_backed_up_and_can_be_restored() {
        let (_temporary, installer, context) = fixture();
        let path = &installer.paths().claude_settings;
        fs::create_dir_all(path.parent().expect("settings parent"))
            .expect("create settings parent");
        let original = br#"{"theme":"light"}"#;
        fs::write(path, original).expect("seed settings");

        let plan = installer
            .plan_setup(&Selection::from_components([Component::Claude]), &context)
            .expect("setup plan");
        let report = plan.apply(false).expect("setup applies");
        assert_eq!(report.backups.len(), 1);
        assert_eq!(fs::read(&report.backups[0]).expect("read backup"), original);

        restore_backup(&report.backups[0], path).expect("restore succeeds");
        assert_eq!(fs::read(path).expect("read restored settings"), original);
    }

    #[test]
    fn applying_stale_plan_reports_concurrent_modification() {
        let (_temporary, installer, context) = fixture();
        let plan = installer
            .plan_setup(&Selection::from_components([Component::Codex]), &context)
            .expect("setup plan");
        fs::create_dir_all(
            installer
                .paths()
                .codex_hooks
                .parent()
                .expect("hooks parent"),
        )
        .expect("create parent");
        fs::write(&installer.paths().codex_hooks, "{}").expect("external edit");
        let error = plan.apply(false).expect_err("stale plan must fail");
        assert!(matches!(error, InstallError::ConcurrentModification(_)));
    }

    #[test]
    fn conflicting_zellij_alias_is_reported_without_writes() {
        let (_temporary, installer, context) = fixture();
        fs::create_dir_all(
            installer
                .paths()
                .zellij_config
                .parent()
                .expect("config parent"),
        )
        .expect("create config parent");
        fs::write(
            &installer.paths().zellij_config,
            "plugins {\n    zag-lens location=\"file:/someone-else.wasm\"\n}\n",
        )
        .expect("seed config");
        let error = installer
            .plan_setup(&Selection::all(), &context)
            .expect_err("conflict expected");
        let InstallError::Conflicts(conflicts) = error else {
            panic!("expected conflicts");
        };
        assert_eq!(conflicts.len(), 1);
        assert!(!installer.paths().manifest.exists());
    }
}
