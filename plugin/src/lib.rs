//! Hidden Zellij runtime for normalized Zag Lens lifecycle events.

pub mod attention;
pub mod title;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

use attention::{
    AttentionConfig, AttentionController, NotificationDecision, NotificationTab, SuppressionReason,
};
use time::OffsetDateTime;
use title::{TitleAction, TitleManager};
use zag_lens_core::{
    AggregateStatus, ApplyOutcome, Icons, Reducer, ReducerConfig, TitleConfig, Transition,
    aggregate_states,
};
use zag_lens_protocol::{MAX_PAYLOAD_BYTES, NormalizedEvent};
use zellij_tile::prelude::*;

const PIPE_NAME: &str = "zag-lens:event";
const TIMER_SECONDS: f64 = 0.25;
const MAX_QUEUED_EVENTS: usize = 256;
const MAX_DIAGNOSTICS: usize = 128;
const TITLE_JOURNAL_PATH: &str = "/data/title-journal-v1";

#[derive(Default)]
struct ZagLensPlugin {
    runtime: RuntimeState,
    reducer: Reducer,
    titles: TitleManager,
    attention: AttentionController,
    host_binary: String,
    permission_stage: PermissionStage,
}

register_plugin!(ZagLensPlugin);

impl ZellijPlugin for ZagLensPlugin {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.runtime = RuntimeState::new(RuntimeConfig::from_zellij(&configuration));
        self.reducer = Reducer::new(reducer_config(&configuration));
        let title_config = title_config(&configuration);
        self.titles = match std::fs::read_to_string(TITLE_JOURNAL_PATH) {
            Ok(journal) => TitleManager::from_journal(title_config, &journal),
            Err(_) => TitleManager::new(title_config),
        };
        let parsed_attention = AttentionConfig::parse_zellij(&configuration);
        for key in parsed_attention.invalid_keys {
            self.runtime.record_diagnostic(key);
        }
        self.attention = AttentionController::new(parsed_attention.config);
        self.host_binary = configuration
            .get("host_binary")
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "zag-lens".to_owned());
        if !self.runtime.config.enabled {
            return;
        }

        // Application-state subscriptions are permission-gated by Zellij.
        // Subscribe to them only after the grant so Zellij emits the initial
        // pane/tab snapshots needed to associate hook events with stable tabs.
        subscribe(&[EventType::PermissionRequestResult]);
        let mut permissions = vec![
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
        ];
        if self.runtime.config.needs_run_commands {
            permissions.push(PermissionType::RunCommands);
        }
        request_permission(&permissions);
        self.permission_stage = PermissionStage::ApplicationPending;
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => {
                let old_tab_ids: Vec<_> = self
                    .runtime
                    .tabs_by_position
                    .values()
                    .map(|tab| tab.tab_id)
                    .collect();
                let tabs: Vec<_> = tabs
                    .into_iter()
                    .filter_map(|tab| {
                        u64::try_from(tab.tab_id).ok().map(|tab_id| TabRecord {
                            position: tab.position,
                            tab_id,
                            name: tab.name,
                            active: tab.active,
                        })
                    })
                    .collect();
                let new_tab_ids: Vec<_> = tabs.iter().map(|tab| tab.tab_id).collect();
                for tab in &tabs {
                    let action = self.titles.observe_tab(tab.tab_id, &tab.name);
                    self.execute_title_action(action);
                }
                for removed in old_tab_ids
                    .into_iter()
                    .filter(|tab_id| !new_tab_ids.contains(tab_id))
                {
                    self.titles.remove_tab(removed);
                }
                self.runtime.update_tabs(tabs);
                self.persist_title_journal();
                self.process_ready_events();
            }
            Event::PaneUpdate(manifest) => {
                self.runtime
                    .update_panes(
                        manifest
                            .panes
                            .into_iter()
                            .flat_map(|(tab_position, panes)| {
                                panes
                                    .into_iter()
                                    .filter(|pane| !pane.is_plugin)
                                    .map(move |pane| PaneRecord {
                                        pane_id: pane.id,
                                        tab_position,
                                    })
                            }),
                    );
                self.process_ready_events();
            }
            Event::PaneClosed(PaneId::Terminal(pane_id)) => {
                let tab_id = self.runtime.tab_id_for_pane(pane_id);
                self.runtime.close_pane(pane_id);
                let pane_id = format!("terminal_{pane_id}");
                for transition in self.reducer.remove_pane(&pane_id) {
                    self.handle_transition(&transition, tab_id);
                }
            }
            Event::Timer(_) => {
                self.runtime.advance_time(now_millis());
                self.process_ready_events();
                for transition in self.reducer.advance_time(OffsetDateTime::now_utc()) {
                    self.handle_transition(&transition, None);
                }
                set_timeout(TIMER_SECONDS);
            }
            Event::PermissionRequestResult(status) => self.on_permission_result(status),
            Event::BeforeClose => {
                self.runtime.before_close = true;
                for action in self.titles.shutdown_actions() {
                    self.execute_title_action(Some(action));
                }
                let _ = std::fs::remove_file(TITLE_JOURNAL_PATH);
            }
            _ => {}
        }
        false
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        let _ = self.runtime.ingest(
            &pipe_message.name,
            pipe_message.payload.as_deref(),
            now_millis(),
        );
        self.process_ready_events();
        false
    }
}

