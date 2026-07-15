use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zag_lens::{HookEnvironment, HookOutcome, ZellijPipeTransport, process_hook};
use zag_lens_claude_adapter::ClaudeAdapter;
use zag_lens_codex_adapter::CodexAdapter;
use zag_lens_installer::{Component, InstallPaths, Installer, Operation, PlanContext, Selection};
use zag_lens_notifier::{
    BackendConfig, CommandConfig, DeliveryStatus, Notification, deliver, sanitize_field,
};

const ZELLIJ_BASELINE: &str = "0.44.1";
const CODEX_BASELINE: &str = "0.144.3";
const CLAUDE_BASELINE: &str = "2.1.207";

#[derive(Debug, Parser)]
#[command(
    name = "zag-lens",
    version,
    about = "Agent status integration for Zellij"
)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Normalize one lifecycle hook and send it to Zellij.
    Hook(HookArgs),
    /// Send a privacy-sanitized host notification.
    Notify(NotifyArgs),
    /// Install assets and configure selected integrations.
    Setup(SetupArgs),
    /// Remove selected integrations and owned assets.
    Uninstall(UninstallArgs),
    /// Report prerequisite versions and resolved installation paths.
    Doctor,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Harness {
    Codex,
    Claude,
}

#[derive(Debug, Args)]
struct HookArgs {
    #[arg(long, value_enum)]
    harness: Harness,
    #[arg(long)]
    event: String,
    /// Write only sanitized failure categories to stderr.
    #[arg(long)]
    debug: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum NotificationBackend {
    Auto,
    Command,
    Bell,
    Off,
}

#[derive(Debug, Args)]
struct NotifyArgs {
    #[arg(long, value_enum, default_value_t = NotificationBackend::Auto)]
    backend: NotificationBackend,
    #[arg(long)]
    title: String,
    #[arg(long)]
    body: String,
    /// Executable used by the command backend.
    #[arg(long)]
    command: Option<PathBuf>,
    /// Fixed argument passed before the sanitized title and body.
    #[arg(long = "command-arg", allow_hyphen_values = true)]
    command_args: Vec<OsString>,
    /// Write a sanitized delivery failure to stderr.
    #[arg(long)]
    debug: bool,
}

#[derive(Clone, Debug, Default, Args)]
#[allow(clippy::struct_excessive_bools)] // These are independent CLI selector flags.
struct ComponentArgs {
    /// Select every integration (also the default when no selector is given).
    #[arg(long)]
    all: bool,
    #[arg(long)]
    zellij: bool,
    #[arg(long)]
    codex: bool,
    #[arg(long)]
    claude: bool,
}

impl ComponentArgs {
    fn selection(&self) -> Selection {
        if self.all || !(self.zellij || self.codex || self.claude) {
            return Selection::all();
        }
        let mut components = Vec::with_capacity(3);
        if self.zellij {
            components.push(Component::Zellij);
        }
        if self.codex {
            components.push(Component::Codex);
        }
        if self.claude {
            components.push(Component::Claude);
        }
        Selection::from_components(components)
    }
}

#[derive(Debug, Args)]
struct SetupArgs {
    #[command(flatten)]
    components: ComponentArgs,
    /// Source Zellij plugin WASM. Required when Zellij is selected.
    #[arg(long)]
    plugin_wasm: Option<PathBuf>,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct UninstallArgs {
    #[command(flatten)]
    components: ComponentArgs,
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let hook_invocation = arguments.get(1).is_some_and(|arg| arg == "hook");
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(_error) if hook_invocation => {
            if debug_from_environment() {
                eprintln!("zag-lens hook: invalid command arguments");
            }
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            let _ = error.print();
            return ExitCode::from(code);
        }
    };

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zag-lens: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        CliCommand::Hook(args) => {
            run_hook(&args);
            Ok(())
        }
        CliCommand::Notify(args) => {
            run_notification(&args);
            Ok(())
        }
        CliCommand::Setup(args) => run_setup(&args),
        CliCommand::Uninstall(args) => run_uninstall(&args),
        CliCommand::Doctor => run_doctor(),
    }
}

