//! Codex lifecycle-hook adapter.
//!
//! This crate deliberately projects only stable lifecycle identifiers. It
//! never reads prompts, transcripts, tool arguments, tool results, or the last
//! assistant message to infer state.

use serde_json::{Map, Value};
use zag_lens_protocol::{
    AdapterDecision, AdapterError, AdapterInfo, Attention, EventContext, EventKind, HarnessAdapter,
    NativeHookEvent, NormalizedEvent, SCHEMA_VERSION, SupportedVersions,
};

const HARNESS: &str = "codex";
const ADAPTER_NAME: &str = "codex-hooks";
const ADAPTER_VERSION: u32 = 1;
const MINIMUM_CODEX_VERSION: &str = "0.144.3";

/// Adapter for Codex command lifecycle hooks.
#[derive(Clone, Copy, Debug, Default)]
pub struct CodexAdapter;

impl HarnessAdapter for CodexAdapter {
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
            minimum: MINIMUM_CODEX_VERSION.to_owned(),
            maximum: None,
        }
    }

    fn normalize(
        &self,
        event: &NativeHookEvent,
        context: &EventContext,
    ) -> Result<AdapterDecision, AdapterError> {
        let Some(classification) = classify(event)? else {
            return Ok(AdapterDecision::Ignore);
        };
        let payload = object_payload(&event.payload)?;
        validate_declared_event(payload, &event.native_event)?;

        let session_id = required_identifier(payload, "session_id")?;
        let agent_instance_id =
            optional_identifier(payload, "agent_id")?.unwrap_or_else(|| session_id.clone());
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
            session_id,
            agent_instance_id,
            turn_id,
            pane_id: context.pane_id.clone(),
            zellij_session: context.zellij_session.clone(),
            cwd: context.cwd.clone(),
            attention: classification.attention,
            adapter: self.adapter_info(),
        };

        normalized.validate().map_err(|error| {
            AdapterError::new(format!("normalized Codex event is invalid: {error}"))
        })?;
        Ok(AdapterDecision::Emit(Box::new(normalized)))
    }
}

struct Classification {
    kind: EventKind,
    attention: Option<Attention>,
}

fn classify(event: &NativeHookEvent) -> Result<Option<Classification>, AdapterError> {
    let kind = match event.native_event.as_str() {
        "SessionStart" => EventKind::SessionStarted,
        "UserPromptSubmit" => EventKind::TurnStarted,
        "PreToolUse" | "PostToolUse" => {
            if !is_supported_tool(object_payload(&event.payload)?)? {
                return Ok(None);
            }
            EventKind::Activity
        }
        "PermissionRequest" => EventKind::InteractionRequired,
        "Stop" => EventKind::TurnCompleted,
        // Reserved for a future Codex lifecycle source that explicitly reports
        // an unrecoverable turn failure. Current tool response contents are
        // never inspected or interpreted as this event.
        "TurnFailure" => EventKind::TurnFailed,
        _ => return Ok(None),
    };

    let attention = (kind == EventKind::InteractionRequired).then(|| Attention {
        kind: "permission".to_owned(),
        summary: Some("Codex requires permission".to_owned()),
    });
    Ok(Some(Classification { kind, attention }))
}

fn object_payload(payload: &Value) -> Result<&Map<String, Value>, AdapterError> {
    payload
        .as_object()
        .ok_or_else(|| AdapterError::new("Codex hook payload must be a JSON object"))
}

fn validate_declared_event(
    payload: &Map<String, Value>,
    native_event: &str,
) -> Result<(), AdapterError> {
    let Some(value) = payload.get("hook_event_name") else {
        return Ok(());
    };
    let declared = value
        .as_str()
        .ok_or_else(|| AdapterError::new("Codex hook field hook_event_name must be a string"))?;
    if declared != native_event {
        return Err(AdapterError::new(
            "Codex hook field hook_event_name does not match the selected event",
        ));
    }
    Ok(())
}

fn required_identifier(
    payload: &Map<String, Value>,
    field: &'static str,
) -> Result<String, AdapterError> {
    optional_identifier(payload, field)?
        .ok_or_else(|| AdapterError::new(format!("Codex hook field {field} is required")))
}

fn optional_identifier(
    payload: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, AdapterError> {
    let Some(value) = payload.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .ok_or_else(|| AdapterError::new(format!("Codex hook field {field} must be a string")))?;
    if value.trim().is_empty() {
        return Err(AdapterError::new(format!(
            "Codex hook field {field} must not be empty"
        )));
    }
    Ok(Some(value.to_owned()))
}

fn is_supported_tool(payload: &Map<String, Value>) -> Result<bool, AdapterError> {
    let tool_name = required_identifier(payload, "tool_name")?;
    Ok(matches!(tool_name.as_str(), "Bash" | "apply_patch") || tool_name.starts_with("mcp__"))
}