impl ZagLensPlugin {
    fn on_permission_result(&mut self, status: PermissionStatus) {
        match (self.permission_stage, status) {
            (PermissionStage::ApplicationPending, PermissionStatus::Granted) => {
                self.runtime.application_permissions_granted = true;
                self.runtime.run_commands_granted = self.runtime.config.needs_run_commands;
                subscribe(&[
                    EventType::PaneUpdate,
                    EventType::TabUpdate,
                    EventType::PaneClosed,
                    EventType::Timer,
                    EventType::BeforeClose,
                ]);
                set_timeout(TIMER_SECONDS);
                self.permission_stage = PermissionStage::Complete;
                Self::finish_permission_flow();
            }
            (PermissionStage::ApplicationPending, PermissionStatus::Denied) => {
                self.runtime.application_permissions_granted = false;
                self.runtime.run_commands_granted = false;
                self.runtime
                    .record_diagnostic("application_permissions_denied");
                if self.runtime.config.needs_run_commands {
                    self.runtime.record_diagnostic("run_commands_denied");
                }
                self.permission_stage = PermissionStage::Complete;
                Self::finish_permission_flow();
            }
            _ => {}
        }
    }

    fn finish_permission_flow() {
        set_selectable(false);
        hide_self();
    }

    fn process_ready_events(&mut self) {
        for ready in self.runtime.take_ready_events() {
            let Ok(outcome) = self.reducer.apply(&ready.event) else {
                self.runtime.record_diagnostic("reducer_rejected_event");
                continue;
            };
            if let ApplyOutcome::Applied {
                transition,
                evicted,
            } = outcome
            {
                self.handle_transition(&transition, Some(ready.tab_id));
                if let Some(evicted) = evicted {
                    self.handle_transition(&evicted, None);
                }
            }
        }
    }

    fn handle_transition(&mut self, transition: &Transition, preferred_tab_id: Option<u64>) {
        let mut affected_tabs = Vec::new();
        if let Some(tab_id) = preferred_tab_id {
            affected_tabs.push(tab_id);
        }
        for pane_id in transition.affected_panes() {
            if let Some(tab_id) = self.runtime.tab_id_for_protocol_pane(pane_id)
                && !affected_tabs.contains(&tab_id)
            {
                affected_tabs.push(tab_id);
            }
        }

        let notification_tab_id = transition
            .current
            .as_ref()
            .and_then(|snapshot| self.runtime.tab_id_for_protocol_pane(&snapshot.pane_id))
            .or(preferred_tab_id);
        if let Some(tab_id) = notification_tab_id
            && let Some(tab) = self.runtime.tab(tab_id)
        {
            let tab_name = self
                .titles
                .managed(tab_id)
                .map_or(tab.name.as_str(), |managed| managed.base_title());
            let decision = self.attention.evaluate(
                transition,
                NotificationTab {
                    name: tab_name,
                    active: tab.active,
                },
                self.runtime.run_commands_granted,
            );
            self.execute_notification(decision);
        }

        for tab_id in affected_tabs {
            let aggregate = self.aggregate_for_tab(tab_id);
            let action = self.titles.set_aggregate(tab_id, aggregate);
            self.execute_title_action(action);
        }
        self.persist_title_journal();
    }

