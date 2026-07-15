use serde_json::Value;
use zag_lens_codex_adapter::CodexAdapter;
use zag_lens_protocol::{
    AdapterDecision, CanonicalState, EventContext, EventKind, HarnessAdapter, NativeHookEvent,
};

const SESSION_START: &str =
    include_str!("../../../tests/fixtures/codex/0.144.3/session-start.json");
const USER_PROMPT_SUBMIT: &str =
    include_str!("../../../tests/fixtures/codex/0.144.3/user-prompt-submit.json");
const PRE_TOOL_USE: &str = include_str!("../../../tests/fixtures/codex/0.144.3/pre-tool-use.json");
const POST_TOOL_USE_FAILURE: &str =
    include_str!("../../../tests/fixtures/codex/0.144.3/post-tool-use-failure.json");
const PERMISSION_REQUEST: &str =
    include_str!("../../../tests/fixtures/codex/0.144.3/permission-request.json");
const STOP: &str = include_str!("../../../tests/fixtures/codex/0.144.3/stop.json");
const TURN_FAILURE: &str = include_str!("../../../tests/fixtures/codex/0.144.3/turn-failure.json");

fn context() -> EventContext {
    EventContext {
        event_id: "018f1324-7e9f-7bd2-a27e-0b1a489e7382".to_owned(),
        occurred_at: "2026-07-13T12:00:00Z".to_owned(),
        pane_id: "terminal_3".to_owned(),
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
    let decision = CodexAdapter
        .normalize(&native(native_event, fixture), &context())
        .expect("fixture must normalize");
    match decision {
        AdapterDecision::Emit(event) => event,
        AdapterDecision::Ignore => panic!("supported event must not be ignored"),
    }
}

#[test]
fn supported_version_and_adapter_identity_are_declared() {
    let adapter = CodexAdapter;

    assert_eq!(adapter.harness(), "codex");
    assert_eq!(adapter.adapter_info().name, "codex-hooks");
    assert_eq!(adapter.adapter_info().version, 1);
    assert_eq!(adapter.supported_versions().minimum, "0.144.3");
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
            "Stop",
            STOP,
            EventKind::TurnCompleted,
            CanonicalState::Succeeded,
        ),
        (
            "TurnFailure",
            TURN_FAILURE,
            EventKind::TurnFailed,
            CanonicalState::Failed,
        ),
    ];

    for (native_event, fixture, expected_kind, expected_state) in cases {
        let event = emitted(native_event, fixture);
        assert_eq!(event.kind, expected_kind, "{native_event}");
        assert_eq!(event.state, expected_state, "{native_event}");
        assert_eq!(event.native_event, native_event);
        assert_eq!(event.harness, "codex");
    }
}

#[test]
fn permission_request_transitions_to_waiting_with_coarse_attention() {
    let event = emitted("PermissionRequest", PERMISSION_REQUEST);

    let attention = event.attention.expect("permission requires attention");
    assert_eq!(attention.kind, "permission");
    assert_eq!(
        attention.summary.as_deref(),
        Some("Codex requires permission")
    );
}

#[test]
fn main_events_use_session_id_as_agent_identity() {
    let event = emitted("UserPromptSubmit", USER_PROMPT_SUBMIT);

    assert_eq!(event.session_id, "session-7");
    assert_eq!(event.agent_instance_id, "session-7");
    assert_eq!(event.turn_id.as_deref(), Some("turn-12"));
}

#[test]
fn stable_agent_id_is_used_when_present() {
    let mut input = native("PreToolUse", PRE_TOOL_USE);
    input.payload["agent_id"] = Value::String("agent-4".to_owned());

    let decision = CodexAdapter
        .normalize(&input, &context())
        .expect("stable subagent identity must normalize");
    let AdapterDecision::Emit(event) = decision else {
        panic!("supported event must emit");
    };
    assert_eq!(event.session_id, "session-7");
    assert_eq!(event.agent_instance_id, "agent-4");
}

#[test]
fn post_tool_failure_does_not_infer_turn_failure() {
    let event = emitted("PostToolUse", POST_TOOL_USE_FAILURE);

    assert_eq!(event.kind, EventKind::Activity);
    assert_eq!(event.state, CanonicalState::Working);
}

#[test]
fn prompts_transcripts_tool_data_and_assistant_text_are_not_transported() {
    for (native_event, fixture) in [
        ("UserPromptSubmit", USER_PROMPT_SUBMIT),
        ("PreToolUse", PRE_TOOL_USE),
        ("PostToolUse", POST_TOOL_USE_FAILURE),
        ("Stop", STOP),
    ] {
        let encoded = emitted(native_event, fixture)
            .to_json_vec()
            .expect("normalized event must encode");
        let encoded = String::from_utf8(encoded).expect("JSON is UTF-8");
        assert!(!encoded.contains("REDACTED"), "{native_event}");
        assert!(!encoded.contains("transcript"), "{native_event}");
        assert!(!encoded.contains("tool_input"), "{native_event}");
        assert!(!encoded.contains("tool_response"), "{native_event}");
        assert!(!encoded.contains("assistant_message"), "{native_event}");
    }
}

#[test]
fn unsupported_events_and_tools_are_intentionally_ignored() {
    let unsupported_event = native("PreCompact", r#"{"session_id":"session-7"}"#);
    assert_eq!(
        CodexAdapter.normalize(&unsupported_event, &context()),
        Ok(AdapterDecision::Ignore)
    );

    let mut unsupported_tool = native("PreToolUse", PRE_TOOL_USE);
    unsupported_tool.payload["tool_name"] = Value::String("WebSearch".to_owned());
    assert_eq!(
        CodexAdapter.normalize(&unsupported_tool, &context()),
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
            r#"{"hook_event_name":"SessionStart","session_id":7}"#,
        ),
        native(
            "Stop",
            r#"{"hook_event_name":"SessionStart","session_id":"session-7"}"#,
        ),
        native(
            "PreToolUse",
            r#"{"hook_event_name":"PreToolUse","session_id":"session-7"}"#,
        ),
    ];

    for input in cases {
        let error = CodexAdapter
            .normalize(&input, &context())
            .expect_err("malformed accepted event must fail");
        assert!(!error.message().contains("REDACTED"));
        assert!(!error.message().contains("session-7"));
    }
}
