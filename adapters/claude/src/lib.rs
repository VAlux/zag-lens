//! Claude Code lifecycle-hook adapter.
//!
//! Only stable lifecycle identifiers and coarse notification classifications
//! are projected. Prompt text, transcript paths, tool inputs, tool results,
//! error details, and assistant messages are deliberately never inspected.

use serde_json::{Map, Value};
use zag_lens_protocol::{
    AdapterDecision, AdapterError, AdapterInfo, Attention, EventContext, EventKind, HarnessAdapter,
    NativeHookEvent, NormalizedEvent, SCHEMA_VERSION, SupportedVersions,
};

const HARNESS: &str = "claude";
const ADAPTER_NAME: &str = "claude-code-hooks";
const ADAPTER_VERSION: u32 = 1;
const MINIMUM_CLAUDE_VERSION: &str = "2.1.207";

/// Adapter for Claude Code command lifecycle hooks.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudeAdapter;

impl HarnessAdapter for ClaudeAdapter {
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
            minimum: MINIMUM_CLAUDE_VERSION.to_owned(),
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
            AdapterError::new(format!("normalized Claude Code event is invalid: {error}"))
        })?;
        Ok(AdapterDecision::Emit(Box::new(normalized)))
    }
}

struct Classification {
    kind: EventKind,
    attention: Option<Attention>,
}

fn classify(event: &NativeHookEvent) -> Result<Option<Classification>, AdapterError> {
    let (kind, attention) = match event.native_event.as_str() {
        "SessionStart" => (EventKind::SessionStarted, None),
        "UserPromptSubmit" => (EventKind::TurnStarted, None),
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => (EventKind::Activity, None),
        "PermissionRequest" => (EventKind::InteractionRequired, Some(permission_attention())),
        "Notification" => {
            let payload = object_payload(&event.payload)?;
            let Some(notification_type) = optional_identifier(payload, "notification_type")? else {
                return Err(AdapterError::new(
                    "Claude Code hook field notification_type is required",
                ));
            };
            let Some(attention) = notification_attention(&notification_type) else {
                return Ok(None);
            };
            (EventKind::InteractionRequired, Some(attention))
        }
        "Stop" => (EventKind::TurnCompleted, None),
        "StopFailure" => (EventKind::TurnFailed, None),
        "SessionEnd" => (EventKind::SessionEnded, None),
        _ => return Ok(None),
    };

    Ok(Some(Classification { kind, attention }))
}

fn permission_attention() -> Attention {
    Attention {
        kind: "permission".to_owned(),
        summary: Some("Claude Code requires permission".to_owned()),
    }
}

fn notification_attention(notification_type: &str) -> Option<Attention> {
    let (kind, summary) = match notification_type {
        // PermissionRequest and permission_prompt intentionally produce the
        // same coarse values so the reducer can recognize one interaction.
        "permission_prompt" => ("permission", "Claude Code requires permission"),
        "idle_prompt" => ("question", "Claude Code is waiting for input"),
        "elicitation_dialog" => ("elicitation", "Claude Code requests input"),
        _ => return None,
    };
    Some(Attention {
        kind: kind.to_owned(),
        summary: Some(summary.to_owned()),
    })
}

fn object_payload(payload: &Value) -> Result<&Map<String, Value>, AdapterError> {
    payload
        .as_object()
        .ok_or_else(|| AdapterError::new("Claude Code hook payload must be a JSON object"))
}

fn validate_declared_event(
    payload: &Map<String, Value>,
    native_event: &str,
) -> Result<(), AdapterError> {
    let Some(value) = payload.get("hook_event_name") else {
        return Ok(());
    };
    let declared = value.as_str().ok_or_else(|| {
        AdapterError::new("Claude Code hook field hook_event_name must be a string")
    })?;
    if declared != native_event {
        return Err(AdapterError::new(
            "Claude Code hook field hook_event_name does not match the selected event",
        ));
    }
    Ok(())
}

fn required_identifier(
    payload: &Map<String, Value>,
    field: &'static str,
) -> Result<String, AdapterError> {
    optional_identifier(payload, field)?
        .ok_or_else(|| AdapterError::new(format!("Claude Code hook field {field} is required")))
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
    let value = value.as_str().ok_or_else(|| {
        AdapterError::new(format!("Claude Code hook field {field} must be a string"))
    })?;
    if value.trim().is_empty() {
        return Err(AdapterError::new(format!(
            "Claude Code hook field {field} must not be empty"
        )));
    }
    Ok(Some(value.to_owned()))
}