    fn aggregate_for_tab(&self, tab_id: u64) -> Option<AggregateStatus> {
        aggregate_states(self.reducer.agents().filter_map(|snapshot| {
            (self.runtime.tab_id_for_protocol_pane(&snapshot.pane_id) == Some(tab_id))
                .then_some(snapshot.state)
        }))
    }

    fn execute_title_action(&mut self, action: Option<TitleAction>) {
        let Some(action) = action else {
            return;
        };
        if self.runtime.application_permissions_granted {
            host_rename_tab(action.tab_id(), action.title());
        }
    }

    fn execute_notification(&mut self, decision: NotificationDecision) {
        match decision {
            NotificationDecision::Emit(mut request) => {
                request.program.clone_from(&self.host_binary);
                let mut command = Vec::with_capacity(request.args.len() + 1);
                command.push(request.program);
                command.extend(request.args);
                let command_refs: Vec<_> = command.iter().map(String::as_str).collect();
                host_run_command(&command_refs);
            }
            NotificationDecision::Suppressed(SuppressionReason::RunCommandsDenied) => {
                self.runtime.record_diagnostic("run_commands_denied");
            }
            NotificationDecision::Suppressed(_) => {}
        }
    }

    fn persist_title_journal(&mut self) {
        if std::fs::write(TITLE_JOURNAL_PATH, self.titles.journal_snapshot()).is_err() {
            self.runtime.record_diagnostic("title_journal_write_failed");
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn host_rename_tab(tab_id: u64, title: &str) {
    rename_tab_with_id(tab_id, title);
}

#[cfg(not(target_arch = "wasm32"))]
fn host_rename_tab(_tab_id: u64, _title: &str) {}

#[cfg(target_arch = "wasm32")]
fn host_run_command(command: &[&str]) {
    run_command(command, BTreeMap::new());
}

#[cfg(not(target_arch = "wasm32"))]
fn host_run_command(_command: &[&str]) {}

fn reducer_config(values: &BTreeMap<String, String>) -> ReducerConfig {
    let defaults = ReducerConfig::default();
    ReducerConfig {
        enabled: parse_bool(values.get("enabled"), defaults.enabled),
        success_ttl: Duration::from_secs(parse_number(
            values.get("success_ttl_seconds"),
            defaults.success_ttl.as_secs(),
            0,
            86_400,
        )),
        stale_after: Duration::from_secs(parse_number(
            values.get("stale_after_seconds"),
            defaults.stale_after.as_secs(),
            1,
            604_800,
        )),
        ..defaults
    }
}

fn title_config(values: &BTreeMap<String, String>) -> TitleConfig {
    let mut icons = if values.get("icon_set").map(String::as_str) == Some("ascii") {
        Icons::ascii()
    } else {
        Icons::unicode()
    };
    for (key, target) in [
        ("icons.working", &mut icons.working),
        ("icons.waiting_for_user", &mut icons.waiting_for_user),
        ("icons.succeeded", &mut icons.succeeded),
        ("icons.failed", &mut icons.failed),
        ("icons.stale", &mut icons.stale),
    ] {
        if let Some(value) = values.get(key) {
            target.clone_from(value);
        }
    }
    TitleConfig {
        format: values
            .get("title_format")
            .cloned()
            .unwrap_or_else(|| "{icon} {title}".to_owned()),
        icons,
        show_counts: parse_bool(values.get("show_counts"), false),
    }
    .with_safe_format()
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PermissionStage {
    #[default]
    NotRequested,
    ApplicationPending,
    Complete,
}

/// Runtime-only configuration parsed from the Zellij plugin block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub enabled: bool,
    pub mapping_timeout_ms: u64,
    pub max_payload_bytes: usize,
    pub debug: bool,
    pub needs_run_commands: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mapping_timeout_ms: 2_000,
            max_payload_bytes: MAX_PAYLOAD_BYTES,
            debug: false,
            needs_run_commands: true,
        }
    }
}

impl RuntimeConfig {
    #[must_use]
    pub fn from_zellij(values: &BTreeMap<String, String>) -> Self {
        let mut config = Self::default();
        config.enabled = parse_bool(values.get("enabled"), config.enabled);
        config.debug = parse_bool(values.get("debug"), config.debug);
        config.mapping_timeout_ms = parse_number(
            values.get("mapping_timeout_ms"),
            config.mapping_timeout_ms,
            1,
            60_000,
        );
        config.max_payload_bytes = parse_number(
            values.get("max_payload_bytes"),
            config.max_payload_bytes,
            1,
            MAX_PAYLOAD_BYTES,
        );
        config.needs_run_commands = values.get("notification_backend").map(String::as_str)
            != Some("off")
            && values.get("notification_policy").map(String::as_str) != Some("off")
            && values.get("notification_focus").map(String::as_str) != Some("never");
        config
    }
}

fn parse_bool(value: Option<&String>, default: bool) -> bool {
    value
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn parse_number<T>(value: Option<&String>, default: T, minimum: T, maximum: T) -> T
where
    T: Copy + Ord + std::str::FromStr,
{
    value
        .and_then(|value| value.parse().ok())
        .filter(|value| *value >= minimum && *value <= maximum)
        .unwrap_or(default)
}

/// Minimal stable tab information retained from `TabUpdate`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TabRecord {
    pub position: usize,
    pub tab_id: u64,
    pub name: String,
    pub active: bool,
}

/// Terminal pane membership retained from `PaneUpdate`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneRecord {
    pub pane_id: u32,
    pub tab_position: usize,
}

