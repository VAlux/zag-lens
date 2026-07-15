//! Fail-open host bridge from harness lifecycle hooks to a Zellij named pipe.

use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ulid::Ulid;
use zag_lens_protocol::{
    AdapterDecision, EventContext, HarnessAdapter, NativeHookEvent, ProtocolError,
};

/// Maximum native hook body accepted before allow-listed projection.
pub const MAX_NATIVE_PAYLOAD_BYTES: usize = 1_048_576;

/// Default local pipe name shared with the plugin.
pub const DEFAULT_PIPE_NAME: &str = "zag-lens:event";

/// Environment metadata captured by the bridge process.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HookEnvironment {
    pub pane_id: Option<String>,
    pub zellij_session: Option<String>,
    pub cwd: Option<String>,
}

impl HookEnvironment {
    /// Captures only metadata needed by the normalized protocol.
    #[must_use]
    pub fn from_current_process() -> Self {
        Self {
            pane_id: non_empty_env("ZELLIJ_PANE_ID"),
            zellij_session: non_empty_env("ZELLIJ_SESSION_NAME"),
            cwd: std::env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()),
        }
    }

    fn event_context(&self) -> Result<EventContext, BridgeFailure> {
        let pane_id = self
            .pane_id
            .as_deref()
            .map(str::trim)
            .filter(|pane_id| !pane_id.is_empty())
            .ok_or(BridgeFailure::OutsideZellij)?;
        let pane_id = canonical_terminal_pane_id(pane_id);

        let occurred_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|_| BridgeFailure::Clock)?;

        Ok(EventContext {
            event_id: Ulid::new().to_string(),
            occurred_at,
            pane_id,
            zellij_session: self.zellij_session.clone(),
            cwd: self.cwd.clone(),
        })
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn canonical_terminal_pane_id(pane_id: &str) -> String {
    if pane_id.starts_with("terminal_") {
        pane_id.to_owned()
    } else if pane_id.parse::<u32>().is_ok() {
        format!("terminal_{pane_id}")
    } else {
        pane_id.to_owned()
    }
}

/// Observable fail-open result. The CLI always converts every variant to exit 0.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookOutcome {
    Delivered,
    Ignored,
    Failed(BridgeFailure),
}

/// Sanitized failure categories suitable for opt-in diagnostics.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BridgeFailure {
    #[error("native hook payload is too large")]
    NativePayloadTooLarge,
    #[error("native hook payload is not a JSON object")]
    InvalidNativePayload,
    #[error("hook is not running inside Zellij")]
    OutsideZellij,
    #[error("system clock could not produce an RFC 3339 timestamp")]
    Clock,
    #[error("adapter rejected the native event")]
    Adapter,
    #[error("normalized event did not satisfy the protocol")]
    Protocol,
    #[error("Zellij pipe transport failed")]
    Transport,
}

/// Transport boundary used by production Zellij delivery and deterministic tests.
pub trait EventTransport {
    /// Sends one already-validated normalized payload.
    ///
    /// # Errors
    ///
    /// Returns a sanitized transport error when delivery cannot complete.
    fn send(&self, payload: &[u8]) -> Result<(), TransportError>;
}

/// Normalizes and delivers one hook invocation without exposing raw input in errors.
#[must_use]
pub fn process_hook(
    adapter: &dyn HarnessAdapter,
    native_event: &str,
    reader: impl Read,
    environment: &HookEnvironment,
    transport: &dyn EventTransport,
) -> HookOutcome {
    let payload = match read_native_payload(reader) {
        Ok(payload) => payload,
        Err(failure) => return HookOutcome::Failed(failure),
    };
    let context = match environment.event_context() {
        Ok(context) => context,
        Err(failure) => return HookOutcome::Failed(failure),
    };
    let event = NativeHookEvent {
        native_event: native_event.to_owned(),
        payload,
    };

    let Ok(decision) = adapter.normalize(&event, &context) else {
        return HookOutcome::Failed(BridgeFailure::Adapter);
    };
    let AdapterDecision::Emit(event) = decision else {
        return HookOutcome::Ignored;
    };
    let Ok(encoded) = event.to_json_vec() else {
        return HookOutcome::Failed(BridgeFailure::Protocol);
    };

    match transport.send(&encoded) {
        Ok(()) => HookOutcome::Delivered,
        Err(_) => HookOutcome::Failed(BridgeFailure::Transport),
    }
}

fn read_native_payload(mut reader: impl Read) -> Result<Value, BridgeFailure> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take((MAX_NATIVE_PAYLOAD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| BridgeFailure::InvalidNativePayload)?;
    if bytes.len() > MAX_NATIVE_PAYLOAD_BYTES {
        return Err(BridgeFailure::NativePayloadTooLarge);
    }

    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| BridgeFailure::InvalidNativePayload)?;
    if !value.is_object() {
        return Err(BridgeFailure::InvalidNativePayload);
    }
    Ok(value)
}

