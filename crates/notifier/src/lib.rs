//! Host notification backends isolated from state reduction.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Maximum number of Unicode scalar values retained in a notification title.
pub const MAX_TITLE_CHARS: usize = 128;

/// Maximum number of Unicode scalar values retained in a notification body.
pub const MAX_BODY_CHARS: usize = 512;

const APP_NAME: &str = "Zag Lens";

/// Notification text after privacy-preserving terminal sanitization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notification {
    title: String,
    body: String,
}

impl Notification {
    /// Sanitizes and bounds notification text before a backend can observe it.
    #[must_use]
    pub fn new(title: impl AsRef<str>, body: impl AsRef<str>) -> Self {
        Self {
            title: sanitize_field(title.as_ref(), MAX_TITLE_CHARS),
            body: sanitize_field(body.as_ref(), MAX_BODY_CHARS),
        }
    }

    /// Returns the sanitized title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the sanitized body.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// Result of submitting work to a notification backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    /// The backend accepted the notification.
    Submitted,
    /// Notifications are intentionally disabled.
    Disabled,
}

/// A backend failure captured without affecting agent state processing.
#[derive(Debug)]
pub enum NotificationError {
    /// A command backend was configured without an executable.
    EmptyCommand,
    /// The current platform has no automatic desktop backend.
    UnsupportedPlatform,
    /// The native desktop service rejected or could not show a notification.
    Desktop(String),
    /// The configured notification command could not be spawned.
    Spawn {
        /// The trusted, user-configured executable.
        program: PathBuf,
        /// The operating-system error returned by `spawn`.
        source: io::Error,
    },
}

impl fmt::Display for NotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCommand => formatter.write_str("notification command is empty"),
            Self::UnsupportedPlatform => {
                formatter.write_str("automatic notifications are unsupported on this platform")
            }
            Self::Desktop(message) => write!(formatter, "desktop notification failed: {message}"),
            Self::Spawn { program, source } => {
                write!(
                    formatter,
                    "failed to spawn notification command {}: {source}",
                    program.display()
                )
            }
        }
    }
}

impl Error for NotificationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Spawn { source, .. } => Some(source),
            Self::EmptyCommand | Self::UnsupportedPlatform | Self::Desktop(_) => None,
        }
    }
}

/// Pluggable host notification behavior.
pub trait Notifier: Send + Sync {
    /// Submits a sanitized notification without mutating agent state.
    ///
    /// # Errors
    ///
    /// Returns a backend-specific delivery error. Callers that must contain
    /// errors at this boundary can use [`deliver`].
    fn notify(&self, notification: &Notification) -> Result<Delivery, NotificationError>;
}

/// A delivery result that keeps backend errors at the notification boundary.
#[derive(Debug)]
pub enum DeliveryStatus {
    /// The backend accepted the notification.
    Submitted,
    /// Notifications were configured off.
    Disabled,
    /// Delivery failed and may be logged as a sanitized diagnostic.
    Failed(NotificationError),
}

/// Delivers a notification while containing backend failures.
#[must_use]
pub fn deliver(notifier: &dyn Notifier, notification: &Notification) -> DeliveryStatus {
    match notifier.notify(notification) {
        Ok(Delivery::Submitted) => DeliveryStatus::Submitted,
        Ok(Delivery::Disabled) => DeliveryStatus::Disabled,
        Err(error) => DeliveryStatus::Failed(error),
    }
}

/// User-selectable notification backend configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendConfig {
    /// Select the native backend for the current operating system.
    Auto,
    /// Spawn a trusted executable with an argv prefix.
    Command(CommandConfig),
    /// Emit a best-effort terminal bell.
    Bell,
    /// Suppress notification delivery.
    Off,
}

impl BackendConfig {
    /// Constructs the configured backend.
    ///
    /// # Errors
    ///
    /// Returns [`NotificationError::EmptyCommand`] when a command backend has
    /// no executable.
    pub fn build(self) -> Result<Box<dyn Notifier>, NotificationError> {
        match self {
            Self::Auto => Ok(Box::new(AutoNotifier)),
            Self::Command(config) => Ok(Box::new(CommandNotifier::new(config)?)),
            Self::Bell => Ok(Box::new(BellNotifier)),
            Self::Off => Ok(Box::new(OffNotifier)),
        }
    }
}

/// Trusted executable and fixed arguments for command notifications.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandConfig {
    program: PathBuf,
    args: Vec<OsString>,
}