/// One validated normalized event associated with a stable tab ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadyEvent {
    pub tab_id: u64,
    pub event: NormalizedEvent,
}

/// Result of accepting one named-pipe message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngestOutcome {
    Ready,
    Queued,
    Dropped(DropReason),
}

/// Sanitized reason why a pipe payload was not accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropReason {
    Disabled,
    WrongPipe,
    PipeClosed,
    PayloadTooLarge,
    InvalidPayload,
    InvalidPaneId,
    MappingTimedOut,
    QueueFull,
}

#[derive(Clone, Debug)]
struct PendingEvent {
    received_at_ms: u64,
    pane_id: u32,
    event: NormalizedEvent,
}

/// Pure application-state and queue substrate used by the Zellij plugin.
#[derive(Clone, Debug)]
pub struct RuntimeState {
    pub config: RuntimeConfig,
    tabs_by_position: HashMap<usize, TabRecord>,
    pane_positions: HashMap<u32, usize>,
    pending: VecDeque<PendingEvent>,
    ready: VecDeque<ReadyEvent>,
    closed_panes: VecDeque<u32>,
    diagnostics: VecDeque<&'static str>,
    pub application_permissions_granted: bool,
    pub run_commands_granted: bool,
    pub before_close: bool,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self::new(RuntimeConfig::default())
    }
}

