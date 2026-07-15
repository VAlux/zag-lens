use serde_json::Value;
use zag_lens_claude_adapter::ClaudeAdapter;
use zag_lens_protocol::{
    AdapterDecision, CanonicalState, EventContext, EventKind, HarnessAdapter, NativeHookEvent,
};

const SESSION_START: &str =
    include_str!("../../../tests/fixtures/claude/2.1.207/session-start.json");
const USER_PROMPT_SUBMIT: &str =
    include_str!("../../../tests/fixtures/claude/2.1.207/user-prompt-submit.json");
const PRE_TOOL_USE: &str = include_str!("../../../tests/fixtures/claude/2.1.207/pre-tool-use.json");
const POST_TOOL_USE: &str =
    include_str!("../../../tests/fixtures/claude/2.1.207/post-tool-use.json");
const POST_TOOL_USE_FAILURE: &str =
    include_str!("../../../tests/fixtures/claude/2.1.207/post-tool-use-failure.json");
const PERMISSION_REQUEST: &str =
    include_str!("../../../tests/fixtures/claude/2.1.207/permission-request.json");
const NOTIFICATION_PERMISSION: &str =
    include_str!("../../../tests/fixtures/claude/2.1.207/notification-permission.json");
const NOTIFICATION_IDLE: &str =
    include_str!("../../../tests/fixtures/claude/2.1.207/notification-idle.json");
const NOTIFICATION_ELICITATION: &str =
    include_str!("../../../tests/fixtures/claude/2.1.207/notification-elicitation.json");
const STOP: &str = include_str!("../../../tests/fixtures/claude/2.1.207/stop.json");
const STOP_FAILURE: &str = include_str!("../../../tests/fixtures/claude/2.1.207/stop-failure.json");
const SESSION_END: &str = include_str!("../../../tests/fixtures/claude/2.1.207/session-end.json");

fn context() -> EventContext {
    EventContext {
        event_id: "018f1324-7e9f-7bd2-a27e-0b1a489e7382".to_owned(),
        occurred_at: "2026-07-13T12:00:00Z".to_owned(),
        pane_id: "terminal_4".to_owned(),
        zellij_session: Some("work".to_owned()),
        cwd: Some("/workspace/project".to_owned()),
    }
}

fn native(native_event: &str, fixture: &str) -> NativeHookEvent {
    NativeHookEvent {
        native_event: native_event.to_owned(),
        payload: serde_json::from_str(fixture).expect("fixture must contain valid JSON"),
    }
}

fn emitted(native_event: &str, fixture: &str) -> Box<zag_lens_protocol::NormalizedEvent> {
    let decision = ClaudeAdapter
        .normalize(&native(native_event, fixture), &context())
        .expect("fixture must normalize");
    match decision {
        AdapterDecision::Emit(event) => event,
        AdapterDecision::Ignore => panic!("supported event must not be ignored"),
    }
}

#[test]
fn supported_version_and_adapter_identity_are_declared() {
    let adapter = ClaudeAdapter;

    assert_eq!(adapter.harness(), "claude");
    assert_eq!(adapter.adapter_info().name, "claude-code-hooks");
    assert_eq!(adapter.adapter_info().version, 1);
    assert_eq!(adapter.supported_versions().minimum, "2.1.207");
    assert_eq!(adapter.supported_versions().maximum, None);
}

#[test]
fn lifecycle_fixtures_map_to_expected_states() {
    let cases = [
        (
            "SessionStart",
            SESSION_START,
            EventKind::SessionStarted,
            CanonicalState::Ready,
        ),
        (
            "UserPromptSubmit",
            USER_PROMPT_SUBMIT,
            EventKind::TurnStarted,
            CanonicalState::Working,
        ),
        (
            "PreToolUse",
            PRE_TOOL_USE,
            EventKind::Activity,
            CanonicalState::Working,
        ),
        (
            "PostToolUse",
            POST_TOOL_USE,
            EventKind::Activity,
            CanonicalState::Working,
        ),
        (
            "PostToolUseFailure",
            POST_TOOL_USE_FAILURE,
            EventKind::Activity,
            CanonicalState::Working,
        ),
        (
            "PermissionRequest",
            PERMISSION_REQUEST,
            EventKind::InteractionRequired,
            CanonicalState::WaitingForUser,
        ),
        (
            "Notification",
            NOTIFICATION_PERMISSION,
            EventKind::InteractionRequired,
            CanonicalState::WaitingForUser,
        ),
        (
            "Notification",
            NOTIFICATION_IDLE,
            EventKind::InteractionRequired,
            CanonicalState::WaitingForUser,
        ),
        (
            "Notification",
            NOTIFICATION_ELICITATION,
            EventKind::InteractionRequired,
            CanonicalState::WaitingForUser,
        ),
        (
            "Stop",
            STOP,
            EventKind::TurnCompleted,
            CanonicalState::Succeeded,
        ),
        (
            "StopFailure",
            STOP_FAILURE,
            EventKind::TurnFailed,
            CanonicalState::Failed,
        ),
        (
            "SessionEnd",
            SESSION_END,
            EventKind::SessionEnded,
            CanonicalState::Stopped,
        ),
    ];

    for (native_event, fixture, expected_kind, expected_state) in cases {
        let event = emitted(native_event, fixture);
        assert_eq!(event.kind, expected_kind, "{native_event}");
        assert_eq!(event.state, expected_state, "{native_event}");
        assert_eq!(event.native_event, native_event);
        assert_eq!(event.harness, "claude");
    }
}