impl CommandConfig {
    /// Creates a command configuration. Validation happens when it is built.
    #[must_use]
    pub fn new<I, S>(program: impl Into<PathBuf>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// Returns the trusted executable.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Returns the trusted argv prefix.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }
}

/// Native macOS Notification Center or Linux freedesktop notifications.
#[derive(Clone, Copy, Debug, Default)]
pub struct AutoNotifier;

impl Notifier for AutoNotifier {
    fn notify(&self, notification: &Notification) -> Result<Delivery, NotificationError> {
        show_desktop_notification(notification)?;
        Ok(Delivery::Submitted)
    }
}

/// Spawns a trusted executable without a shell and appends title and body args.
#[derive(Clone, Debug)]
pub struct CommandNotifier {
    config: CommandConfig,
}

impl CommandNotifier {
    /// Validates and creates a command backend.
    ///
    /// # Errors
    ///
    /// Returns [`NotificationError::EmptyCommand`] when `config` has no
    /// executable.
    pub fn new(config: CommandConfig) -> Result<Self, NotificationError> {
        if config.program.as_os_str().is_empty() {
            return Err(NotificationError::EmptyCommand);
        }
        Ok(Self { config })
    }

    /// Returns the validated command configuration.
    #[must_use]
    pub fn config(&self) -> &CommandConfig {
        &self.config
    }

    fn command(&self, notification: &Notification) -> Command {
        let mut command = Command::new(&self.config.program);
        command
            .args(&self.config.args)
            .arg(notification.title())
            .arg(notification.body())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
}

impl Notifier for CommandNotifier {
    fn notify(&self, notification: &Notification) -> Result<Delivery, NotificationError> {
        self.command(notification)
            .spawn()
            .map_err(|source| NotificationError::Spawn {
                program: self.config.program.clone(),
                source,
            })?;
        Ok(Delivery::Submitted)
    }
}

/// Best-effort terminal attention signal.
#[derive(Clone, Copy, Debug, Default)]
pub struct BellNotifier;

impl Notifier for BellNotifier {
    fn notify(&self, _notification: &Notification) -> Result<Delivery, NotificationError> {
        emit_terminal_bell();
        Ok(Delivery::Submitted)
    }
}

/// A backend that intentionally performs no work.
#[derive(Clone, Copy, Debug, Default)]
pub struct OffNotifier;

impl Notifier for OffNotifier {
    fn notify(&self, _notification: &Notification) -> Result<Delivery, NotificationError> {
        Ok(Delivery::Disabled)
    }
}

/// Removes terminal escape sequences and controls, normalizes whitespace, and
/// truncates by Unicode scalar values without splitting UTF-8.
#[must_use]
pub fn sanitize_field(value: &str, max_chars: usize) -> String {
    let without_escapes = strip_terminal_sequences(value);
    let mut normalized = String::with_capacity(without_escapes.len().min(max_chars));
    let mut needs_space = false;

    for character in without_escapes.chars() {
        if character.is_whitespace() {
            needs_space = !normalized.is_empty();
        } else if !character.is_control() {
            if needs_space {
                normalized.push(' ');
                needs_space = false;
            }
            normalized.push(character);
        }
    }

    let mut bounded: String = normalized.chars().take(max_chars).collect();
    bounded.truncate(bounded.trim_end().len());
    bounded
}

fn strip_terminal_sequences(value: &str) -> String {
    let mut characters = value.chars().peekable();
    let mut output = String::with_capacity(value.len());

    while let Some(character) = characters.next() {
        match character {
            '\u{1b}' => consume_escape_sequence(&mut characters),
            '\u{009b}' => consume_csi(&mut characters),
            '\u{0090}' | '\u{0098}' | '\u{009d}' | '\u{009e}' | '\u{009f}' => {
                consume_control_string(&mut characters);
            }
            _ => output.push(character),
        }
    }

    output
}

fn consume_escape_sequence<I>(characters: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    match characters.next() {
        Some('[') => consume_csi(characters),
        Some(']' | 'P' | 'X' | '^' | '_') => {
            consume_control_string(characters);
        }
        Some(character) if ('\u{20}'..='\u{2f}').contains(&character) => {
            for character in characters.by_ref() {
                if ('\u{30}'..='\u{7e}').contains(&character) {
                    break;
                }
            }
        }
        Some(_) | None => {}
    }
}

fn consume_csi<I>(characters: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    for character in characters.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&character) {
            break;
        }
    }
}

