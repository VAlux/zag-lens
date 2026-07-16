//! Pure attention-notification policy and request construction.
//!
//! The controller consumes only accepted reducer transitions and stable tab
//! metadata. It never observes native hook payloads, prompts, tool arguments,
//! command output, or transcripts.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt;
use std::str::FromStr;

use zag_lens_core::{Transition, TransitionCause};
use zag_lens_protocol::{AgentIdentity, CanonicalState, EventKind};

const DEFAULT_MAX_OUTSTANDING: usize = 1_024;
const MAX_TITLE_CHARS: usize = 128;
const MAX_BODY_CHARS: usize = 512;
const MAX_COMMAND_ARGS: usize = 64;
const MAX_COMMAND_ARG_CHARS: usize = 1_024;

/// Which accepted state transitions may produce notifications.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NotificationPolicy {
    /// Notify only when an agent requires user interaction.
    #[default]
    WaitingOnly,
    /// Also notify when a turn succeeds or fails.
    WaitingAndComplete,
    /// Keep title status only.
    Off,
}

impl NotificationPolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WaitingOnly => "waiting-only",
            Self::WaitingAndComplete => "waiting-and-complete",
            Self::Off => "off",
        }
    }
}

impl FromStr for NotificationPolicy {
    type Err = ParseConfigValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "waiting-only" => Ok(Self::WaitingOnly),
            "waiting-and-complete" => Ok(Self::WaitingAndComplete),
            "off" => Ok(Self::Off),
            _ => Err(ParseConfigValueError),
        }
    }
}

/// Whether active-tab state suppresses a notification.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NotificationFocus {
    /// Notify only when the affected Zellij tab is inactive.
    #[default]
    InactiveTab,
    /// Notify regardless of active-tab state.
    Always,
    /// Never invoke a notification backend.
    Never,
}

impl NotificationFocus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InactiveTab => "inactive-tab",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

impl FromStr for NotificationFocus {
    type Err = ParseConfigValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "inactive-tab" => Ok(Self::InactiveTab),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            _ => Err(ParseConfigValueError),
        }
    }
}

/// Host notification backend selected by plugin configuration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NotificationBackend {
    #[default]
    Auto,
    AppleScript,
    Command,
    Bell,
    Off,
}

impl NotificationBackend {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::AppleScript => "applescript",
            Self::Command => "command",
            Self::Bell => "bell",
            Self::Off => "off",
        }
    }
}

impl FromStr for NotificationBackend {
    type Err = ParseConfigValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "applescript" => Ok(Self::AppleScript),
            "command" => Ok(Self::Command),
            "bell" => Ok(Self::Bell),
            "off" => Ok(Self::Off),
            _ => Err(ParseConfigValueError),
        }
    }
}

/// Marker error for an invalid enumerated plugin configuration value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseConfigValueError;

impl fmt::Display for ParseConfigValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid notification configuration value")
    }
}

/// Policy settings owned by the attention controller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttentionConfig {
    pub policy: NotificationPolicy,
    pub focus: NotificationFocus,
    pub backend: NotificationBackend,
    /// Trusted executable used only by the `command` backend.
    pub command: Option<String>,
    /// Trusted argv prefix parsed from `notification_command_args` JSON.
    pub command_args: Vec<String>,
    /// Includes only the normalized, explicitly supplied attention summary.
    pub include_message_details: bool,
    pub max_outstanding_interactions: usize,
}

impl Default for AttentionConfig {
    fn default() -> Self {
        Self {
            policy: NotificationPolicy::WaitingOnly,
            focus: NotificationFocus::InactiveTab,
            backend: NotificationBackend::Auto,
            command: None,
            command_args: Vec::new(),
            include_message_details: false,
            max_outstanding_interactions: DEFAULT_MAX_OUTSTANDING,
        }
    }
}

