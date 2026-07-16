use serde_json::Value;
use zag_lens_opencode_adapter::OpenCodeAdapter;
use zag_lens_protocol::{
    AdapterDecision, CanonicalState, EventContext, EventKind, HarnessAdapter, NativeHookEvent,
};

const SESSION_CREATED: &str =
    include_str!("../../../tests/fixtures/opencode/1.17.15/session-created.json");
const BUSY: &str = include_str!("../../../tests/fixtures/opencode/1.17.15/session-busy.json");
const RETRY: &str = include_str!("../../../tests/fixtures/opencode/1.17.15/session-retry.json");
const PERMISSION_ASKED: &str =
    include_str!("../../../tests/fixtures/opencode/1.17.15/permission-asked.json");
const PERMISSION_REJECTED: &str =
    include_str!("../../../tests/fixtures/opencode/1.17.15/permission-rejected.json");
const PERMISSION_ACCEPTED: &str =
    include_str!("../../../tests/fixtures/opencode/1.17.15/permission-accepted.json");
const QUESTION_ASKED: &str =
    include_str!("../../../tests/fixtures/opencode/1.17.15/question-asked.json");
const QUESTION_REPLIED: &str =
    include_str!("../../../tests/fixtures/opencode/1.17.15/question-replied.json");
const QUESTION_REJECTED: &str =
    include_str!("../../../tests/fixtures/opencode/1.17.15/question-rejected.json");
const MESSAGE_COMPLETED: &str =
    include_str!("../../../tests/fixtures/opencode/1.17.15/message-completed.json");
const SESSION_FAILED: &str =
    include_str!("../../../tests/fixtures/opencode/1.17.15/session-failed.json");
const SESSION_ABORTED: &str =
    include_str!("../../../tests/fixtures/opencode/1.17.15/session-aborted.json");
const SESSION_DELETED: &str =
    include_str!("../../../tests/fixtures/opencode/1.17.15/session-deleted.json");

fn context() -> EventContext {
    EventContext {
        event_id: "01J2Z3Y4X5W6V7T8S9R0Q1P2N3".to_owned(),
        occurred_at: "2026-07-15T12:00:00Z".to_owned(),
        pane_id: "terminal_7".to_owned(),
        zellij_session: Some("work".to_owned()),
        cwd: Some("/workspace/project".to_owned()),
    }
}

fn emitted(native_event: &str, fixture: &str) -> Box<zag_lens_protocol::NormalizedEvent> {
    let payload: Value = serde_json::from_str(fixture).expect("valid fixture");
    let event = NativeHookEvent {
        native_event: native_event.to_owned(),
        payload,
    };
    match OpenCodeAdapter
        .normalize(&event, &context())
        .expect("fixture normalizes")
    {
        AdapterDecision::Emit(event) => event,
        AdapterDecision::Ignore => panic!("fixture must emit"),
    }
}

#[test]
fn lifecycle_events_map_to_canonical_states() {
    for (native, fixture, kind, state) in [
        (
            "session.created",
            SESSION_CREATED,
            EventKind::SessionStarted,
            CanonicalState::Ready,
        ),
        (
            "session.status",
            BUSY,
            EventKind::TurnStarted,
            CanonicalState::Working,
        ),
        (
            "session.status",
            RETRY,
            EventKind::Activity,
            CanonicalState::Working,
        ),
        (
            "permission.replied",
            PERMISSION_ACCEPTED,
            EventKind::Activity,
            CanonicalState::Working,
        ),
        (
            "permission.replied",
            PERMISSION_REJECTED,
            EventKind::TurnCancelled,
            CanonicalState::Stopped,
        ),
        (
            "question.replied",
            QUESTION_REPLIED,
            EventKind::Activity,
            CanonicalState::Working,
        ),
        (
            "question.rejected",
            QUESTION_REJECTED,
            EventKind::TurnCancelled,
            CanonicalState::Stopped,
        ),
        (
            "message.updated",
            MESSAGE_COMPLETED,
            EventKind::TurnCompleted,
            CanonicalState::Succeeded,
        ),
        (
            "session.error",
            SESSION_FAILED,
            EventKind::TurnFailed,
            CanonicalState::Failed,
        ),
        (
            "session.error",
            SESSION_ABORTED,
            EventKind::TurnCancelled,
            CanonicalState::Stopped,
        ),
        (
            "session.deleted",
            SESSION_DELETED,
            EventKind::SessionEnded,
            CanonicalState::Stopped,
        ),
    ] {
        let event = emitted(native, fixture);
        assert_eq!(event.kind, kind, "{native}");
        assert_eq!(event.state, state, "{native}");
        assert_eq!(event.harness, "opencode");
        assert_eq!(event.session_id, "ses_local_7");
        assert_eq!(event.agent_instance_id, "ses_local_7");
    }
}