/// Host implementation of the `zag-lens:event` Zellij pipe transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZellijPipeTransport {
    executable: OsString,
    pipe_name: String,
    timeout: Duration,
}

impl Default for ZellijPipeTransport {
    fn default() -> Self {
        Self {
            executable: OsString::from("zellij"),
            pipe_name: DEFAULT_PIPE_NAME.to_owned(),
            timeout: Duration::from_millis(75),
        }
    }
}

impl ZellijPipeTransport {
    #[must_use]
    pub fn new(
        executable: impl Into<OsString>,
        pipe_name: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            executable: executable.into(),
            pipe_name: pipe_name.into(),
            timeout,
        }
    }

    /// Constructs the exact child command without invoking a shell.
    #[must_use]
    pub fn command_for(&self, payload: &[u8]) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .arg("pipe")
            .arg("--name")
            .arg(&self.pipe_name)
            .arg("--")
            .arg(String::from_utf8_lossy(payload).as_ref())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
}

impl EventTransport for ZellijPipeTransport {
    fn send(&self, payload: &[u8]) -> Result<(), TransportError> {
        let mut child = self
            .command_for(payload)
            .spawn()
            .map_err(TransportError::Spawn)?;
        let deadline = Instant::now() + self.timeout;

        loop {
            match child.try_wait().map_err(TransportError::Wait)? {
                Some(status) if status.success() => return Ok(()),
                Some(status) => return Err(TransportError::Exit(status.code())),
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(TransportError::Timeout);
                }
                None => thread::sleep(Duration::from_millis(2)),
            }
        }
    }
}

/// Internal transport details; callers deliberately collapse these to `BridgeFailure::Transport`.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("could not start Zellij: {0}")]
    Spawn(std::io::Error),
    #[error("could not wait for Zellij: {0}")]
    Wait(std::io::Error),
    #[error("Zellij pipe exited unsuccessfully with code {0:?}")]
    Exit(Option<i32>),
    #[error("Zellij pipe exceeded the configured timeout")]
    Timeout,
}

impl From<ProtocolError> for BridgeFailure {
    fn from(_: ProtocolError) -> Self {
        Self::Protocol
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use zag_lens_protocol::{
        AdapterError, AdapterInfo, Attention, CanonicalState, EventKind, NormalizedEvent,
        SupportedVersions,
    };

    use super::*;

    struct TestAdapter {
        decision: TestDecision,
    }

    enum TestDecision {
        Emit,
        Ignore,
        Fail,
    }

    impl HarnessAdapter for TestAdapter {
        fn harness(&self) -> &'static str {
            "test"
        }

        fn adapter_info(&self) -> AdapterInfo {
            AdapterInfo {
                name: "test".to_owned(),
                version: 1,
            }
        }

        fn supported_versions(&self) -> SupportedVersions {
            SupportedVersions {
                minimum: "1".to_owned(),
                maximum: None,
            }
        }

        fn normalize(
            &self,
            event: &NativeHookEvent,
            context: &EventContext,
        ) -> Result<AdapterDecision, AdapterError> {
            match self.decision {
                TestDecision::Ignore => Ok(AdapterDecision::Ignore),
                TestDecision::Fail => Err(AdapterError::new("sanitized failure")),
                TestDecision::Emit => Ok(AdapterDecision::Emit(Box::new(NormalizedEvent {
                    schema_version: 1,
                    event_id: context.event_id.clone(),
                    occurred_at: context.occurred_at.clone(),
                    harness: "test".to_owned(),
                    native_event: event.native_event.clone(),
                    kind: EventKind::InteractionRequired,
                    state: CanonicalState::WaitingForUser,
                    session_id: "session-1".to_owned(),
                    agent_instance_id: "session-1".to_owned(),
                    turn_id: Some("turn-1".to_owned()),
                    pane_id: context.pane_id.clone(),
                    zellij_session: context.zellij_session.clone(),
                    cwd: context.cwd.clone(),
                    attention: Some(Attention {
                        kind: "permission".to_owned(),
                        summary: Some("Permission required".to_owned()),
                    }),
                    adapter: self.adapter_info(),
                }))),
            }
        }
    }

    #[derive(Default)]
    struct RecordingTransport {
        payloads: RefCell<Vec<Vec<u8>>>,
        fail: bool,
    }

    impl EventTransport for RecordingTransport {
        fn send(&self, payload: &[u8]) -> Result<(), TransportError> {
            if self.fail {
                return Err(TransportError::Timeout);
            }
            self.payloads.borrow_mut().push(payload.to_vec());
            Ok(())
        }
    }

    fn environment() -> HookEnvironment {
        HookEnvironment {
            pane_id: Some("terminal_3".to_owned()),
            zellij_session: Some("work".to_owned()),
            cwd: Some("/workspace/project".to_owned()),
        }
    }