impl RuntimeState {
    #[must_use]
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            config,
            tabs_by_position: HashMap::new(),
            pane_positions: HashMap::new(),
            pending: VecDeque::new(),
            ready: VecDeque::new(),
            closed_panes: VecDeque::new(),
            diagnostics: VecDeque::new(),
            application_permissions_granted: false,
            run_commands_granted: false,
            before_close: false,
        }
    }

    pub fn update_tabs(&mut self, tabs: impl IntoIterator<Item = TabRecord>) {
        self.tabs_by_position = tabs.into_iter().map(|tab| (tab.position, tab)).collect();
        self.replay_pending(u64::MAX, false);
    }

    pub fn update_panes(&mut self, panes: impl IntoIterator<Item = PaneRecord>) {
        self.pane_positions = panes
            .into_iter()
            .map(|pane| (pane.pane_id, pane.tab_position))
            .collect();
        self.replay_pending(u64::MAX, false);
    }

    #[must_use]
    pub fn ingest(&mut self, pipe_name: &str, payload: Option<&str>, now_ms: u64) -> IngestOutcome {
        if !self.config.enabled {
            return IngestOutcome::Dropped(DropReason::Disabled);
        }
        if pipe_name != PIPE_NAME {
            return IngestOutcome::Dropped(DropReason::WrongPipe);
        }
        let Some(payload) = payload else {
            return IngestOutcome::Dropped(DropReason::PipeClosed);
        };
        if payload.len() > self.config.max_payload_bytes {
            self.record_diagnostic("payload_too_large");
            return IngestOutcome::Dropped(DropReason::PayloadTooLarge);
        }
        let Ok(event) = NormalizedEvent::from_json_slice(payload.as_bytes()) else {
            self.record_diagnostic("invalid_payload");
            return IngestOutcome::Dropped(DropReason::InvalidPayload);
        };
        let Some(pane_id) = parse_terminal_pane_id(&event.pane_id) else {
            self.record_diagnostic("invalid_pane_id");
            return IngestOutcome::Dropped(DropReason::InvalidPaneId);
        };

        if let Some(tab_id) = self.tab_id_for_pane(pane_id) {
            self.ready.push_back(ReadyEvent { tab_id, event });
            return IngestOutcome::Ready;
        }
        if self.pending.len() >= MAX_QUEUED_EVENTS {
            self.record_diagnostic("mapping_queue_full");
            return IngestOutcome::Dropped(DropReason::QueueFull);
        }
        self.pending.push_back(PendingEvent {
            received_at_ms: now_ms,
            pane_id,
            event,
        });
        IngestOutcome::Queued
    }

    pub fn advance_time(&mut self, now_ms: u64) {
        self.replay_pending(now_ms, true);
    }

    pub fn close_pane(&mut self, pane_id: u32) {
        self.pane_positions.remove(&pane_id);
        self.pending.retain(|pending| pending.pane_id != pane_id);
        self.closed_panes.push_back(pane_id);
    }

    #[must_use]
    pub fn take_ready_events(&mut self) -> Vec<ReadyEvent> {
        self.ready.drain(..).collect()
    }

    #[must_use]
    pub fn take_closed_panes(&mut self) -> Vec<u32> {
        self.closed_panes.drain(..).collect()
    }

    #[must_use]
    pub fn tab(&self, tab_id: u64) -> Option<&TabRecord> {
        self.tabs_by_position
            .values()
            .find(|tab| tab.tab_id == tab_id)
    }

    pub fn diagnostics(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.diagnostics.iter().copied()
    }

    fn tab_id_for_pane(&self, pane_id: u32) -> Option<u64> {
        self.pane_positions
            .get(&pane_id)
            .and_then(|position| self.tabs_by_position.get(position))
            .map(|tab| tab.tab_id)
    }

    fn tab_id_for_protocol_pane(&self, pane_id: &str) -> Option<u64> {
        parse_terminal_pane_id(pane_id).and_then(|pane_id| self.tab_id_for_pane(pane_id))
    }

    fn replay_pending(&mut self, now_ms: u64, expire: bool) {
        let mut retained = VecDeque::with_capacity(self.pending.len());
        while let Some(pending) = self.pending.pop_front() {
            if let Some(tab_id) = self.tab_id_for_pane(pending.pane_id) {
                self.ready.push_back(ReadyEvent {
                    tab_id,
                    event: pending.event,
                });
            } else if expire
                && now_ms.saturating_sub(pending.received_at_ms) >= self.config.mapping_timeout_ms
            {
                self.record_diagnostic("mapping_timed_out");
            } else {
                retained.push_back(pending);
            }
        }
        self.pending = retained;
    }

    fn record_diagnostic(&mut self, code: &'static str) {
        if self.diagnostics.len() >= MAX_DIAGNOSTICS {
            self.diagnostics.pop_front();
        }
        self.diagnostics.push_back(code);
        if self.config.debug {
            eprintln!("zag-lens: {code}");
        }
    }
}