#[test]
fn interaction_events_emit_only_coarse_attention() {
    let permission = emitted("permission.asked", PERMISSION_ASKED);
    assert_eq!(permission.kind, EventKind::InteractionRequired);
    assert_eq!(permission.attention.expect("attention").kind, "permission");

    let question = emitted("question.asked", QUESTION_ASKED);
    let attention = question.attention.expect("attention");
    assert_eq!(attention.kind, "question");
    assert_eq!(
        attention.summary.as_deref(),
        Some("OpenCode requires an answer")
    );
}

#[test]
fn assistant_message_id_becomes_turn_id() {
    let event = emitted("message.updated", MESSAGE_COMPLETED);
    assert_eq!(event.turn_id.as_deref(), Some("msg_assistant_9"));
}

#[test]
fn child_sessions_are_tracked_as_independent_agent_instances() {
    let event = NativeHookEvent {
        native_event: "session.created".to_owned(),
        payload: serde_json::json!({
            "event_type": "session.created",
            "session_id": "ses_child_3"
        }),
    };
    let AdapterDecision::Emit(event) = OpenCodeAdapter
        .normalize(&event, &context())
        .expect("child session normalizes")
    else {
        panic!("child session must emit");
    };
    assert_eq!(event.session_id, "ses_child_3");
    assert_eq!(event.agent_instance_id, "ses_child_3");
}

#[test]
fn idle_and_error_message_completion_are_ignored() {
    for (native_event, payload) in [
        (
            "session.idle",
            r#"{"event_type":"session.idle","session_id":"ses_local_7"}"#,
        ),
        (
            "session.status",
            r#"{"event_type":"session.status","session_id":"ses_local_7","status":"idle"}"#,
        ),
        (
            "message.updated",
            r#"{"event_type":"message.updated","session_id":"ses_local_7","turn_id":"msg_1","completed":true,"has_error":true}"#,
        ),
    ] {
        let event = NativeHookEvent {
            native_event: native_event.to_owned(),
            payload: serde_json::from_str(payload).expect("valid JSON"),
        };
        assert_eq!(
            OpenCodeAdapter.normalize(&event, &context()),
            Ok(AdapterDecision::Ignore),
            "{native_event}"
        );
    }
}

#[test]
fn malformed_or_mismatched_envelopes_are_rejected_safely() {
    let missing_session = NativeHookEvent {
        native_event: "session.created".to_owned(),
        payload: serde_json::json!({"event_type": "session.created"}),
    };
    assert!(
        OpenCodeAdapter
            .normalize(&missing_session, &context())
            .is_err()
    );

    let mismatched = NativeHookEvent {
        native_event: "session.created".to_owned(),
        payload: serde_json::json!({
            "event_type": "session.deleted",
            "session_id": "ses_local_7"
        }),
    };
    assert!(OpenCodeAdapter.normalize(&mismatched, &context()).is_err());
}

#[test]
fn session_error_without_identity_is_intentionally_ignored() {
    let event = NativeHookEvent {
        native_event: "session.error".to_owned(),
        payload: serde_json::json!({
            "event_type": "session.error",
            "error_name": "UnknownError"
        }),
    };
    assert_eq!(
        OpenCodeAdapter.normalize(&event, &context()),
        Ok(AdapterDecision::Ignore)
    );
}

#[test]
fn supported_version_and_adapter_identity_are_declared() {
    assert_eq!(OpenCodeAdapter.harness(), "opencode");
    assert_eq!(OpenCodeAdapter.supported_versions().minimum, "1.17.15");
    assert_eq!(OpenCodeAdapter.supported_versions().maximum, None);
    assert_eq!(OpenCodeAdapter.adapter_info().name, "opencode-local-plugin");
}

#[test]
fn normalized_payload_contains_no_sensitive_native_fields() {
    for (native, fixture) in [
        ("permission.asked", PERMISSION_ASKED),
        ("question.asked", QUESTION_ASKED),
        ("session.error", SESSION_FAILED),
        ("message.updated", MESSAGE_COMPLETED),
    ] {
        let encoded = emitted(native, fixture)
            .to_json_vec()
            .expect("normalized event serializes");
        let encoded = String::from_utf8(encoded).expect("UTF-8 JSON");
        for forbidden in [
            "REDACTED",
            "patterns",
            "metadata",
            "tool_input",
            "tool_result",
            "error_message",
            "assistant_text",
        ] {
            assert!(!encoded.contains(forbidden), "{native}: {forbidden}");
        }
    }
}
