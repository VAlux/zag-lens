use zag_lens_protocol::{
    CanonicalState, EventKind, MAX_PAYLOAD_BYTES, MINIMUM_SCHEMA_VERSION, NormalizedEvent,
    ProtocolError, SCHEMA_VERSION, ValidationError,
};

const VALID: &[u8] = include_bytes!("../../../tests/fixtures/protocol/valid.json");
const DUPLICATE: &[u8] = include_bytes!("../../../tests/fixtures/protocol/duplicate.json");
const FUTURE_VERSION: &[u8] =
    include_bytes!("../../../tests/fixtures/protocol/future-version.json");
const MALFORMED: &[u8] = include_bytes!("../../../tests/fixtures/protocol/malformed.json");

#[test]
fn valid_event_accepts_unknown_fields() {
    let event = NormalizedEvent::from_json_slice(VALID).expect("fixture must be valid");

    assert_eq!(event.schema_version, MINIMUM_SCHEMA_VERSION);
    assert_eq!(event.kind, EventKind::InteractionRequired);
    assert_eq!(event.state, CanonicalState::WaitingForUser);
    assert_eq!(event.agent_identity().session_id, "session-7");
    assert_eq!(event.agent_identity().agent_instance_id, "subagent-2");
}

#[test]
fn duplicate_delivery_has_the_same_deduplication_key_and_identity() {
    let first = NormalizedEvent::from_json_slice(VALID).expect("fixture must be valid");
    let duplicate =
        NormalizedEvent::from_json_slice(DUPLICATE).expect("duplicate fixture must be valid");

    assert_eq!(first.deduplication_key(), duplicate.deduplication_key());
    assert_eq!(first.agent_identity(), duplicate.agent_identity());
}

#[test]
fn malformed_json_is_rejected() {
    assert!(matches!(
        NormalizedEvent::from_json_slice(MALFORMED),
        Err(ProtocolError::MalformedJson(_))
    ));
}

#[test]
fn missing_required_field_is_rejected() {
    let payload = String::from_utf8(VALID.to_vec())
        .expect("UTF-8 fixture")
        .replace("  \"pane_id\": \"terminal_3\",\n", "");

    assert!(matches!(
        NormalizedEvent::from_json_slice(payload.as_bytes()),
        Err(ProtocolError::MalformedJson(_))
    ));
}

#[test]
fn payload_larger_than_limit_is_rejected_before_parsing() {
    let oversized = vec![b' '; MAX_PAYLOAD_BYTES + 1];

    assert!(matches!(
        NormalizedEvent::from_json_slice(&oversized),
        Err(ProtocolError::PayloadTooLarge {
            actual,
            maximum: MAX_PAYLOAD_BYTES
        }) if actual == MAX_PAYLOAD_BYTES + 1
    ));
}

#[test]
fn future_schema_version_is_rejected() {
    assert!(matches!(
        NormalizedEvent::from_json_slice(FUTURE_VERSION),
        Err(ProtocolError::Validation(
            ValidationError::UnsupportedSchemaVersion {
                actual: 3,
                minimum: MINIMUM_SCHEMA_VERSION,
                maximum: SCHEMA_VERSION
            }
        ))
    ));
}

#[test]
fn version_two_accepts_turn_cancelled() {
    let payload = String::from_utf8(VALID.to_vec())
        .expect("UTF-8 fixture")
        .replace("\"schema_version\": 1", "\"schema_version\": 2")
        .replace(
            "\"kind\": \"interaction_required\"",
            "\"kind\": \"turn_cancelled\"",
        )
        .replace("\"state\": \"waiting_for_user\"", "\"state\": \"stopped\"");
    let event = NormalizedEvent::from_json_slice(payload.as_bytes()).expect("version 2 cancel");
    assert_eq!(event.kind, EventKind::TurnCancelled);
    assert_eq!(event.state, CanonicalState::Stopped);
}

#[test]
fn version_one_rejects_turn_cancelled() {
    let payload = String::from_utf8(VALID.to_vec())
        .expect("UTF-8 fixture")
        .replace(
            "\"kind\": \"interaction_required\"",
            "\"kind\": \"turn_cancelled\"",
        )
        .replace("\"state\": \"waiting_for_user\"", "\"state\": \"stopped\"");
    assert!(matches!(
        NormalizedEvent::from_json_slice(payload.as_bytes()),
        Err(ProtocolError::Validation(
            ValidationError::KindUnsupportedBySchema {
                kind: EventKind::TurnCancelled,
                schema_version: 1
            }
        ))
    ));
}

#[test]
fn invalid_event_id_is_rejected() {
    let payload = String::from_utf8(VALID.to_vec())
        .expect("UTF-8 fixture")
        .replace("01J2Z3Y4X5W6V7T8S9R0Q1P2N3", "not-an-id");

    assert!(matches!(
        NormalizedEvent::from_json_slice(payload.as_bytes()),
        Err(ProtocolError::Validation(ValidationError::InvalidEventId))
    ));
}

#[test]
fn invalid_timestamp_is_rejected() {
    let payload = String::from_utf8(VALID.to_vec())
        .expect("UTF-8 fixture")
        .replace("2026-07-13T12:00:00.000Z", "tomorrow");

    assert!(matches!(
        NormalizedEvent::from_json_slice(payload.as_bytes()),
        Err(ProtocolError::Validation(ValidationError::InvalidTimestamp))
    ));
}

#[test]
fn mismatched_kind_and_state_is_rejected() {
    let payload = String::from_utf8(VALID.to_vec())
        .expect("UTF-8 fixture")
        .replace("waiting_for_user", "working");

    assert!(matches!(
        NormalizedEvent::from_json_slice(payload.as_bytes()),
        Err(ProtocolError::Validation(
            ValidationError::KindStateMismatch { .. }
        ))
    ));
}

#[test]
fn validated_event_round_trips() {
    let event = NormalizedEvent::from_json_slice(VALID).expect("fixture must be valid");
    let encoded = event.to_json_vec().expect("valid event must serialize");
    let decoded = NormalizedEvent::from_json_slice(&encoded).expect("encoded event must parse");

    assert_eq!(event, decoded);
}