fn run_hook(args: &HookArgs) {
    let environment = HookEnvironment::from_current_process();
    let transport = ZellijPipeTransport::default();
    let stdin = io::stdin();
    let outcome = match args.harness {
        Harness::Codex => process_hook(
            &CodexAdapter,
            &args.event,
            stdin.lock(),
            &environment,
            &transport,
        ),
        Harness::Claude => process_hook(
            &ClaudeAdapter,
            &args.event,
            stdin.lock(),
            &environment,
            &transport,
        ),
    };

    if (args.debug || debug_from_environment())
        && let HookOutcome::Failed(failure) = outcome
    {
        eprintln!("zag-lens hook: {failure}");
    }
}

fn run_notification(args: &NotifyArgs) {
    let config = match args.backend {
        NotificationBackend::Auto => BackendConfig::Auto,
        NotificationBackend::Command => {
            let Some(program) = args.command.clone() else {
                debug_notification(args, "command backend requires --command");
                return;
            };
            BackendConfig::Command(CommandConfig::new(program, args.command_args.clone()))
        }
        NotificationBackend::Bell => BackendConfig::Bell,
        NotificationBackend::Off => BackendConfig::Off,
    };
    let Ok(notifier) = config.build() else {
        debug_notification(args, "notification backend configuration failed");
        return;
    };
    let notification = Notification::new(&args.title, &args.body);
    if let DeliveryStatus::Failed(error) = deliver(notifier.as_ref(), &notification) {
        debug_notification(args, &error.to_string());
    }
}

fn debug_notification(args: &NotifyArgs, message: &str) {
    if args.debug || debug_from_environment() {
        eprintln!("zag-lens notify: {}", sanitize_field(message, 256));
    }
}

fn run_setup(args: &SetupArgs) -> Result<(), String> {
    let selection = args.components.selection();
    let installer = Installer::from_current_environment().map_err(|error| error.to_string())?;
    let context = plan_context()?;
    let plan = installer
        .plan_setup(&selection, &context)
        .map_err(|error| format_install_error(&error))?;
    let assets = setup_assets(&selection, installer.paths(), args.plugin_wasm.as_deref())?;

    for asset in &assets {
        if args.dry_run {
            println!(
                "would install {} -> {}",
                asset.source.display(),
                asset.destination.display()
            );
        } else if atomic_copy(&asset.source, &asset.destination)? {
            println!("installed {}", asset.destination.display());
        }
    }
    print_plan(&plan, args.dry_run);
    let report = plan
        .apply(args.dry_run)
        .map_err(|error| error.to_string())?;
    for notice in report.notices {
        println!("notice: {}", notice.message);
    }
    Ok(())
}

fn run_uninstall(args: &UninstallArgs) -> Result<(), String> {
    let selection = args.components.selection();
    let installer = Installer::from_current_environment().map_err(|error| error.to_string())?;
    let context = plan_context()?;
    let plan = installer
        .plan_uninstall(&selection, &context)
        .map_err(|error| format_install_error(&error))?;

    print_plan(&plan, args.dry_run);
    plan.apply(args.dry_run)
        .map_err(|error| error.to_string())?;
    remove_selected_assets(&selection, installer.paths(), args.dry_run)?;
    Ok(())
}

