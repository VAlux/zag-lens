use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::PathBuf;

/// A separately selectable integration.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Component {
    Zellij,
    Codex,
    Claude,
}

/// Components affected by setup or uninstall.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selection {
    components: BTreeSet<Component>,
}

impl Selection {
    #[must_use]
    pub fn all() -> Self {
        Self::from_components([Component::Zellij, Component::Codex, Component::Claude])
    }

    #[must_use]
    pub fn from_components(components: impl IntoIterator<Item = Component>) -> Self {
        Self {
            components: components.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn contains(&self, component: Component) -> bool {
        self.components.contains(&component)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }
}

impl Default for Selection {
    fn default() -> Self {
        Self::all()
    }
}

/// Stable timestamps and backup labels supplied by the CLI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanContext {
    /// RFC 3339 timestamp persisted in the ownership manifest.
    pub timestamp: String,
    /// Filesystem-safe suffix used for backup files.
    pub backup_label: String,
}

impl PlanContext {
    /// Creates deterministic timestamp metadata for a plan.
    ///
    /// # Errors
    ///
    /// Returns [`InstallError::InvalidContext`] when either value is empty or
    /// the backup label contains characters unsafe for a filename.
    pub fn new(
        timestamp: impl Into<String>,
        backup_label: impl Into<String>,
    ) -> Result<Self, InstallError> {
        let timestamp = timestamp.into();
        let backup_label = backup_label.into();
        if timestamp.trim().is_empty() {
            return Err(InstallError::InvalidContext(
                "timestamp must not be empty".to_owned(),
            ));
        }
        if backup_label.is_empty()
            || !backup_label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(InstallError::InvalidContext(
                "backup label must contain only ASCII letters, digits, '.', '-' or '_'".to_owned(),
            ));
        }
        Ok(Self {
            timestamp,
            backup_label,
        })
    }
}

/// A detected ownership or configuration conflict.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    pub component: Component,
    pub path: PathBuf,
    pub message: String,
}

/// A user-visible post-setup instruction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notice {
    pub component: Component,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Write,
    Remove,
}

/// Public summary of one planned filesystem change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileChange {
    pub component: Option<Component>,
    pub path: PathBuf,
    pub operation: Operation,
    pub backup_path: Option<PathBuf>,
    pub description: String,
    pub(crate) original: Option<Vec<u8>>,
    pub(crate) replacement: Option<Vec<u8>>,
}

/// A complete, conflict-free setup or uninstall plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallPlan {
    pub(crate) changes: Vec<FileChange>,
    pub(crate) notices: Vec<Notice>,
}

impl InstallPlan {
    #[must_use]
    pub fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    #[must_use]
    pub fn notices(&self) -> &[Notice] {
        &self.notices
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Applies this exact plan, or only reports it when `dry_run` is true.
    ///
    /// # Errors
    ///
    /// Returns an error if a planned input changed or a filesystem operation
    /// cannot be completed. Successfully written files are rolled back when a
    /// later write fails.
    pub fn apply(&self, dry_run: bool) -> Result<ApplyReport, InstallError> {
        crate::engine::apply_plan(self, dry_run)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplyReport {
    pub dry_run: bool,
    pub changed_paths: Vec<PathBuf>,
    pub backups: Vec<PathBuf>,
    pub notices: Vec<Notice>,
}

#[derive(Debug)]
pub enum InstallError {
    MissingHome,
    InvalidContext(String),
    InvalidConfig {
        component: Component,
        path: PathBuf,
        message: String,
    },
    UnsupportedManifest {
        path: PathBuf,
        schema_version: u32,
    },
    Conflicts(Vec<Conflict>),
    ConcurrentModification(PathBuf),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Serialization(String),
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHome => write!(formatter, "HOME is not set"),
            Self::InvalidContext(message) => write!(formatter, "invalid plan context: {message}"),
            Self::InvalidConfig {
                component,
                path,
                message,
            } => write!(
                formatter,
                "invalid {component:?} configuration at {}: {message}",
                path.display()
            ),
            Self::UnsupportedManifest {
                path,
                schema_version,
            } => write!(
                formatter,
                "unsupported manifest schema {schema_version} at {}",
                path.display()
            ),
            Self::Conflicts(conflicts) => {
                write!(formatter, "{} installation conflict(s)", conflicts.len())
            }
            Self::ConcurrentModification(path) => write!(
                formatter,
                "{} changed after the installation plan was created",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(formatter, "I/O error at {}: {source}", path.display())
            }
            Self::Serialization(message) => write!(formatter, "serialization failed: {message}"),
        }
    }
}

impl std::error::Error for InstallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
