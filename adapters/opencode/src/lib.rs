//! `OpenCode` local-TUI event adapter.
//!
//! The installed `OpenCode` plugin projects native events onto a deliberately
//! small envelope before invoking the bridge. Prompt text, permission patterns,
//! metadata, tool arguments, tool results, and error messages never cross the
//! process boundary.

use serde_json::{Map, Value};
use zag_lens_protocol::{
    AdapterDecision, AdapterError, AdapterInfo, Attention, EventContext, EventKind, HarnessAdapter,
    NativeHookEvent, NormalizedEvent, SCHEMA_VERSION, SupportedVersions,
};

const HARNESS: &str = "opencode";
const ADAPTER_NAME: &str = "opencode-local-plugin";
const ADAPTER_VERSION: u32 = 1;
const MINIMUM_OPENCODE_VERSION: &str = "1.17.15";

/// Adapter for sanitized events emitted by the installed `OpenCode` plugin.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenCodeAdapter;

impl HarnessAdapter for OpenCodeAdapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn adapter_info(&self) -> AdapterInfo {
        AdapterInfo {
            name: ADAPTER_NAME.to_owned(),
            version: ADAPTER_VERSION,
        }
    }

    fn supported_versions(&self) -> SupportedVersions {
        SupportedVersions {
            minimum: MINIMUM_OPENCODE_VERSION.to_owned(),
            maximum: None,
        }
    }

    fn normalize(
        &self,
        event: &NativeHookEvent,
        context: &EventContext,
    ) -> Result<AdapterDecision, AdapterError> {
        let payload = object_payload(&event.payload)?;
        validate_declared_event(payload, &event.native_event)?;
        let Some(classification) = classify(&event.native_event, payload)? else {
            return Ok(AdapterDecision::Ignore);
        };

        let session_id = match optional_identifier(payload, "session_id")? {
            Some(session_id) => session_id,
            None if event.native_event == "session.error" => return Ok(AdapterDecision::Ignore),
            None => {
                return Err(AdapterError::new(
                    "OpenCode plugin field session_id is required",
                ));
            }
        };
        let turn_id = optional_identifier(payload, "turn_id")?;
        let kind = classification.kind;
        let normalized = NormalizedEvent {
            schema_version: SCHEMA_VERSION,
            event_id: context.event_id.clone(),
            occurred_at: context.occurred_at.clone(),
            harness: HARNESS.to_owned(),
            native_event: event.native_event.clone(),
            kind,
            state: kind.canonical_state(),
            agent_instance_id: session_id.clone(),
            session_id,
            turn_id,
            pane_id: context.pane_id.clone(),
            zellij_session: context.zellij_session.clone(),
            cwd: context.cwd.clone(),
            attention: classification.attention,
            adapter: self.adapter_info(),
        };

        normalized.validate().map_err(|error| {
            AdapterError::new(format!("normalized OpenCode event is invalid: {error}"))
        })?;
        Ok(AdapterDecision::Emit(Box::new(normalized)))
    }
}

struct Classification {
    kind: EventKind,
    attention: Option<Attention>,
}

fn classify(
    native_event: &str,
    payload: &Map<String, Value>,
) -> Result<Option<Classification>, AdapterError> {
    let classification = match native_event {
        "session.created" => Classification::plain(EventKind::SessionStarted),
        "session.status" => match required_identifier(payload, "status")?.as_str() {
            "busy" => Classification::plain(EventKind::TurnStarted),
            "retry" => Classification::plain(EventKind::Activity),
            _ => return Ok(None),
        },
        "permission.asked" => {
            Classification::attention("permission", "OpenCode requires permission")
        }
        "question.asked" => Classification::attention("question", "OpenCode requires an answer"),
        "permission.replied" => match required_identifier(payload, "reply")?.as_str() {
            "once" | "always" => Classification::plain(EventKind::Activity),
            "reject" => Classification::plain(EventKind::TurnCancelled),
            _ => return Ok(None),
        },
        "question.replied" => Classification::plain(EventKind::Activity),
        "question.rejected" => Classification::plain(EventKind::TurnCancelled),
        "message.updated" => {
            if !optional_boolean(payload, "completed")?.unwrap_or(false)
                || optional_boolean(payload, "has_error")?.unwrap_or(false)
            {
                return Ok(None);
            }
            Classification::plain(EventKind::TurnCompleted)
        }
        "session.error" => {
            let kind = if optional_identifier(payload, "error_name")?.as_deref()
                == Some("MessageAbortedError")
            {
                EventKind::TurnCancelled
            } else {
                EventKind::TurnFailed
            };
            Classification::plain(kind)
        }
        "session.deleted" => Classification::plain(EventKind::SessionEnded),
        _ => return Ok(None),
    };
    Ok(Some(classification))
}

impl Classification {
    fn plain(kind: EventKind) -> Self {
        Self {
            kind,
            attention: None,
        }
    }

    fn attention(kind: &str, summary: &str) -> Self {
        Self {
            kind: EventKind::InteractionRequired,
            attention: Some(Attention {
                kind: kind.to_owned(),
                summary: Some(summary.to_owned()),
            }),
        }
    }
}

fn object_payload(payload: &Value) -> Result<&Map<String, Value>, AdapterError> {
    payload
        .as_object()
        .ok_or_else(|| AdapterError::new("OpenCode plugin payload must be a JSON object"))
}

fn validate_declared_event(
    payload: &Map<String, Value>,
    native_event: &str,
) -> Result<(), AdapterError> {
    let declared = required_identifier(payload, "event_type")?;
    if declared != native_event {
        return Err(AdapterError::new(
            "OpenCode plugin event_type does not match the requested native event",
        ));
    }
    Ok(())
}

fn required_identifier(
    payload: &Map<String, Value>,
    field: &'static str,
) -> Result<String, AdapterError> {
    optional_identifier(payload, field)?
        .ok_or_else(|| AdapterError::new(format!("OpenCode plugin field {field} is required")))
}

fn optional_identifier(
    payload: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, AdapterError> {
    let Some(value) = payload.get(field) else {
        return Ok(None);
    };
    let Some(value) = value.as_str() else {
        return Err(AdapterError::new(format!(
            "OpenCode plugin field {field} must be a string"
        )));
    };
    if value.trim().is_empty() {
        return Err(AdapterError::new(format!(
            "OpenCode plugin field {field} must not be empty"
        )));
    }
    Ok(Some(value.to_owned()))
}

fn optional_boolean(
    payload: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<bool>, AdapterError> {
    let Some(value) = payload.get(field) else {
        return Ok(None);
    };
    value.as_bool().map(Some).ok_or_else(|| {
        AdapterError::new(format!("OpenCode plugin field {field} must be a boolean"))
    })
}
