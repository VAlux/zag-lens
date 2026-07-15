use std::env;
use std::path::PathBuf;

use crate::model::InstallError;

/// Environment values used to resolve a user-level installation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathEnvironment {
    pub home: PathBuf,
    pub xdg_bin_home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub claude_config_dir: Option<PathBuf>,
}

impl PathEnvironment {
    /// Reads only path-related variables from the current process.
    ///
    /// # Errors
    ///
    /// Returns [`InstallError::MissingHome`] when `HOME` is unset or empty.
    pub fn from_current() -> Result<Self, InstallError> {
        let home = env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or(InstallError::MissingHome)?;

        Ok(Self {
            home,
            xdg_bin_home: optional_path("XDG_BIN_HOME"),
            xdg_config_home: optional_path("XDG_CONFIG_HOME"),
            xdg_data_home: optional_path("XDG_DATA_HOME"),
            codex_home: optional_path("CODEX_HOME"),
            claude_config_dir: optional_path("CLAUDE_CONFIG_DIR"),
        })
    }
}

fn optional_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// All paths read or referenced by user-level setup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallPaths {
    pub binary: PathBuf,
    pub plugin: PathBuf,
    pub zellij_config: PathBuf,
    pub codex_hooks: PathBuf,
    pub claude_settings: PathBuf,
    pub manifest: PathBuf,
}

impl InstallPaths {
    #[must_use]
    pub fn resolve(environment: &PathEnvironment) -> Self {
        let bin_home = environment
            .xdg_bin_home
            .clone()
            .unwrap_or_else(|| environment.home.join(".local/bin"));
        let config_home = environment
            .xdg_config_home
            .clone()
            .unwrap_or_else(|| environment.home.join(".config"));
        let data_home = environment
            .xdg_data_home
            .clone()
            .unwrap_or_else(|| environment.home.join(".local/share"));
        let data_dir = data_home.join("zag-lens");
        let codex_home = environment
            .codex_home
            .clone()
            .unwrap_or_else(|| environment.home.join(".codex"));
        let claude_home = environment
            .claude_config_dir
            .clone()
            .unwrap_or_else(|| environment.home.join(".claude"));

        Self {
            binary: bin_home.join("zag-lens"),
            plugin: data_dir.join("zag-lens.wasm"),
            zellij_config: config_home.join("zellij/config.kdl"),
            codex_hooks: codex_home.join("hooks.json"),
            claude_settings: claude_home.join("settings.json"),
            manifest: data_dir.join("install-manifest.json"),
        }
    }

    /// Resolves paths from the current process environment.
    ///
    /// # Errors
    ///
    /// Returns [`InstallError::MissingHome`] when `HOME` is unset or empty.
    pub fn from_current_environment() -> Result<Self, InstallError> {
        Ok(Self::resolve(&PathEnvironment::from_current()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn paths_use_local_fallbacks() {
        let environment = PathEnvironment {
            home: PathBuf::from("/home/tester"),
            xdg_bin_home: None,
            xdg_config_home: None,
            xdg_data_home: None,
            codex_home: None,
            claude_config_dir: None,
        };
        let paths = InstallPaths::resolve(&environment);
        assert_eq!(paths.binary, Path::new("/home/tester/.local/bin/zag-lens"));
        assert_eq!(
            paths.plugin,
            Path::new("/home/tester/.local/share/zag-lens/zag-lens.wasm")
        );
        assert_eq!(
            paths.zellij_config,
            Path::new("/home/tester/.config/zellij/config.kdl")
        );
        assert_eq!(
            paths.codex_hooks,
            Path::new("/home/tester/.codex/hooks.json")
        );
        assert_eq!(
            paths.claude_settings,
            Path::new("/home/tester/.claude/settings.json")
        );
    }

    #[test]
    fn explicit_environment_paths_take_precedence() {
        let environment = PathEnvironment {
            home: PathBuf::from("/home/tester"),
            xdg_bin_home: Some(PathBuf::from("/xdg/bin")),
            xdg_config_home: Some(PathBuf::from("/xdg/config")),
            xdg_data_home: Some(PathBuf::from("/xdg/data")),
            codex_home: Some(PathBuf::from("/agents/codex")),
            claude_config_dir: Some(PathBuf::from("/agents/claude")),
        };
        let paths = InstallPaths::resolve(&environment);
        assert_eq!(paths.binary, Path::new("/xdg/bin/zag-lens"));
        assert_eq!(paths.plugin, Path::new("/xdg/data/zag-lens/zag-lens.wasm"));
        assert_eq!(
            paths.zellij_config,
            Path::new("/xdg/config/zellij/config.kdl")
        );
        assert_eq!(paths.codex_hooks, Path::new("/agents/codex/hooks.json"));
        assert_eq!(
            paths.claude_settings,
            Path::new("/agents/claude/settings.json")
        );
    }
}