fn format_install_error(error: &zag_lens_installer::InstallError) -> String {
    if let zag_lens_installer::InstallError::Conflicts(conflicts) = error {
        let details = conflicts
            .iter()
            .map(|conflict| {
                format!(
                    "{:?} at {}: {}",
                    conflict.component,
                    conflict.path.display(),
                    conflict.message
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return format!("{error}: {details}");
    }
    error.to_string()
}

fn print_plan(plan: &zag_lens_installer::InstallPlan, dry_run: bool) {
    for change in plan.changes() {
        let operation = match change.operation {
            Operation::Write => "write",
            Operation::Remove => "remove",
        };
        let prefix = if dry_run { "would " } else { "" };
        println!(
            "{prefix}{operation} {} ({})",
            change.path.display(),
            change.description
        );
    }
    if plan.is_empty() {
        println!("configuration already in the requested state");
    }
}

struct AssetCopy {
    source: PathBuf,
    destination: PathBuf,
}

fn setup_assets(
    selection: &Selection,
    paths: &InstallPaths,
    plugin_wasm: Option<&Path>,
) -> Result<Vec<AssetCopy>, String> {
    let mut assets = Vec::with_capacity(2);
    // The plugin invokes this executable for host notifications, so even a
    // Zellij-only setup needs the native asset.
    let current = std::env::current_exe()
        .map_err(|error| format!("could not resolve current executable: {error}"))?;
    require_regular_file(&current, "current executable")?;
    assets.push(AssetCopy {
        source: current,
        destination: paths.binary.clone(),
    });
    if selection.contains(Component::Zellij) {
        let source = plugin_wasm.ok_or_else(|| {
            "--plugin-wasm is required when the Zellij component is selected".to_owned()
        })?;
        require_regular_file(source, "plugin WASM")?;
        assets.push(AssetCopy {
            source: source.to_path_buf(),
            destination: paths.plugin.clone(),
        });
    }
    Ok(assets)
}

fn require_regular_file(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("could not read {label} {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{label} is not a regular file: {}", path.display()));
    }
    Ok(())
}

fn atomic_copy(source: &Path, destination: &Path) -> Result<bool, String> {
    if destination
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "refusing to replace symbolic link {}",
            destination.display()
        ));
    }
    if files_equal(source, destination)? {
        return Ok(false);
    }
    let parent = destination
        .parent()
        .ok_or_else(|| format!("installation path has no parent: {}", destination.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    let temporary = parent.join(format!(
        ".zag-lens-install-{}-{}",
        std::process::id(),
        OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    let result = copy_to_temporary(source, &temporary)
        .and_then(|()| fs::rename(&temporary, destination).map_err(|error| error.to_string()));
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "could not install {}: {error}",
            destination.display()
        ));
    }
    Ok(true)
}

fn copy_to_temporary(source: &Path, temporary: &Path) -> Result<(), String> {
    let mut input = File::open(source).map_err(|error| error.to_string())?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(temporary)
        .map_err(|error| error.to_string())?;
    io::copy(&mut input, &mut output).map_err(|error| error.to_string())?;
    output.sync_all().map_err(|error| error.to_string())?;
    let permissions = input
        .metadata()
        .map_err(|error| error.to_string())?
        .permissions();
    fs::set_permissions(temporary, permissions).map_err(|error| error.to_string())
}

fn files_equal(left: &Path, right: &Path) -> Result<bool, String> {
    let Ok(right_metadata) = fs::metadata(right) else {
        return Ok(false);
    };
    if !right_metadata.is_file() {
        return Ok(false);
    }
    let left_metadata = fs::metadata(left).map_err(|error| error.to_string())?;
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }
    let mut left_bytes = Vec::new();
    let mut right_bytes = Vec::new();
    File::open(left)
        .and_then(|mut file| file.read_to_end(&mut left_bytes))
        .map_err(|error| error.to_string())?;
    File::open(right)
        .and_then(|mut file| file.read_to_end(&mut right_bytes))
        .map_err(|error| error.to_string())?;
    Ok(left_bytes == right_bytes)
}

fn remove_selected_assets(
    selection: &Selection,
    paths: &InstallPaths,
    dry_run: bool,
) -> Result<(), String> {
    if selection.contains(Component::Zellij) {
        remove_asset(&paths.plugin, dry_run, false)?;
    }
    // Keep the shared host executable while any selected integration remains.
    if selection.contains(Component::Zellij)
        && selection.contains(Component::Codex)
        && selection.contains(Component::Claude)
    {
        remove_asset(&paths.binary, dry_run, true)?;
    }
    Ok(())
}