    #[test]
    fn valid_hook_is_normalized_and_delivered_once() {
        let transport = RecordingTransport::default();
        let outcome = process_hook(
            &TestAdapter {
                decision: TestDecision::Emit,
            },
            "PermissionRequest",
            br#"{"session_id":"session-1"}"#.as_slice(),
            &environment(),
            &transport,
        );

        assert_eq!(outcome, HookOutcome::Delivered);
        let payloads = transport.payloads.borrow();
        assert_eq!(payloads.len(), 1);
        let event = zag_lens_protocol::NormalizedEvent::from_json_slice(&payloads[0])
            .expect("bridge output must satisfy protocol");
        assert_eq!(event.pane_id, "terminal_3");
        assert_eq!(event.native_event, "PermissionRequest");
    }

    #[test]
    fn numeric_zellij_pane_id_is_canonicalized_for_the_plugin() {
        let transport = RecordingTransport::default();
        let mut environment = environment();
        environment.pane_id = Some("3".to_owned());

        let outcome = process_hook(
            &TestAdapter {
                decision: TestDecision::Emit,
            },
            "PermissionRequest",
            br#"{"session_id":"session-1"}"#.as_slice(),
            &environment,
            &transport,
        );

        assert_eq!(outcome, HookOutcome::Delivered);
        let payloads = transport.payloads.borrow();
        let event = zag_lens_protocol::NormalizedEvent::from_json_slice(&payloads[0])
            .expect("bridge output must satisfy protocol");
        assert_eq!(event.pane_id, "terminal_3");
    }

    #[test]
    fn invalid_native_json_fails_open_without_delivery() {
        let transport = RecordingTransport::default();
        let outcome = process_hook(
            &TestAdapter {
                decision: TestDecision::Emit,
            },
            "Stop",
            b"not-json".as_slice(),
            &environment(),
            &transport,
        );

        assert_eq!(
            outcome,
            HookOutcome::Failed(BridgeFailure::InvalidNativePayload)
        );
        assert!(transport.payloads.borrow().is_empty());
    }

    #[test]
    fn non_object_native_json_is_rejected() {
        let outcome = process_hook(
            &TestAdapter {
                decision: TestDecision::Emit,
            },
            "Stop",
            b"[]".as_slice(),
            &environment(),
            &RecordingTransport::default(),
        );

        assert_eq!(
            outcome,
            HookOutcome::Failed(BridgeFailure::InvalidNativePayload)
        );
    }

    #[test]
    fn oversized_native_payload_is_rejected_before_adapter() {
        let input = vec![b' '; MAX_NATIVE_PAYLOAD_BYTES + 1];
        let outcome = process_hook(
            &TestAdapter {
                decision: TestDecision::Fail,
            },
            "Stop",
            input.as_slice(),
            &environment(),
            &RecordingTransport::default(),
        );

        assert_eq!(
            outcome,
            HookOutcome::Failed(BridgeFailure::NativePayloadTooLarge)
        );
    }

    #[test]
    fn missing_zellij_pane_fails_open() {
        let outcome = process_hook(
            &TestAdapter {
                decision: TestDecision::Emit,
            },
            "Stop",
            b"{}".as_slice(),
            &HookEnvironment::default(),
            &RecordingTransport::default(),
        );

        assert_eq!(outcome, HookOutcome::Failed(BridgeFailure::OutsideZellij));
    }

    #[test]
    fn ignored_event_does_not_touch_transport() {
        let transport = RecordingTransport::default();
        let outcome = process_hook(
            &TestAdapter {
                decision: TestDecision::Ignore,
            },
            "Unknown",
            b"{}".as_slice(),
            &environment(),
            &transport,
        );

        assert_eq!(outcome, HookOutcome::Ignored);
        assert!(transport.payloads.borrow().is_empty());
    }

    #[test]
    fn adapter_and_transport_errors_are_sanitized() {
        let adapter_failure = process_hook(
            &TestAdapter {
                decision: TestDecision::Fail,
            },
            "Stop",
            b"{}".as_slice(),
            &environment(),
            &RecordingTransport::default(),
        );
        let transport_failure = process_hook(
            &TestAdapter {
                decision: TestDecision::Emit,
            },
            "Stop",
            b"{}".as_slice(),
            &environment(),
            &RecordingTransport {
                payloads: RefCell::default(),
                fail: true,
            },
        );

        assert_eq!(adapter_failure, HookOutcome::Failed(BridgeFailure::Adapter));
        assert_eq!(
            transport_failure,
            HookOutcome::Failed(BridgeFailure::Transport)
        );
    }

    #[test]
    fn zellij_command_uses_named_pipe_without_shell() {
        let transport =
            ZellijPipeTransport::new("/test/zellij", "zag-lens:event", Duration::from_millis(50));
        let command = transport.command_for(br#"{"schema_version":1}"#);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(command.get_program(), "/test/zellij");
        assert_eq!(
            args,
            [
                "pipe",
                "--name",
                "zag-lens:event",
                "--",
                r#"{"schema_version":1}"#,
            ]
        );
    }
}