impl AttentionConfig {
    /// Parses Zellij configuration with per-field safe defaults.
    ///
    /// Invalid key names are returned separately for sanitized diagnostics.
    #[must_use]
    pub fn parse_zellij(values: &BTreeMap<String, String>) -> ParsedAttentionConfig {
        let defaults = Self::default();
        let mut invalid_keys = Vec::new();

        let policy = parse_or_default(
            values,
            "notification_policy",
            defaults.policy,
            &mut invalid_keys,
        );
        let focus = parse_or_default(
            values,
            "notification_focus",
            defaults.focus,
            &mut invalid_keys,
        );
        let backend = parse_or_default(
            values,
            "notification_backend",
            defaults.backend,
            &mut invalid_keys,
        );
        let command = values.get("notification_command").and_then(|value| {
            let value = value.trim();
            if value.is_empty() || value.contains('\0') {
                invalid_keys.push("notification_command");
                None
            } else {
                Some(value.to_owned())
            }
        });
        let command_args = match values.get("notification_command_args") {
            Some(value) => serde_json::from_str::<Vec<String>>(value)
                .ok()
                .filter(|args| {
                    args.len() <= MAX_COMMAND_ARGS
                        && args.iter().all(|arg| {
                            arg.chars().count() <= MAX_COMMAND_ARG_CHARS && !arg.contains('\0')
                        })
                })
                .unwrap_or_else(|| {
                    invalid_keys.push("notification_command_args");
                    Vec::new()
                }),
            None => Vec::new(),
        };
        if backend == NotificationBackend::Command
            && command.is_none()
            && !invalid_keys.contains(&"notification_command")
        {
            invalid_keys.push("notification_command");
        }
        let include_message_details = match values.get("include_message_details") {
            Some(value) => {
                if let Ok(parsed) = value.parse::<bool>() {
                    parsed
                } else {
                    invalid_keys.push("include_message_details");
                    defaults.include_message_details
                }
            }
            None => defaults.include_message_details,
        };

        ParsedAttentionConfig {
            config: Self {
                policy,
                focus,
                backend,
                command,
                command_args,
                include_message_details,
                max_outstanding_interactions: defaults.max_outstanding_interactions,
            },
            invalid_keys,
        }
    }
}

fn parse_or_default<T>(
    values: &BTreeMap<String, String>,
    key: &'static str,
    default: T,
    invalid_keys: &mut Vec<&'static str>,
) -> T
where
    T: Copy + FromStr,
{
    match values.get(key) {
        Some(value) => {
            if let Ok(parsed) = value.parse() {
                parsed
            } else {
                invalid_keys.push(key);
                default
            }
        }
        None => default,
    }
}

/// Parsed configuration plus names of fields that used safe defaults.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedAttentionConfig {
    pub config: AttentionConfig,
    pub invalid_keys: Vec<&'static str>,
}

/// Stable tab metadata supplied by the Zellij runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotificationTab<'a> {
    pub name: &'a str,
    pub active: bool,
}

/// A sanitized asynchronous host command for one notification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostNotificationRequest {
    pub program: String,
    pub args: Vec<String>,
    pub title: String,
    pub body: String,
}

impl HostNotificationRequest {
    fn new(config: &AttentionConfig, title: String, body: String) -> Self {
        let mut args = vec![
            "notify".to_owned(),
            "--backend".to_owned(),
            config.backend.as_str().to_owned(),
        ];
        if config.backend == NotificationBackend::Command
            && let Some(command) = &config.command
        {
            args.push("--command".to_owned());
            args.push(command.clone());
            for argument in &config.command_args {
                args.push("--command-arg".to_owned());
                args.push(argument.clone());
            }
        }
        args.extend([
            "--title".to_owned(),
            title.clone(),
            "--body".to_owned(),
            body.clone(),
        ]);
        Self {
            program: "zag-lens".to_owned(),
            args,
            title,
            body,
        }
    }
}

/// Why an accepted transition did not produce a host command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SuppressionReason {
    NotNotifiable,
    PolicyOff,
    BackendOff,
    FocusNever,
    ActiveTab,
    RunCommandsDenied,
    DuplicateInteraction,
}