fn parse_terminal_pane_id(value: &str) -> Option<u32> {
    value
        .strip_prefix("terminal_")
        .unwrap_or(value)
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_EVENT: &str = r#"{
        "schema_version": 1,
        "event_id": "01J2Z3Y4X5W6V7T8S9R0Q1P2N3",
        "occurred_at": "2026-07-13T12:00:00.000Z",
        "harness": "codex",
        "native_event": "PermissionRequest",
        "kind": "interaction_required",
        "state": "waiting_for_user",
        "session_id": "session-7",
        "agent_instance_id": "session-7",
        "pane_id": "terminal_3",
        "adapter": { "name": "codex", "version": 1 }
    }"#;

    const CLAUDE_EVENT: &str = r#"{
        "schema_version": 1,
        "event_id": "01J2Z3Y4X5W6V7T8S9R0Q1P2N4",
        "occurred_at": "2026-07-13T12:00:01.000Z",
        "harness": "claude",
        "native_event": "PreToolUse",
        "kind": "activity",
        "state": "working",
        "session_id": "session-8",
        "agent_instance_id": "session-8",
        "pane_id": "terminal_4",
        "adapter": { "name": "claude-code-hooks", "version": 1 }
    }"#;

    fn mapped_runtime() -> RuntimeState {
        let mut runtime = RuntimeState::default();
        runtime.update_tabs([TabRecord {
            position: 1,
            tab_id: 42,
            name: "work".to_owned(),
            active: false,
        }]);
        runtime.update_panes([PaneRecord {
            pane_id: 3,
            tab_position: 1,
        }]);
        runtime
    }

    #[test]
    fn mapped_payload_becomes_ready_for_stable_tab() {
        let mut runtime = mapped_runtime();

        assert_eq!(
            runtime.ingest(PIPE_NAME, Some(VALID_EVENT), 1_000),
            IngestOutcome::Ready
        );
        let ready = runtime.take_ready_events();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].tab_id, 42);
        assert_eq!(ready[0].event.pane_id, "terminal_3");
    }

    #[test]
    fn event_waits_for_pane_and_tab_mapping() {
        let mut runtime = RuntimeState::default();
        assert_eq!(
            runtime.ingest(PIPE_NAME, Some(VALID_EVENT), 1_000),
            IngestOutcome::Queued
        );

        runtime.update_panes([PaneRecord {
            pane_id: 3,
            tab_position: 1,
        }]);
        assert!(runtime.take_ready_events().is_empty());
        runtime.update_tabs([TabRecord {
            position: 1,
            tab_id: 77,
            name: "mapped".to_owned(),
            active: true,
        }]);

        assert_eq!(runtime.take_ready_events()[0].tab_id, 77);
    }

    #[test]
    fn unmapped_event_expires_after_timeout() {
        let mut runtime = RuntimeState::default();
        let _ = runtime.ingest(PIPE_NAME, Some(VALID_EVENT), 1_000);

        runtime.advance_time(3_000);

        assert!(runtime.take_ready_events().is_empty());
        assert_eq!(runtime.diagnostics().last(), Some("mapping_timed_out"));
    }

    #[test]
    fn malformed_oversized_and_wrong_pipe_payloads_are_dropped() {
        let mut runtime = RuntimeState::default();
        let oversized = " ".repeat(MAX_PAYLOAD_BYTES + 1);

        assert_eq!(
            runtime.ingest("other", Some(VALID_EVENT), 0),
            IngestOutcome::Dropped(DropReason::WrongPipe)
        );
        assert_eq!(
            runtime.ingest(PIPE_NAME, Some("{}"), 0),
            IngestOutcome::Dropped(DropReason::InvalidPayload)
        );
        assert_eq!(
            runtime.ingest(PIPE_NAME, Some(&oversized), 0),
            IngestOutcome::Dropped(DropReason::PayloadTooLarge)
        );
    }

    #[test]
    fn pane_close_clears_mapping_and_pending_events() {
        let mut runtime = RuntimeState::default();
        runtime.update_panes([PaneRecord {
            pane_id: 3,
            tab_position: 1,
        }]);
        let _ = runtime.ingest(PIPE_NAME, Some(VALID_EVENT), 0);

        runtime.close_pane(3);
        runtime.update_tabs([TabRecord {
            position: 1,
            tab_id: 42,
            name: "work".to_owned(),
            active: false,
        }]);

        assert!(runtime.take_ready_events().is_empty());
        assert_eq!(runtime.take_closed_panes(), [3]);
    }

    #[test]
    fn configuration_is_bounded_and_notification_permission_is_optional() {
        let values = BTreeMap::from([
            ("mapping_timeout_ms".to_owned(), "999999".to_owned()),
            ("max_payload_bytes".to_owned(), "999999".to_owned()),
            ("notification_backend".to_owned(), "bell".to_owned()),
            ("debug".to_owned(), "true".to_owned()),
        ]);

        let config = RuntimeConfig::from_zellij(&values);

        assert_eq!(config.mapping_timeout_ms, 2_000);
        assert_eq!(config.max_payload_bytes, MAX_PAYLOAD_BYTES);
        assert!(config.debug);
        assert!(config.needs_run_commands);

        let off = RuntimeConfig::from_zellij(&BTreeMap::from([(
            "notification_backend".to_owned(),
            "off".to_owned(),
        )]));
        assert!(!off.needs_run_commands);
    }

    #[test]
    fn disabled_runtime_ignores_valid_events() {
        let mut runtime = RuntimeState::new(RuntimeConfig {
            enabled: false,
            ..RuntimeConfig::default()
        });

        assert_eq!(
            runtime.ingest(PIPE_NAME, Some(VALID_EVENT), 0),
            IngestOutcome::Dropped(DropReason::Disabled)
        );
    }

    #[test]
    fn normalized_pipe_event_reduces_and_decorates_the_mapped_tab() {
        let mut runtime = mapped_runtime();
        let mut reducer = Reducer::default();
        let mut titles = TitleManager::new(TitleConfig::default());
        assert_eq!(titles.observe_tab(42, "work"), None);

        assert_eq!(
            runtime.ingest(PIPE_NAME, Some(VALID_EVENT), 1_000),
            IngestOutcome::Ready
        );
        let ready = runtime.take_ready_events().pop().expect("mapped event");
        let ApplyOutcome::Applied { transition, .. } = reducer
            .apply(&ready.event)
            .expect("validated event reduces")
        else {
            panic!("event should change reducer state");
        };
        assert_eq!(
            transition.current.as_ref().map(|state| state.state),
            Some(zag_lens_protocol::CanonicalState::WaitingForUser)
        );

        let aggregate = aggregate_states(reducer.agents().map(|agent| agent.state));
        assert_eq!(
            titles.set_aggregate(ready.tab_id, aggregate),
            Some(TitleAction::Rename {
                tab_id: 42,
                title: "? work".to_owned(),
            })
        );
    }

    #[test]
    fn concurrent_codex_and_claude_events_update_only_their_own_tabs() {
        let mut runtime = RuntimeState::default();
        runtime.update_tabs([
            TabRecord {
                position: 1,
                tab_id: 42,
                name: "codex-work".to_owned(),
                active: false,
            },
            TabRecord {
                position: 2,
                tab_id: 84,
                name: "claude-work".to_owned(),
                active: false,
            },
        ]);
        runtime.update_panes([
            PaneRecord {
                pane_id: 3,
                tab_position: 1,
            },
            PaneRecord {
                pane_id: 4,
                tab_position: 2,
            },
        ]);
        let mut reducer = Reducer::default();
        let mut titles = TitleManager::new(TitleConfig::default());
        titles.observe_tab(42, "codex-work");
        titles.observe_tab(84, "claude-work");

        assert_eq!(
            runtime.ingest(PIPE_NAME, Some(VALID_EVENT), 1_000),
            IngestOutcome::Ready
        );
        assert_eq!(
            runtime.ingest(PIPE_NAME, Some(CLAUDE_EVENT), 1_001),
            IngestOutcome::Ready
        );

        let mut actions = Vec::new();
        for ready in runtime.take_ready_events() {
            assert!(matches!(
                reducer.apply(&ready.event).expect("event reduces"),
                ApplyOutcome::Applied { .. }
            ));
            let aggregate = aggregate_states(reducer.agents().filter_map(|agent| {
                (runtime.tab_id_for_protocol_pane(&agent.pane_id) == Some(ready.tab_id))
                    .then_some(agent.state)
            }));
            actions.extend(titles.set_aggregate(ready.tab_id, aggregate));
        }

        assert_eq!(
            actions,
            [
                TitleAction::Rename {
                    tab_id: 42,
                    title: "? codex-work".to_owned(),
                },
                TitleAction::Rename {
                    tab_id: 84,
                    title: "● claude-work".to_owned(),
                },
            ]
        );
    }
}
