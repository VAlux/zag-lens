//! Versioned normalized event protocol shared by Zag Lens components.
//!
//! Native harness payloads do not belong in this crate. Adapters project them
//! onto [`NormalizedEvent`], and every transport consumer validates the result
//! through [`NormalizedEvent::from_json_slice`].

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ulid::Ulid;
use uuid::Uuid;

/// The latest normalized event schema emitted by this release.
pub const SCHEMA_VERSION: u16 = 2;

/// The oldest normalized event schema accepted by this release.
pub const MINIMUM_SCHEMA_VERSION: u16 = 1;

/// Default and absolute bridge-to-plugin payload limit.
pub const MAX_PAYLOAD_BYTES: usize = 65_536;

/// A harness-neutral lifecycle event delivered to the Zellij plugin.
///
/// Serde intentionally ignores unknown JSON fields. This allows producers to
/// add non-semantic metadata without breaking older consumers of that schema.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NormalizedEvent {
    pub schema_version: u16,
    pub event_id: String,
    pub occurred_at: String,
    pub harness: String,
    pub native_event: String,
    pub kind: EventKind,
    pub state: CanonicalState,
    pub session_id: String,
    pub agent_instance_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zellij_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<Attention>,
    pub adapter: AdapterInfo,
}

impl NormalizedEvent {
    /// Parses and validates one bridge-to-plugin payload.
    ///
    /// The byte limit is checked before JSON allocation. Unknown fields are
    /// ignored, while unsupported schema versions, kinds, and states are
    /// rejected.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the payload exceeds the limit, is not a
    /// schema-valid JSON event, or violates a semantic protocol invariant.
    pub fn from_json_slice(payload: &[u8]) -> Result<Self, ProtocolError> {
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(ProtocolError::PayloadTooLarge {
                actual: payload.len(),
                maximum: MAX_PAYLOAD_BYTES,
            });
        }

        let event: Self = serde_json::from_slice(payload).map_err(ProtocolError::MalformedJson)?;
        event.validate().map_err(ProtocolError::Validation)?;
        Ok(event)
    }

    /// Serializes a validated event and enforces the transport limit.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the event is invalid, cannot be encoded,
    /// or its encoded representation exceeds the transport limit.
    pub fn to_json_vec(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate().map_err(ProtocolError::Validation)?;
        let payload = serde_json::to_vec(self).map_err(ProtocolError::Serialization)?;
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(ProtocolError::PayloadTooLarge {
                actual: payload.len(),
                maximum: MAX_PAYLOAD_BYTES,
            });
        }
        Ok(payload)
    }

    /// Checks schema-level invariants independently of transport parsing.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] for an unsupported schema, malformed
    /// identifier or timestamp, empty required value, or inconsistent state.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !(MINIMUM_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&self.schema_version) {
            return Err(ValidationError::UnsupportedSchemaVersion {
                actual: self.schema_version,
                minimum: MINIMUM_SCHEMA_VERSION,
                maximum: SCHEMA_VERSION,
            });
        }
        if self.schema_version == 1 && self.kind == EventKind::TurnCancelled {
            return Err(ValidationError::KindUnsupportedBySchema {
                kind: self.kind,
                schema_version: self.schema_version,
            });
        }
        if !is_event_id(&self.event_id) {
            return Err(ValidationError::InvalidEventId);
        }
        if OffsetDateTime::parse(&self.occurred_at, &Rfc3339).is_err() {
            return Err(ValidationError::InvalidTimestamp);
        }

        for (field, value) in [
            ("harness", self.harness.as_str()),
            ("native_event", self.native_event.as_str()),
            ("session_id", self.session_id.as_str()),
            ("agent_instance_id", self.agent_instance_id.as_str()),
            ("pane_id", self.pane_id.as_str()),
            ("adapter.name", self.adapter.name.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ValidationError::EmptyRequiredField(field));
            }
        }

        if self.adapter.version == 0 {
            return Err(ValidationError::InvalidAdapterVersion);
        }

        let expected = self.kind.canonical_state();
        if self.state != expected {
            return Err(ValidationError::KindStateMismatch {
                kind: self.kind,
                state: self.state,
                expected,
            });
        }

        Ok(())
    }

    /// Stable state ownership identity; pane association is deliberately absent.
    #[must_use]
    pub fn agent_identity(&self) -> AgentIdentity {
        AgentIdentity {
            harness: self.harness.clone(),
            session_id: self.session_id.clone(),
            agent_instance_id: self.agent_instance_id.clone(),
        }
    }

    /// Key used by bounded recent-ID sets to suppress duplicate deliveries.
    #[must_use]
    pub fn deduplication_key(&self) -> &str {
        &self.event_id
    }
}