/// Pure result for runtime dispatch and sanitized diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationDecision {
    Emit(HostNotificationRequest),
    Suppressed(SuppressionReason),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct InteractionKey {
    identity: AgentIdentity,
    turn_id: Option<String>,
    attention_kind: String,
}

/// Bounded policy and outstanding-interaction state.
#[derive(Debug)]
pub struct AttentionController {
    config: AttentionConfig,
    outstanding: HashSet<InteractionKey>,
    order: VecDeque<InteractionKey>,
}

impl AttentionController {
    #[must_use]
    pub fn new(mut config: AttentionConfig) -> Self {
        config.max_outstanding_interactions = config.max_outstanding_interactions.max(1);
        Self {
            config,
            outstanding: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    #[must_use]
    pub fn config(&self) -> &AttentionConfig {
        &self.config
    }

    #[must_use]
    pub fn outstanding_len(&self) -> usize {
        self.outstanding.len()
    }

    /// Evaluates one accepted reducer transition without invoking host APIs.
    ///
    /// `run_commands_granted` is an explicit capability input. A denial is
    /// contained as a suppression decision and never affects reducer or title
    /// state.
    #[must_use]
    pub fn evaluate(
        &mut self,
        transition: &Transition,
        tab: NotificationTab<'_>,
        run_commands_granted: bool,
    ) -> NotificationDecision {
        self.clear_if_resumed_or_terminal(transition);

        let Some(current) = transition.current.as_ref() else {
            return NotificationDecision::Suppressed(SuppressionReason::NotNotifiable);
        };

        let notification = match (transition.cause, current.state) {
            (
                TransitionCause::Event(EventKind::InteractionRequired),
                CanonicalState::WaitingForUser,
            ) => {
                let attention_kind = current
                    .attention
                    .as_ref()
                    .map(|attention| sanitize_field(&attention.kind, 48))
                    .filter(|kind| !kind.is_empty())
                    .unwrap_or_else(|| "interaction".to_owned());
                let key = InteractionKey {
                    identity: current.identity.clone(),
                    turn_id: current.turn_id.clone(),
                    attention_kind: attention_kind.clone(),
                };
                if !self.remember_interaction(key) {
                    return NotificationDecision::Suppressed(
                        SuppressionReason::DuplicateInteraction,
                    );
                }
                NotificationKind::Waiting { attention_kind }
            }
            (TransitionCause::Event(EventKind::TurnCompleted), CanonicalState::Succeeded)
            | (TransitionCause::Event(EventKind::TurnFailed), CanonicalState::Failed) => {
                if !transition.state_changed() {
                    return NotificationDecision::Suppressed(SuppressionReason::NotNotifiable);
                }
                NotificationKind::Completion(current.state)
            }
            _ => {
                return NotificationDecision::Suppressed(SuppressionReason::NotNotifiable);
            }
        };

        if self.config.policy == NotificationPolicy::Off {
            return NotificationDecision::Suppressed(SuppressionReason::PolicyOff);
        }
        if self.config.backend == NotificationBackend::Off {
            return NotificationDecision::Suppressed(SuppressionReason::BackendOff);
        }
        if self.config.focus == NotificationFocus::Never {
            return NotificationDecision::Suppressed(SuppressionReason::FocusNever);
        }
        if self.config.focus == NotificationFocus::InactiveTab && tab.active {
            return NotificationDecision::Suppressed(SuppressionReason::ActiveTab);
        }
        if !run_commands_granted {
            return NotificationDecision::Suppressed(SuppressionReason::RunCommandsDenied);
        }
        if matches!(notification, NotificationKind::Completion(_))
            && self.config.policy != NotificationPolicy::WaitingAndComplete
        {
            return NotificationDecision::Suppressed(SuppressionReason::NotNotifiable);
        }

        let (title, body) = self.render(current, notification, tab.name);
        NotificationDecision::Emit(HostNotificationRequest::new(&self.config, title, body))
    }

    fn render(
        &self,
        current: &zag_lens_core::AgentSnapshot,
        kind: NotificationKind,
        tab_name: &str,
    ) -> (String, String) {
        let harness = harness_display_name(&current.identity.harness);
        let tab = non_empty_or(sanitize_field(tab_name, 128), "unnamed tab");
        let is_waiting = matches!(&kind, NotificationKind::Waiting { .. });

        let (title, mut body) = match kind {
            NotificationKind::Waiting { attention_kind } => (
                format!("{harness} needs attention"),
                format!("{tab} · {attention_kind}"),
            ),
            NotificationKind::Completion(CanonicalState::Succeeded) => (
                format!("{harness} completed"),
                format!("{tab} · turn succeeded"),
            ),
            NotificationKind::Completion(CanonicalState::Failed) => {
                (format!("{harness} failed"), format!("{tab} · turn failed"))
            }
            NotificationKind::Completion(_) => unreachable!("completion states are filtered"),
        };

        if self.config.include_message_details
            && is_waiting
            && let Some(summary) = current
                .attention
                .as_ref()
                .and_then(|value| value.summary.as_ref())
        {
            let summary = sanitize_field(summary, 256);
            if !summary.is_empty() {
                body.push_str(" — ");
                body.push_str(&summary);
            }
        }

        (
            sanitize_field(&title, MAX_TITLE_CHARS),
            sanitize_field(&body, MAX_BODY_CHARS),
        )
    }

    fn remember_interaction(&mut self, key: InteractionKey) -> bool {
        if !self.outstanding.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > self.config.max_outstanding_interactions {
            if let Some(expired) = self.order.pop_front() {
                self.outstanding.remove(&expired);
            }
        }
        true
    }

    fn clear_if_resumed_or_terminal(&mut self, transition: &Transition) {
        let identity = transition
            .current
            .as_ref()
            .map(|snapshot| &snapshot.identity)
            .or_else(|| {
                transition
                    .previous
                    .as_ref()
                    .map(|snapshot| &snapshot.identity)
            });
        let Some(identity) = identity else {
            return;
        };
        let should_clear = transition.current.as_ref().is_none_or(|snapshot| {
            matches!(
                snapshot.state,
                CanonicalState::Ready
                    | CanonicalState::Working
                    | CanonicalState::Succeeded
                    | CanonicalState::Failed
                    | CanonicalState::Stale
                    | CanonicalState::Stopped
            )
        });
        if should_clear {
            self.outstanding.retain(|key| &key.identity != identity);
            self.order.retain(|key| &key.identity != identity);
        }
    }
}

impl Default for AttentionController {
    fn default() -> Self {
        Self::new(AttentionConfig::default())
    }
}

#[derive(Clone, Debug)]
enum NotificationKind {
    Waiting { attention_kind: String },
    Completion(CanonicalState),
}

fn harness_display_name(value: &str) -> String {
    match value {
        "codex" => "Codex".to_owned(),
        "claude" | "claude-code" | "claude_code" => "Claude Code".to_owned(),
        "opencode" | "open-code" | "open_code" => "OpenCode".to_owned(),
        _ => non_empty_or(sanitize_field(value, 48), "Agent"),
    }
}

fn non_empty_or(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

fn sanitize_field(value: &str, max_chars: usize) -> String {
    let mut output = String::with_capacity(value.len().min(max_chars));
    let mut characters = value.chars().peekable();
    let mut pending_space = false;

    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            consume_escape(&mut characters);
        } else if character == '\u{009b}' {
            consume_csi(&mut characters);
        } else if character.is_whitespace() {
            pending_space = !output.is_empty();
        } else if !character.is_control() {
            if pending_space && output.chars().count() < max_chars {
                output.push(' ');
            }
            pending_space = false;
            if output.chars().count() < max_chars {
                output.push(character);
            }
        }
    }
    output
}

fn consume_escape<I>(characters: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    match characters.next() {
        Some('[') => consume_csi(characters),
        Some(']' | 'P' | 'X' | '^' | '_') => consume_control_string(characters),
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
    let mut previous_was_escape = false;
    for character in characters.by_ref() {
        if character == '\u{7}' || (previous_was_escape && character == '\\') {
            break;
        }
        previous_was_escape = character == '\u{1b}';
    }
}

#[cfg(test)]
mod tests {
    use zag_lens_core::{AgentSnapshot, Transition, TransitionCause};
    use zag_lens_protocol::{AgentIdentity, Attention, CanonicalState, EventKind};

    use super::*;

    fn snapshot(
        state: CanonicalState,
        turn_id: Option<&str>,
        attention_kind: Option<&str>,
        summary: Option<&str>,
    ) -> AgentSnapshot {
        AgentSnapshot {
            identity: AgentIdentity {
                harness: "claude".to_owned(),
                session_id: "session-1".to_owned(),
                agent_instance_id: "session-1".to_owned(),
            },
            pane_id: "terminal_1".to_owned(),
            state,
            turn_id: turn_id.map(str::to_owned),
            attention: attention_kind.map(|kind| Attention {
                kind: kind.to_owned(),
                summary: summary.map(str::to_owned),
            }),
            occurred_at_unix_nanos: 0,
        }
    }

    fn transition(
        previous: Option<AgentSnapshot>,
        current: Option<AgentSnapshot>,
        kind: EventKind,
    ) -> Transition {
        Transition {
            previous,
            current,
            cause: TransitionCause::Event(kind),
        }
    }

    fn waiting(previous: Option<AgentSnapshot>) -> Transition {
        transition(
            previous,
            Some(snapshot(
                CanonicalState::WaitingForUser,
                Some("turn-1"),
                Some("permission"),
                Some("Permission required"),
            )),
            EventKind::InteractionRequired,
        )
    }

    fn inactive_tab() -> NotificationTab<'static> {
        NotificationTab {
            name: "review",
            active: false,
        }
    }

    fn emitted(decision: NotificationDecision) -> HostNotificationRequest {
        match decision {
            NotificationDecision::Emit(request) => request,
            other @ NotificationDecision::Suppressed(_) => {
                panic!("expected notification request, got {other:?}")
            }
        }
    }

    #[test]
    fn defaults_match_specification() {
        let config = AttentionConfig::default();
        assert_eq!(config.policy, NotificationPolicy::WaitingOnly);
        assert_eq!(config.focus, NotificationFocus::InactiveTab);
        assert_eq!(config.backend, NotificationBackend::Auto);
        assert_eq!(config.command, None);
        assert!(config.command_args.is_empty());
        assert!(!config.include_message_details);
    }

    #[test]
    fn command_backend_uses_trusted_json_argv_without_a_shell() {
        let values = BTreeMap::from([
            ("notification_backend".to_owned(), "command".to_owned()),
            (
                "notification_command".to_owned(),
                "/usr/local/bin/notify-wrapper".to_owned(),
            ),
            (
                "notification_command_args".to_owned(),
                r#"["--urgency=normal","literal value"]"#.to_owned(),
            ),
        ]);
        let parsed = AttentionConfig::parse_zellij(&values);
        assert!(parsed.invalid_keys.is_empty());
        let mut controller = AttentionController::new(parsed.config);

        let request = emitted(controller.evaluate(&waiting(None), inactive_tab(), true));
        assert_eq!(
            request.args,
            [
                "notify",
                "--backend",
                "command",
                "--command",
                "/usr/local/bin/notify-wrapper",
                "--command-arg",
                "--urgency=normal",
                "--command-arg",
                "literal value",
                "--title",
                "Claude Code needs attention",
                "--body",
                "review · permission",
            ]
        );
    }

    #[test]
    fn applescript_backend_needs_no_command_configuration() {
        let values =
            BTreeMap::from([("notification_backend".to_owned(), "applescript".to_owned())]);
        let parsed = AttentionConfig::parse_zellij(&values);
        assert!(parsed.invalid_keys.is_empty());
        let mut controller = AttentionController::new(parsed.config);

        let request = emitted(controller.evaluate(&waiting(None), inactive_tab(), true));
        assert_eq!(
            request.args,
            [
                "notify",
                "--backend",
                "applescript",
                "--title",
                "Claude Code needs attention",
                "--body",
                "review · permission",
            ]
        );
    }

    #[test]
    fn invalid_command_configuration_falls_back_without_affecting_titles() {
        let values = BTreeMap::from([
            ("notification_backend".to_owned(), "command".to_owned()),
            (
                "notification_command_args".to_owned(),
                "not-json".to_owned(),
            ),
        ]);
        let parsed = AttentionConfig::parse_zellij(&values);
        assert_eq!(
            parsed.invalid_keys,
            ["notification_command_args", "notification_command"]
        );
        assert_eq!(parsed.config.backend, NotificationBackend::Command);
        assert_eq!(parsed.config.command, None);
        assert!(parsed.config.command_args.is_empty());
    }

    #[test]
    fn parses_values_and_reports_invalid_keys_with_safe_defaults() {
        let values = BTreeMap::from([
            (
                "notification_policy".to_owned(),
                "waiting-and-complete".to_owned(),
            ),
            ("notification_focus".to_owned(), "invalid".to_owned()),
            ("notification_backend".to_owned(), "bell".to_owned()),
            ("include_message_details".to_owned(), "perhaps".to_owned()),
        ]);
        let parsed = AttentionConfig::parse_zellij(&values);

        assert_eq!(parsed.config.policy, NotificationPolicy::WaitingAndComplete);
        assert_eq!(parsed.config.focus, NotificationFocus::InactiveTab);
        assert_eq!(parsed.config.backend, NotificationBackend::Bell);
        assert!(!parsed.config.include_message_details);
        assert_eq!(
            parsed.invalid_keys,
            vec!["notification_focus", "include_message_details"]
        );
    }

    #[test]
    fn permission_and_related_notification_emit_only_once() {
        let mut controller = AttentionController::default();
        let permission = waiting(None);
        let native_notification = waiting(permission.current.clone());

        emitted(controller.evaluate(&permission, inactive_tab(), true));
        assert_eq!(
            controller.evaluate(&native_notification, inactive_tab(), true),
            NotificationDecision::Suppressed(SuppressionReason::DuplicateInteraction)
        );
        assert_eq!(controller.outstanding_len(), 1);
    }

    #[test]
    fn active_tab_suppression_still_deduplicates_interaction() {
        let mut controller = AttentionController::default();
        let event = waiting(None);
        assert_eq!(
            controller.evaluate(
                &event,
                NotificationTab {
                    name: "active",
                    active: true,
                },
                true,
            ),
            NotificationDecision::Suppressed(SuppressionReason::ActiveTab)
        );
        assert_eq!(
            controller.evaluate(&event, inactive_tab(), true),
            NotificationDecision::Suppressed(SuppressionReason::DuplicateInteraction)
        );
    }

    #[test]
    fn focus_always_notifies_active_tab_and_never_suppresses_every_tab() {
        let event = waiting(None);
        let mut always = AttentionController::new(AttentionConfig {
            focus: NotificationFocus::Always,
            ..AttentionConfig::default()
        });
        assert!(matches!(
            always.evaluate(
                &event,
                NotificationTab {
                    name: "active",
                    active: true
                },
                true
            ),
            NotificationDecision::Emit(_)
        ));

        let mut never = AttentionController::new(AttentionConfig {
            focus: NotificationFocus::Never,
            ..AttentionConfig::default()
        });
        assert_eq!(
            never.evaluate(&event, inactive_tab(), true),
            NotificationDecision::Suppressed(SuppressionReason::FocusNever)
        );
    }

    #[test]
    fn denied_run_commands_is_a_contained_decision() {
        let mut controller = AttentionController::default();
        assert_eq!(
            controller.evaluate(&waiting(None), inactive_tab(), false),
            NotificationDecision::Suppressed(SuppressionReason::RunCommandsDenied)
        );
        assert_eq!(controller.outstanding_len(), 1);
    }

    #[test]
    fn waiting_only_suppresses_completion() {
        let complete = transition(
            Some(snapshot(
                CanonicalState::Working,
                Some("turn-1"),
                None,
                None,
            )),
            Some(snapshot(
                CanonicalState::Succeeded,
                Some("turn-1"),
                None,
                None,
            )),
            EventKind::TurnCompleted,
        );
        assert_eq!(
            AttentionController::default().evaluate(&complete, inactive_tab(), true),
            NotificationDecision::Suppressed(SuppressionReason::NotNotifiable)
        );
    }

    #[test]
    fn completion_policy_emits_for_success_and_failure() {
        let config = AttentionConfig {
            policy: NotificationPolicy::WaitingAndComplete,
            ..AttentionConfig::default()
        };
        for (kind, state, expected_title) in [
            (
                EventKind::TurnCompleted,
                CanonicalState::Succeeded,
                "Claude Code completed",
            ),
            (
                EventKind::TurnFailed,
                CanonicalState::Failed,
                "Claude Code failed",
            ),
        ] {
            let mut controller = AttentionController::new(config.clone());
            let event = transition(
                Some(snapshot(
                    CanonicalState::Working,
                    Some("turn-1"),
                    None,
                    None,
                )),
                Some(snapshot(state, Some("turn-1"), None, None)),
                kind,
            );
            assert_eq!(
                emitted(controller.evaluate(&event, inactive_tab(), true)).title,
                expected_title
            );
        }
    }

    #[test]
    fn cancellation_clears_attention_without_a_completion_notification() {
        let config = AttentionConfig {
            policy: NotificationPolicy::WaitingAndComplete,
            ..AttentionConfig::default()
        };
        let cancelled = transition(
            Some(snapshot(
                CanonicalState::WaitingForUser,
                Some("turn-1"),
                Some("permission"),
                None,
            )),
            Some(snapshot(
                CanonicalState::Stopped,
                Some("turn-1"),
                None,
                None,
            )),
            EventKind::TurnCancelled,
        );
        let mut controller = AttentionController::new(config);
        let _ = controller.evaluate(&waiting(None), inactive_tab(), true);
        assert_eq!(controller.outstanding_len(), 1);
        assert_eq!(
            controller.evaluate(&cancelled, inactive_tab(), true),
            NotificationDecision::Suppressed(SuppressionReason::NotNotifiable)
        );
        assert_eq!(controller.outstanding_len(), 0);
    }

    #[test]
    fn opencode_has_a_user_facing_display_name() {
        assert_eq!(harness_display_name("opencode"), "OpenCode");
    }

    #[test]
    fn off_policy_backend_and_focus_have_explicit_suppression() {
        for (config, expected) in [
            (
                AttentionConfig {
                    policy: NotificationPolicy::Off,
                    ..AttentionConfig::default()
                },
                SuppressionReason::PolicyOff,
            ),
            (
                AttentionConfig {
                    backend: NotificationBackend::Off,
                    ..AttentionConfig::default()
                },
                SuppressionReason::BackendOff,
            ),
            (
                AttentionConfig {
                    focus: NotificationFocus::Never,
                    ..AttentionConfig::default()
                },
                SuppressionReason::FocusNever,
            ),
        ] {
            assert_eq!(
                AttentionController::new(config).evaluate(&waiting(None), inactive_tab(), true),
                NotificationDecision::Suppressed(expected)
            );
        }
    }

    #[test]
    fn resumed_and_terminal_states_clear_outstanding_interactions() {
        let mut controller = AttentionController::default();
        let first = waiting(None);
        emitted(controller.evaluate(&first, inactive_tab(), true));

        let resumed = transition(
            first.current.clone(),
            Some(snapshot(
                CanonicalState::Working,
                Some("turn-1"),
                None,
                None,
            )),
            EventKind::Activity,
        );
        assert_eq!(
            controller.evaluate(&resumed, inactive_tab(), true),
            NotificationDecision::Suppressed(SuppressionReason::NotNotifiable)
        );
        assert_eq!(controller.outstanding_len(), 0);
        emitted(controller.evaluate(&waiting(resumed.current), inactive_tab(), true));

        let ended = transition(
            Some(snapshot(
                CanonicalState::WaitingForUser,
                Some("turn-1"),
                Some("permission"),
                None,
            )),
            None,
            EventKind::SessionEnded,
        );
        let _ = controller.evaluate(&ended, inactive_tab(), true);
        assert_eq!(controller.outstanding_len(), 0);
    }

    #[test]
    fn different_turn_or_interaction_kind_is_not_a_duplicate() {
        let mut controller = AttentionController::default();
        emitted(controller.evaluate(&waiting(None), inactive_tab(), true));

        let different_kind = transition(
            None,
            Some(snapshot(
                CanonicalState::WaitingForUser,
                Some("turn-1"),
                Some("elicitation"),
                None,
            )),
            EventKind::InteractionRequired,
        );
        emitted(controller.evaluate(&different_kind, inactive_tab(), true));

        let different_turn = transition(
            None,
            Some(snapshot(
                CanonicalState::WaitingForUser,
                Some("turn-2"),
                Some("permission"),
                None,
            )),
            EventKind::InteractionRequired,
        );
        emitted(controller.evaluate(&different_turn, inactive_tab(), true));
    }

    #[test]
    fn outstanding_interactions_are_bounded() {
        let mut controller = AttentionController::new(AttentionConfig {
            max_outstanding_interactions: 2,
            ..AttentionConfig::default()
        });
        for turn in ["turn-1", "turn-2", "turn-3"] {
            let event = transition(
                None,
                Some(snapshot(
                    CanonicalState::WaitingForUser,
                    Some(turn),
                    Some("permission"),
                    None,
                )),
                EventKind::InteractionRequired,
            );
            emitted(controller.evaluate(&event, inactive_tab(), true));
        }
        assert_eq!(controller.outstanding_len(), 2);
    }

    #[test]
    fn request_contains_only_sanitized_coarse_metadata_by_default() {
        let event = transition(
            None,
            Some(snapshot(
                CanonicalState::WaitingForUser,
                Some("turn-1"),
                Some("permission\u{1b}[31m"),
                Some("SECRET prompt text\nwith tool arguments"),
            )),
            EventKind::InteractionRequired,
        );
        let request = emitted(AttentionController::default().evaluate(
            &event,
            NotificationTab {
                name: "review\u{1b}]0;stolen\u{7}",
                active: false,
            },
            true,
        ));

        assert_eq!(request.title, "Claude Code needs attention");
        assert_eq!(request.body, "review · permission");
        assert!(!request.args.join(" ").contains("SECRET"));
        assert!(!request.args.join(" ").contains('\u{1b}'));
        assert_eq!(request.program, "zag-lens");
    }

    #[test]
    fn opted_in_normalized_summary_is_sanitized_and_bounded() {
        let mut controller = AttentionController::new(AttentionConfig {
            include_message_details: true,
            ..AttentionConfig::default()
        });
        let summary = format!("safe\u{1b}[31m summary {}", "x".repeat(600));
        let event = transition(
            None,
            Some(snapshot(
                CanonicalState::WaitingForUser,
                None,
                Some("question"),
                Some(&summary),
            )),
            EventKind::InteractionRequired,
        );
        let request = emitted(controller.evaluate(&event, inactive_tab(), true));
        assert!(request.body.starts_with("review · question — safe summary"));
        assert!(request.body.chars().count() <= MAX_BODY_CHARS);
        assert!(!request.body.contains('\u{1b}'));
    }
}