fn remove_asset(path: &Path, dry_run: bool, verify_current_binary: bool) -> Result<(), String> {
    let Ok(metadata) = path.symlink_metadata() else {
        return Ok(());
    };
    if !metadata.file_type().is_file() {
        return Err(format!(
            "refusing to remove non-regular asset {}",
            path.display()
        ));
    }
    if verify_current_binary {
        let current = std::env::current_exe()
            .map_err(|error| format!("could not resolve current executable: {error}"))?;
        if !files_equal(&current, path)? {
            println!(
                "kept {} because it differs from the running executable",
                path.display()
            );
            return Ok(());
        }
    }
    if dry_run {
        println!("would remove {}", path.display());
    } else {
        fs::remove_file(path)
            .map_err(|error| format!("could not remove {}: {error}", path.display()))?;
        println!("removed {}", path.display());
    }
    Ok(())
}

fn plan_context() -> Result<PlanContext, String> {
    let now = OffsetDateTime::now_utc();
    let timestamp = now
        .format(&Rfc3339)
        .map_err(|error| format!("could not format installation timestamp: {error}"))?;
    let backup_label = format!("t{}", now.unix_timestamp_nanos());
    PlanContext::new(timestamp, backup_label).map_err(|error| error.to_string())
}

fn run_doctor() -> Result<(), String> {
    let mut healthy = true;
    for (program, baseline) in [
        ("zellij", ZELLIJ_BASELINE),
        ("codex", CODEX_BASELINE),
        ("claude", CLAUDE_BASELINE),
    ] {
        match command_version(program) {
            Ok(version) => println!("{program}: {version} (baseline {baseline})"),
            Err(error) => {
                healthy = false;
                println!("{program}: unavailable ({error}; baseline {baseline})");
            }
        }
    }

    let paths = InstallPaths::from_current_environment().map_err(|error| error.to_string())?;
    for (label, path) in [
        ("binary", &paths.binary),
        ("plugin", &paths.plugin),
        ("zellij config", &paths.zellij_config),
        ("codex hooks", &paths.codex_hooks),
        ("claude settings", &paths.claude_settings),
        ("ownership manifest", &paths.manifest),
    ] {
        let state = if path.exists() {
            "present"
        } else {
            "not present"
        };
        println!("{label}: {} ({state})", path.display());
    }

    if healthy {
        Ok(())
    } else {
        Err("one or more required programs are unavailable".to_owned())
    }
}

fn command_version(program: &str) -> Result<String, String> {
    let output = Command::new(program)
        .arg("--version")
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!("exited with {}", output.status));
    }
    let raw = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr)
    } else {
        String::from_utf8_lossy(&output.stdout)
    };
    let version = sanitize_field(raw.lines().next().unwrap_or_default(), 256);
    if version.is_empty() {
        Err("returned no version".to_owned())
    } else {
        Ok(version)
    }
}

fn debug_from_environment() -> bool {
    std::env::var("ZAG_LENS_DEBUG").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_selection_defaults_to_all() {
        let selection = ComponentArgs::default().selection();
        assert!(selection.contains(Component::Zellij));
        assert!(selection.contains(Component::Codex));
        assert!(selection.contains(Component::Claude));
    }

    #[test]
    fn component_selection_honors_explicit_subset() {
        let selection = ComponentArgs {
            codex: true,
            ..ComponentArgs::default()
        }
        .selection();
        assert!(!selection.contains(Component::Zellij));
        assert!(selection.contains(Component::Codex));
        assert!(!selection.contains(Component::Claude));
    }

    #[test]
    fn command_backend_keeps_hyphen_prefixed_arguments() {
        let cli = Cli::try_parse_from([
            "zag-lens",
            "notify",
            "--backend",
            "command",
            "--title",
            "title",
            "--body",
            "body",
            "--command",
            "/usr/bin/notify",
            "--command-arg",
            "--urgency=normal",
        ])
        .expect("valid CLI");
        let CliCommand::Notify(args) = cli.command else {
            panic!("notify command expected");
        };
        assert_eq!(args.command_args, [OsString::from("--urgency=normal")]);
    }

    #[test]
    fn plan_context_is_valid_and_filename_safe() {
        let context = plan_context().expect("current time can be represented");
        assert!(!context.timestamp.is_empty());
        assert!(context.backup_label.starts_with('t'));
    }
}