fn is_event_id(value: &str) -> bool {
    Uuid::parse_str(value).is_ok() || value.parse::<Ulid>().is_ok()
}

/// Protocol-level lifecycle classification.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    SessionStarted,
    TurnStarted,
    Activity,
    InteractionRequired,
    TurnCompleted,
    TurnFailed,
    TurnCancelled,
    SessionEnded,
}

impl EventKind {
    /// State that a valid incoming event of this kind must carry.
    #[must_use]
    pub const fn canonical_state(self) -> CanonicalState {
        match self {
            Self::SessionStarted => CanonicalState::Ready,
            Self::TurnStarted | Self::Activity => CanonicalState::Working,
            Self::InteractionRequired => CanonicalState::WaitingForUser,
            Self::TurnCompleted => CanonicalState::Succeeded,
            Self::TurnFailed => CanonicalState::Failed,
            Self::TurnCancelled | Self::SessionEnded => CanonicalState::Stopped,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStarted => "session_started",
            Self::TurnStarted => "turn_started",
            Self::Activity => "activity",
            Self::InteractionRequired => "interaction_required",
            Self::TurnCompleted => "turn_completed",
            Self::TurnFailed => "turn_failed",
            Self::TurnCancelled => "turn_cancelled",
            Self::SessionEnded => "session_ended",
        }
    }
}

/// Harness-neutral state used by the reducer and title renderer.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalState {
    Ready,
    Working,
    WaitingForUser,
    Succeeded,
    Failed,
    Stale,
    Stopped,
}

impl CanonicalState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Working => "working",
            Self::WaitingForUser => "waiting_for_user",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Stale => "stale",
            Self::Stopped => "stopped",
        }
    }
}

/// Optional coarse details for an interaction request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Attention {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Identifies the adapter implementation that produced an event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdapterInfo {
    pub name: String,
    pub version: u32,
}

/// Primary reducer key for one main agent or subagent.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AgentIdentity {
    pub harness: String,
    pub session_id: String,
    pub agent_instance_id: String,
}

/// Native hook input passed to a harness adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeHookEvent {
    pub native_event: String,
    pub payload: Value,
}

/// Bridge-owned metadata used to complete a normalized event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventContext {
    pub event_id: String,
    pub occurred_at: String,
    pub pane_id: String,
    pub zellij_session: Option<String>,
    pub cwd: Option<String>,
}

/// Version interval for native harness payloads supported by an adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupportedVersions {
    pub minimum: String,
    pub maximum: Option<String>,
}

/// Total outcome of adapting an accepted native hook.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdapterDecision {
    Emit(Box<NormalizedEvent>),
    Ignore,
}

/// Harness-specific projection boundary used by the host bridge.
pub trait HarnessAdapter: Send + Sync {
    fn harness(&self) -> &'static str;
    fn adapter_info(&self) -> AdapterInfo;
    fn supported_versions(&self) -> SupportedVersions;
    /// Projects one native hook into a normalized event or an intentional ignore.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when an accepted native hook cannot be safely
    /// projected onto the normalized schema.
    fn normalize(
        &self,
        event: &NativeHookEvent,
        context: &EventContext,
    ) -> Result<AdapterDecision, AdapterError>;
}

/// Sanitized adapter failure suitable for optional diagnostics.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct AdapterError {
    message: String,
}

impl AdapterError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Failure to decode, validate, or encode a protocol message.
#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("payload is {actual} bytes; maximum is {maximum}")]
    PayloadTooLarge { actual: usize, maximum: usize },
    #[error("malformed normalized event JSON: {0}")]
    MalformedJson(serde_json::Error),
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error("failed to serialize normalized event: {0}")]
    Serialization(serde_json::Error),
}

/// Semantic validation failure for a decoded event.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ValidationError {
    #[error(
        "unsupported schema version {actual}; supported versions are {minimum} through {maximum}"
    )]
    UnsupportedSchemaVersion {
        actual: u16,
        minimum: u16,
        maximum: u16,
    },
    #[error("kind {kind} is not supported by schema version {schema_version}")]
    KindUnsupportedBySchema {
        kind: EventKind,
        schema_version: u16,
    },
    #[error("event_id must be a UUID or ULID")]
    InvalidEventId,
    #[error("occurred_at must be an RFC 3339 timestamp")]
    InvalidTimestamp,
    #[error("required field {0} must not be empty")]
    EmptyRequiredField(&'static str),
    #[error("adapter.version must be greater than zero")]
    InvalidAdapterVersion,
    #[error("kind {kind} requires state {expected}, not {state}")]
    KindStateMismatch {
        kind: EventKind,
        state: CanonicalState,
        expected: CanonicalState,
    },
}

impl fmt::Display for EventKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Display for CanonicalState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