#[test]
fn interaction_types_are_coarse_and_stable() {
    let cases = [
        (
            "PermissionRequest",
            PERMISSION_REQUEST,
            "permission",
            "Claude Code requires permission",
        ),
        (
            "Notification",
            NOTIFICATION_PERMISSION,
            "permission",
            "Claude Code requires permission",
        ),
        (
            "Notification",
            NOTIFICATION_IDLE,
            "question",
            "Claude Code is waiting for input",
        ),
        (
            "Notification",
            NOTIFICATION_ELICITATION,
            "elicitation",
            "Claude Code requests input",
        ),
    ];

    for (native_event, fixture, expected_kind, expected_summary) in cases {
        let attention = emitted(native_event, fixture)
            .attention
            .expect("interaction requires attention");
        assert_eq!(attention.kind, expected_kind);
        assert_eq!(attention.summary.as_deref(), Some(expected_summary));
    }
}

#[test]
fn permission_sources_share_a_deduplication_classification() {
    let direct = emitted("PermissionRequest", PERMISSION_REQUEST)
        .attention
        .expect("permission requires attention");
    let notification = emitted("Notification", NOTIFICATION_PERMISSION)
        .attention
        .expect("permission requires attention");

    assert_eq!(direct, notification);
}

#[test]
fn main_events_use_session_id_as_agent_identity() {
    let event = emitted("UserPromptSubmit", USER_PROMPT_SUBMIT);

    assert_eq!(event.session_id, "session-8");
    assert_eq!(event.agent_instance_id, "session-8");
    assert_eq!(event.turn_id, None);
}

#[test]
fn stable_agent_id_is_used_when_present() {
    let mut input = native("PreToolUse", PRE_TOOL_USE);
    input.payload["agent_id"] = Value::String("agent-5".to_owned());

    let decision = ClaudeAdapter
        .normalize(&input, &context())
        .expect("stable subagent identity must normalize");
    let AdapterDecision::Emit(event) = decision else {
        panic!("supported event must emit");
    };
    assert_eq!(event.session_id, "session-8");
    assert_eq!(event.agent_instance_id, "agent-5");
}

#[test]
fn tool_failures_remain_activity_but_stop_failures_fail_the_turn() {
    let tool = emitted("PostToolUseFailure", POST_TOOL_USE_FAILURE);
    let turn = emitted("StopFailure", STOP_FAILURE);

    assert_eq!(tool.kind, EventKind::Activity);
    assert_eq!(tool.state, CanonicalState::Working);
    assert_eq!(turn.kind, EventKind::TurnFailed);
    assert_eq!(turn.state, CanonicalState::Failed);
}

#[test]
fn sensitive_native_fields_are_not_transported() {
    for (native_event, fixture) in [
        ("UserPromptSubmit", USER_PROMPT_SUBMIT),
        ("PreToolUse", PRE_TOOL_USE),
        ("PostToolUse", POST_TOOL_USE),
        ("PostToolUseFailure", POST_TOOL_USE_FAILURE),
        ("PermissionRequest", PERMISSION_REQUEST),
        ("Notification", NOTIFICATION_PERMISSION),
        ("Stop", STOP),
        ("StopFailure", STOP_FAILURE),
    ] {
        let encoded = emitted(native_event, fixture)
            .to_json_vec()
            .expect("normalized event must encode");
        let encoded = String::from_utf8(encoded).expect("JSON is UTF-8");
        assert!(!encoded.contains("REDACTED"), "{native_event}");
        assert!(!encoded.contains("transcript"), "{native_event}");
        assert!(!encoded.contains("tool_input"), "{native_event}");
        assert!(!encoded.contains("tool_response"), "{native_event}");
        assert!(
            !encoded.contains("last_assistant_message"),
            "{native_event}"
        );
        assert!(!encoded.contains("error_details"), "{native_event}");
    }
}

#[test]
fn unsupported_events_and_notification_types_are_intentionally_ignored() {
    let unsupported_event = native("PreCompact", r#"{"session_id":"session-8"}"#);
    assert_eq!(
        ClaudeAdapter.normalize(&unsupported_event, &context()),
        Ok(AdapterDecision::Ignore)
    );

    let unsupported_notification = native(
        "Notification",
        r#"{
          "session_id":"session-8",
          "hook_event_name":"Notification",
          "notification_type":"auth_success",
          "message":"REDACTED"
        }"#,
    );
    assert_eq!(
        ClaudeAdapter.normalize(&unsupported_notification, &context()),
        Ok(AdapterDecision::Ignore)
    );
}

#[test]
fn malformed_payloads_return_sanitized_errors() {
    let cases = [
        native("SessionStart", "[]"),
        native("SessionStart", r#"{"hook_event_name":"SessionStart"}"#),
        native(
            "SessionStart",
            r#"{"hook_event_name":"SessionStart","session_id":8}"#,
        ),
        native(
            "Stop",
            r#"{"hook_event_name":"SessionStart","session_id":"session-8"}"#,
        ),
        native(
            "Notification",
            r#"{"hook_event_name":"Notification","session_id":"session-8"}"#,
        ),
        native(
            "Notification",
            r#"{
              "hook_event_name":"Notification",
              "session_id":"session-8",
              "notification_type":7
            }"#,
        ),
    ];

    for input in cases {
        let error = ClaudeAdapter
            .normalize(&input, &context())
            .expect_err("malformed accepted event must fail");
        assert!(!error.message().contains("REDACTED"));
        assert!(!error.message().contains("session-8"));
    }
}