fn consume_control_string<I>(characters: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while let Some(character) = characters.next() {
        if matches!(character, '\u{07}' | '\u{009c}') {
            break;
        }
        if character == '\u{1b}' && characters.next_if_eq(&'\\').is_some() {
            break;
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn show_desktop_notification(notification: &Notification) -> Result<(), NotificationError> {
    notify_rust::Notification::new()
        .appname(APP_NAME)
        .summary(notification.title())
        .body(notification.body())
        .show()
        .map(|_| ())
        .map_err(|error| NotificationError::Desktop(error.to_string()))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn show_desktop_notification(_notification: &Notification) -> Result<(), NotificationError> {
    Err(NotificationError::UnsupportedPlatform)
}

fn emit_terminal_bell() {
    if let Ok(mut terminal) = OpenOptions::new().write(true).open("/dev/tty") {
        let _ = write_bell(&mut terminal);
    }
}

fn write_bell(writer: &mut impl Write) -> io::Result<()> {
    writer.write_all(b"\x07")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::sync::Mutex;

    struct Unavailable;

    impl Write for Unavailable {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "unavailable"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn notification_fields_are_sanitized_before_delivery() {
        let notification = Notification::new(
            " \x1b[31mCodex\x1b[0m\tneeds\ninput ",
            "open \x1b]8;;https://example.invalid\x07link\x1b]8;;\x1b\\\0 now",
        );

        assert_eq!(notification.title(), "Codex needs input");
        assert_eq!(notification.body(), "open link now");
    }

    #[test]
    fn sanitization_handles_c1_sequences_and_unicode_caps() {
        let input = "a\u{009b}31mβ🙂c\u{009b}0m";

        assert_eq!(sanitize_field(input, 3), "aβ🙂");
        assert_eq!(sanitize_field(input, 0), "");
    }

    #[test]
    fn command_backend_appends_fields_as_literal_arguments() {
        let backend = CommandNotifier::new(CommandConfig::new(
            "/usr/bin/notifier",
            ["--urgency", "critical"],
        ))
        .expect("valid command");
        let notification = Notification::new("$(touch /tmp/nope)", "hello; exit 1");
        let command = backend.command(&notification);
        let args: Vec<&OsStr> = command.get_args().collect();

        assert_eq!(command.get_program(), OsStr::new("/usr/bin/notifier"));
        assert_eq!(
            args,
            [
                OsStr::new("--urgency"),
                OsStr::new("critical"),
                OsStr::new("$(touch /tmp/nope)"),
                OsStr::new("hello; exit 1"),
            ]
        );
    }

    #[test]
    fn empty_command_is_rejected() {
        let result = CommandNotifier::new(CommandConfig::new("", [] as [&str; 0]));

        assert!(matches!(result, Err(NotificationError::EmptyCommand)));
    }

    #[test]
    fn command_spawn_failure_is_reported_without_panicking() {
        let backend = CommandNotifier::new(CommandConfig::new(
            "/path/that/does/not/exist/zag-lens-notifier",
            [] as [&str; 0],
        ))
        .expect("non-empty command");

        let status = deliver(&backend, &Notification::new("title", "body"));

        assert!(matches!(
            status,
            DeliveryStatus::Failed(NotificationError::Spawn { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn asynchronous_command_exit_does_not_escape_delivery_boundary() {
        let backend = CommandNotifier::new(CommandConfig::new("/usr/bin/false", [] as [&str; 0]))
            .expect("valid command");

        assert!(matches!(
            deliver(&backend, &Notification::new("title", "body")),
            DeliveryStatus::Submitted
        ));
    }

    #[test]
    fn fake_backend_observes_only_sanitized_fields() {
        #[derive(Default)]
        struct FakeNotifier {
            observed: Mutex<Option<Notification>>,
        }

        impl Notifier for FakeNotifier {
            fn notify(&self, notification: &Notification) -> Result<Delivery, NotificationError> {
                self.observed
                    .lock()
                    .expect("fake notifier lock")
                    .replace(notification.clone());
                Ok(Delivery::Submitted)
            }
        }

        let backend = FakeNotifier::default();
        let notification = Notification::new("\x1b[1mClaude\x1b[0m", "line\nready");

        assert!(matches!(
            deliver(&backend, &notification),
            DeliveryStatus::Submitted
        ));
        assert_eq!(
            backend.observed.into_inner().expect("fake notifier lock"),
            Some(Notification::new("Claude", "line ready"))
        );
    }

    #[test]
    fn bell_writes_one_attention_byte_and_ignores_unavailable_terminal() {
        let mut bytes = Vec::new();
        write_bell(&mut bytes).expect("in-memory bell");
        assert_eq!(bytes, b"\x07");

        assert!(write_bell(&mut Unavailable).is_err());
    }

    #[test]
    fn off_backend_reports_disabled() {
        assert!(matches!(
            deliver(&OffNotifier, &Notification::new("title", "body")),
            DeliveryStatus::Disabled
        ));
    }

    #[test]
    fn all_backend_variants_build_without_delivery() {
        let configurations = [
            BackendConfig::Auto,
            BackendConfig::Bell,
            BackendConfig::Off,
            BackendConfig::Command(CommandConfig::new("/usr/bin/notifier", ["--prefix"])),
        ];

        for configuration in configurations {
            assert!(configuration.build().is_ok());
        }
    }
}
