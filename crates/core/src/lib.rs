//! Pure state reduction, aggregation, and title formatting.
//!
//! This crate deliberately knows nothing about Zellij APIs or native harness
//! payloads. Callers provide normalized events and an explicit clock, making
//! every transition deterministic and independently testable.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zag_lens_protocol::{AgentIdentity, Attention, CanonicalState, EventKind, NormalizedEvent};

const DEFAULT_SUCCESS_TTL_SECONDS: u64 = 30;
const DEFAULT_STALE_AFTER_SECONDS: u64 = 1_800;
const DEFAULT_MAX_AGENTS: usize = 1_024;
const DEFAULT_DEDUPLICATION_CAPACITY: usize = 4_096;
const DEFAULT_IDENTITY_CURSOR_CAPACITY: usize = 2_048;
const DEFAULT_TITLE_FORMAT: &str = "{icon} {title}";

/// Runtime limits and timer thresholds for [`Reducer`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReducerConfig {
    pub enabled: bool,
    pub success_ttl: Duration,
    pub stale_after: Duration,
    pub max_agents: usize,
    pub deduplication_capacity: usize,
    pub identity_cursor_capacity: usize,
}

impl Default for ReducerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            success_ttl: Duration::from_secs(DEFAULT_SUCCESS_TTL_SECONDS),
            stale_after: Duration::from_secs(DEFAULT_STALE_AFTER_SECONDS),
            max_agents: DEFAULT_MAX_AGENTS,
            deduplication_capacity: DEFAULT_DEDUPLICATION_CAPACITY,
            identity_cursor_capacity: DEFAULT_IDENTITY_CURSOR_CAPACITY,
        }
    }
}

impl ReducerConfig {
    fn normalized(mut self) -> Self {
        self.max_agents = self.max_agents.max(1);
        self.deduplication_capacity = self.deduplication_capacity.max(1);
        self.identity_cursor_capacity = self.identity_cursor_capacity.max(self.max_agents);
        self
    }
}

/// Failure to interpret an event that bypassed protocol parsing.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ReduceError {
    #[error("event occurred_at is not an RFC 3339 timestamp")]
    InvalidOccurredAt,
}

/// Immutable state of one tracked agent instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentSnapshot {
    pub identity: AgentIdentity,
    pub pane_id: String,
    pub state: CanonicalState,
    pub turn_id: Option<String>,
    pub attention: Option<Attention>,
    pub occurred_at_unix_nanos: i128,
}

/// Why a tracked instance changed or was removed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionCause {
    Event(EventKind),
    InactivityTimeout,
    SuccessTtlExpired,
    PaneClosed,
    CapacityEviction,
    ExplicitClear,
}

/// One accepted change, including removals represented by `current: None`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transition {
    pub previous: Option<AgentSnapshot>,
    pub current: Option<AgentSnapshot>,
    pub cause: TransitionCause,
}

impl Transition {
    /// Whether this transition changed the canonical state (or its presence).
    #[must_use]
    pub fn state_changed(&self) -> bool {
        self.previous.as_ref().map(|snapshot| snapshot.state)
            != self.current.as_ref().map(|snapshot| snapshot.state)
    }

    /// Pane IDs whose tab aggregates may need recomputation.
    #[must_use]
    pub fn affected_panes(&self) -> Vec<&str> {
        let mut panes = Vec::with_capacity(2);
        if let Some(previous) = &self.previous {
            panes.push(previous.pane_id.as_str());
        }
        if let Some(current) = &self.current
            && !panes.contains(&current.pane_id.as_str())
        {
            panes.push(current.pane_id.as_str());
        }
        panes
    }
}

/// Result of applying one normalized delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplyOutcome {
    Disabled,
    Duplicate,
    OutOfOrder,
    Applied {
        transition: Box<Transition>,
        evicted: Option<Box<Transition>>,
    },
}

#[derive(Clone, Debug)]
struct EventCursor {
    occurred_at_unix_nanos: i128,
    turn_id: Option<String>,
    kind: EventKind,
}

/// Authoritative, bounded state for all agents in one Zellij session.
#[derive(Debug)]
pub struct Reducer {
    config: ReducerConfig,
    agents: HashMap<AgentIdentity, AgentSnapshot>,
    agent_order: VecDeque<AgentIdentity>,
    recent_event_ids: HashSet<String>,
    event_id_order: VecDeque<String>,
    cursors: HashMap<AgentIdentity, EventCursor>,
    cursor_order: VecDeque<AgentIdentity>,
}

impl Reducer {
    #[must_use]
    pub fn new(config: ReducerConfig) -> Self {
        Self {
            config: config.normalized(),
            agents: HashMap::new(),
            agent_order: VecDeque::new(),
            recent_event_ids: HashSet::new(),
            event_id_order: VecDeque::new(),
            cursors: HashMap::new(),
            cursor_order: VecDeque::new(),
        }
    }

    #[must_use]
    pub fn config(&self) -> &ReducerConfig {
        &self.config
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    pub fn agents(&self) -> impl Iterator<Item = &AgentSnapshot> {
        self.agents.values()
    }

    #[must_use]
    pub fn agent(&self, identity: &AgentIdentity) -> Option<&AgentSnapshot> {
        self.agents.get(identity)
    }

    /// Applies an event after duplicate and ordering checks.
    ///
    /// # Errors
    ///
    /// Returns [`ReduceError::InvalidOccurredAt`] if a caller constructed an
    /// event without passing it through protocol validation.
    pub fn apply(&mut self, event: &NormalizedEvent) -> Result<ApplyOutcome, ReduceError> {
        if !self.config.enabled {
            return Ok(ApplyOutcome::Disabled);
        }

        let occurred_at = OffsetDateTime::parse(&event.occurred_at, &Rfc3339)
            .map_err(|_| ReduceError::InvalidOccurredAt)?
            .unix_timestamp_nanos();

        if self.recent_event_ids.contains(event.deduplication_key()) {
            return Ok(ApplyOutcome::Duplicate);
        }
        self.remember_event_id(event.event_id.clone());

        let identity = event.agent_identity();
        if self
            .cursors
            .get(&identity)
            .is_some_and(|cursor| is_out_of_order(cursor, event, occurred_at))
        {
            return Ok(ApplyOutcome::OutOfOrder);
        }

        self.remember_cursor(
            identity.clone(),
            EventCursor {
                occurred_at_unix_nanos: occurred_at,
                turn_id: event.turn_id.clone(),
                kind: event.kind,
            },
        );

        let previous = self.agents.remove(&identity);
        remove_from_order(&mut self.agent_order, &identity);

        if event.kind == EventKind::SessionEnded {
            return Ok(ApplyOutcome::Applied {
                transition: Box::new(Transition {
                    previous,
                    current: None,
                    cause: TransitionCause::Event(event.kind),
                }),
                evicted: None,
            });
        }

        let current = AgentSnapshot {
            identity: identity.clone(),
            pane_id: event.pane_id.clone(),
            state: event.state,
            turn_id: event.turn_id.clone(),
            attention: event.attention.clone(),
            occurred_at_unix_nanos: occurred_at,
        };
        self.agents.insert(identity.clone(), current.clone());
        self.agent_order.push_back(identity);

        let evicted = self.evict_agent_over_capacity();
        Ok(ApplyOutcome::Applied {
            transition: Box::new(Transition {
                previous,
                current: Some(current),
                cause: TransitionCause::Event(event.kind),
            }),
            evicted: evicted.map(Box::new),
        })
    }

    /// Applies success expiry and inactivity thresholds at an explicit time.
    pub fn advance_time(&mut self, now: OffsetDateTime) -> Vec<Transition> {
        if !self.config.enabled {
            return Vec::new();
        }

        let now = now.unix_timestamp_nanos();
        let success_ttl = duration_nanos(self.config.success_ttl);
        let stale_after = duration_nanos(self.config.stale_after);
        let mut identities: Vec<_> = self.agents.keys().cloned().collect();
        identities.sort_by(compare_identity);
        let mut transitions = Vec::new();

        for identity in identities {
            let Some(previous) = self.agents.get(&identity).cloned() else {
                continue;
            };
            let elapsed = now.saturating_sub(previous.occurred_at_unix_nanos);

            if previous.state == CanonicalState::Succeeded && elapsed >= success_ttl {
                self.agents.remove(&identity);
                remove_from_order(&mut self.agent_order, &identity);
                transitions.push(Transition {
                    previous: Some(previous),
                    current: None,
                    cause: TransitionCause::SuccessTtlExpired,
                });
            } else if is_stale_candidate(previous.state) && elapsed >= stale_after {
                let mut current = previous.clone();
                current.state = CanonicalState::Stale;
                current.attention = None;
                self.agents.insert(identity, current.clone());
                transitions.push(Transition {
                    previous: Some(previous),
                    current: Some(current),
                    cause: TransitionCause::InactivityTimeout,
                });
            }
        }
        transitions
    }

    /// Removes every instance owned by a closed pane.
    pub fn remove_pane(&mut self, pane_id: &str) -> Vec<Transition> {
        let mut identities: Vec<_> = self
            .agents
            .iter()
            .filter(|(_, snapshot)| snapshot.pane_id == pane_id)
            .map(|(identity, _)| identity.clone())
            .collect();
        identities.sort_by(compare_identity);
        identities
            .into_iter()
            .filter_map(|identity| self.remove(&identity, TransitionCause::PaneClosed))
            .collect()
    }

    /// Explicitly clears one tracked instance while retaining its ordering
    /// cursor, so a late older delivery cannot immediately resurrect it.
    pub fn clear(&mut self, identity: &AgentIdentity) -> Option<Transition> {
        self.remove(identity, TransitionCause::ExplicitClear)
    }

    /// Aggregates agents whose current pane belongs to the supplied tab.
    pub fn aggregate_for_panes<'a>(
        &self,
        pane_ids: impl IntoIterator<Item = &'a str>,
    ) -> Option<AggregateStatus> {
        let panes: HashSet<&str> = pane_ids.into_iter().collect();
        aggregate_states(
            self.agents
                .values()
                .filter(|snapshot| panes.contains(snapshot.pane_id.as_str()))
                .map(|snapshot| snapshot.state),
        )
    }

    fn remove(&mut self, identity: &AgentIdentity, cause: TransitionCause) -> Option<Transition> {
        let previous = self.agents.remove(identity)?;
        remove_from_order(&mut self.agent_order, identity);
        Some(Transition {
            previous: Some(previous),
            current: None,
            cause,
        })
    }

    fn evict_agent_over_capacity(&mut self) -> Option<Transition> {
        if self.agents.len() <= self.config.max_agents {
            return None;
        }
        let identity = self.agent_order.pop_front()?;
        let previous = self.agents.remove(&identity)?;
        Some(Transition {
            previous: Some(previous),
            current: None,
            cause: TransitionCause::CapacityEviction,
        })
    }

    fn remember_event_id(&mut self, event_id: String) {
        self.recent_event_ids.insert(event_id.clone());
        self.event_id_order.push_back(event_id);
        while self.event_id_order.len() > self.config.deduplication_capacity {
            if let Some(expired) = self.event_id_order.pop_front() {
                self.recent_event_ids.remove(&expired);
            }
        }
    }

    fn remember_cursor(&mut self, identity: AgentIdentity, cursor: EventCursor) {
        remove_from_order(&mut self.cursor_order, &identity);
        self.cursors.insert(identity.clone(), cursor);
        self.cursor_order.push_back(identity);
        while self.cursor_order.len() > self.config.identity_cursor_capacity {
            if let Some(expired) = self.cursor_order.pop_front() {
                self.cursors.remove(&expired);
            }
        }
    }
}

impl Default for Reducer {
    fn default() -> Self {
        Self::new(ReducerConfig::default())
    }
}

fn is_out_of_order(cursor: &EventCursor, event: &NormalizedEvent, occurred_at: i128) -> bool {
    if cursor.kind == EventKind::TurnCancelled && event.kind == EventKind::TurnCompleted {
        return true;
    }
    if occurred_at != cursor.occurred_at_unix_nanos {
        return occurred_at < cursor.occurred_at_unix_nanos;
    }
    if cursor.kind == EventKind::SessionEnded {
        return true;
    }
    if event.kind == EventKind::SessionEnded {
        return false;
    }

    let different_concrete_turn =
        cursor.turn_id.is_some() && event.turn_id.is_some() && cursor.turn_id != event.turn_id;
    if different_concrete_turn && matches!(event.kind, EventKind::TurnStarted | EventKind::Activity)
    {
        return false;
    }

    event_precedence(event.kind) < event_precedence(cursor.kind)
}

const fn event_precedence(kind: EventKind) -> u8 {
    match kind {
        EventKind::SessionStarted => 0,
        EventKind::TurnStarted => 1,
        EventKind::Activity => 2,
        EventKind::InteractionRequired => 3,
        EventKind::TurnCompleted => 4,
        EventKind::TurnFailed => 5,
        EventKind::TurnCancelled => 6,
        EventKind::SessionEnded => 7,
    }
}

const fn is_stale_candidate(state: CanonicalState) -> bool {
    matches!(
        state,
        CanonicalState::Ready | CanonicalState::Working | CanonicalState::WaitingForUser
    )
}

fn duration_nanos(duration: Duration) -> i128 {
    i128::try_from(duration.as_nanos()).unwrap_or(i128::MAX)
}

fn compare_identity(left: &AgentIdentity, right: &AgentIdentity) -> std::cmp::Ordering {
    (&left.harness, &left.session_id, &left.agent_instance_id).cmp(&(
        &right.harness,
        &right.session_id,
        &right.agent_instance_id,
    ))
}

fn remove_from_order(order: &mut VecDeque<AgentIdentity>, identity: &AgentIdentity) {
    if let Some(index) = order.iter().position(|candidate| candidate == identity) {
        order.remove(index);
    }
}

/// Visible aggregate for a tab and number of agents contributing to it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AggregateStatus {
    pub state: CanonicalState,
    pub count: usize,
}

/// Selects the highest-priority visible state and its same-state count.
#[must_use]
pub fn aggregate_states(
    states: impl IntoIterator<Item = CanonicalState>,
) -> Option<AggregateStatus> {
    let mut selected: Option<AggregateStatus> = None;
    for state in states {
        if !is_visible(state) {
            continue;
        }
        match &mut selected {
            Some(aggregate) if state_priority(state) > state_priority(aggregate.state) => {
                *aggregate = AggregateStatus { state, count: 1 };
            }
            Some(aggregate) if state == aggregate.state => aggregate.count += 1,
            Some(_) => {}
            None => selected = Some(AggregateStatus { state, count: 1 }),
        }
    }
    selected
}

const fn is_visible(state: CanonicalState) -> bool {
    !matches!(state, CanonicalState::Ready | CanonicalState::Stopped)
}

const fn state_priority(state: CanonicalState) -> u8 {
    match state {
        CanonicalState::Ready | CanonicalState::Stopped => 0,
        CanonicalState::Stale => 1,
        CanonicalState::Succeeded => 2,
        CanonicalState::Working => 3,
        CanonicalState::Failed => 4,
        CanonicalState::WaitingForUser => 5,
    }
}

/// Built-in status glyphs, with configurable per-state overrides.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Icons {
    pub working: String,
    pub waiting_for_user: String,
    pub succeeded: String,
    pub failed: String,
    pub stale: String,
}

impl Icons {
    #[must_use]
    pub fn unicode() -> Self {
        Self {
            working: "●".into(),
            waiting_for_user: "?".into(),
            succeeded: "✓".into(),
            failed: "×".into(),
            stale: "!".into(),
        }
    }

    #[must_use]
    pub fn ascii() -> Self {
        Self {
            working: "*".into(),
            waiting_for_user: "?".into(),
            succeeded: "+".into(),
            failed: "x".into(),
            stale: "!".into(),
        }
    }

    #[must_use]
    pub fn for_state(&self, state: CanonicalState) -> Option<&str> {
        match state {
            CanonicalState::Working => Some(&self.working),
            CanonicalState::WaitingForUser => Some(&self.waiting_for_user),
            CanonicalState::Succeeded => Some(&self.succeeded),
            CanonicalState::Failed => Some(&self.failed),
            CanonicalState::Stale => Some(&self.stale),
            CanonicalState::Ready | CanonicalState::Stopped => None,
        }
    }
}

impl Default for Icons {
    fn default() -> Self {
        Self::unicode()
    }
}

/// Pure title-rendering configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleConfig {
    pub format: String,
    pub icons: Icons,
    pub show_counts: bool,
}

impl Default for TitleConfig {
    fn default() -> Self {
        Self {
            format: DEFAULT_TITLE_FORMAT.into(),
            icons: Icons::default(),
            show_counts: false,
        }
    }
}

impl TitleConfig {
    /// Restores the safe default if a configured format could discard the tab
    /// title or cannot display an icon.
    #[must_use]
    pub fn with_safe_format(mut self) -> Self {
        if !self.format.contains("{title}") || !self.format.contains("{icon}") {
            self.format = DEFAULT_TITLE_FORMAT.into();
        }
        self
    }

    /// Renders a base title once. Invisible or absent states return it exactly.
    #[must_use]
    pub fn render(&self, base_title: &str, aggregate: Option<AggregateStatus>) -> String {
        let Some(aggregate) = aggregate else {
            return base_title.to_owned();
        };
        let Some(icon) = self.icons.for_state(aggregate.state) else {
            return base_title.to_owned();
        };
        let rendered_icon = if self.show_counts && aggregate.count > 1 {
            format!("{icon}{}", aggregate.count)
        } else {
            icon.to_owned()
        };
        let format = if self.format.contains("{title}") && self.format.contains("{icon}") {
            self.format.as_str()
        } else {
            DEFAULT_TITLE_FORMAT
        };
        format
            .replace("{icon}", &rendered_icon)
            .replace("{title}", base_title)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use time::Duration as TimeDuration;
    use zag_lens_protocol::{AdapterInfo, SCHEMA_VERSION};

    use super::*;

    static EVENT_SEQUENCE: AtomicUsize = AtomicUsize::new(1);

    fn event(kind: EventKind, at: &str) -> NormalizedEvent {
        let sequence = EVENT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        NormalizedEvent {
            schema_version: SCHEMA_VERSION,
            event_id: format!("00000000-0000-4000-8000-{sequence:012x}"),
            occurred_at: at.into(),
            harness: "codex".into(),
            native_event: kind.as_str().into(),
            kind,
            state: kind.canonical_state(),
            session_id: "session-1".into(),
            agent_instance_id: "agent-1".into(),
            turn_id: Some("turn-1".into()),
            pane_id: "pane-1".into(),
            zellij_session: Some("dev".into()),
            cwd: None,
            attention: (kind == EventKind::InteractionRequired).then(|| Attention {
                kind: "permission".into(),
                summary: None,
            }),
            adapter: AdapterInfo {
                name: "test".into(),
                version: 1,
            },
        }
    }

    fn applied(outcome: ApplyOutcome) -> Transition {
        match outcome {
            ApplyOutcome::Applied { transition, .. } => *transition,
            other => panic!("expected applied, got {other:?}"),
        }
    }

    fn at(value: &str) -> OffsetDateTime {
        OffsetDateTime::parse(value, &Rfc3339).unwrap()
    }

    #[test]
    fn lifecycle_events_transition_through_every_canonical_state() {
        let mut reducer = Reducer::default();
        let cases = [
            (EventKind::SessionStarted, CanonicalState::Ready),
            (EventKind::TurnStarted, CanonicalState::Working),
            (EventKind::Activity, CanonicalState::Working),
            (
                EventKind::InteractionRequired,
                CanonicalState::WaitingForUser,
            ),
            (EventKind::TurnCompleted, CanonicalState::Succeeded),
            (EventKind::TurnFailed, CanonicalState::Failed),
            (EventKind::TurnCancelled, CanonicalState::Stopped),
        ];

        for (second, (kind, state)) in cases.into_iter().enumerate() {
            let timestamp = format!("2026-07-13T12:00:{second:02}Z");
            applied(reducer.apply(&event(kind, &timestamp)).unwrap());
            assert_eq!(
                reducer.agents().next().map(|snapshot| snapshot.state),
                Some(state)
            );
        }
    }

    #[test]
    fn duplicate_event_id_is_ignored_without_refreshing_state() {
        let mut reducer = Reducer::default();
        let first = event(EventKind::TurnStarted, "2026-07-13T12:00:00Z");
        applied(reducer.apply(&first).unwrap());
        assert_eq!(reducer.apply(&first).unwrap(), ApplyOutcome::Duplicate);
        assert_eq!(reducer.len(), 1);
    }

    #[test]
    fn bounded_event_ids_allow_an_expired_id_again() {
        let mut reducer = Reducer::new(ReducerConfig {
            deduplication_capacity: 1,
            ..ReducerConfig::default()
        });
        let first = event(EventKind::TurnStarted, "2026-07-13T12:00:00Z");
        let second = event(EventKind::Activity, "2026-07-13T12:00:01Z");
        applied(reducer.apply(&first).unwrap());
        applied(reducer.apply(&second).unwrap());

        let mut replay = first;
        replay.occurred_at = "2026-07-13T12:00:02Z".into();
        assert!(matches!(
            reducer.apply(&replay).unwrap(),
            ApplyOutcome::Applied { .. }
        ));
    }

    #[test]
    fn older_activity_does_not_regress_a_completion() {
        let mut reducer = Reducer::default();
        applied(
            reducer
                .apply(&event(EventKind::TurnCompleted, "2026-07-13T12:00:02Z"))
                .unwrap(),
        );
        assert_eq!(
            reducer
                .apply(&event(EventKind::Activity, "2026-07-13T12:00:01Z"))
                .unwrap(),
            ApplyOutcome::OutOfOrder
        );
        assert_eq!(
            reducer.agents().next().unwrap().state,
            CanonicalState::Succeeded
        );
    }

    #[test]
    fn same_timestamp_lifecycle_precedence_prevents_regression() {
        let mut reducer = Reducer::default();
        applied(
            reducer
                .apply(&event(
                    EventKind::InteractionRequired,
                    "2026-07-13T12:00:00Z",
                ))
                .unwrap(),
        );
        assert_eq!(
            reducer
                .apply(&event(EventKind::Activity, "2026-07-13T12:00:00Z"))
                .unwrap(),
            ApplyOutcome::OutOfOrder
        );
    }

    #[test]
    fn a_new_turn_at_the_same_timestamp_clears_terminal_state() {
        let mut reducer = Reducer::default();
        applied(
            reducer
                .apply(&event(EventKind::TurnCompleted, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );
        let mut next = event(EventKind::TurnStarted, "2026-07-13T12:00:00Z");
        next.turn_id = Some("turn-2".into());
        applied(reducer.apply(&next).unwrap());
        assert_eq!(
            reducer.agents().next().unwrap().state,
            CanonicalState::Working
        );
    }

    #[test]
    fn concrete_activity_clears_waiting_for_user() {
        let mut reducer = Reducer::default();
        applied(
            reducer
                .apply(&event(
                    EventKind::InteractionRequired,
                    "2026-07-13T12:00:00Z",
                ))
                .unwrap(),
        );
        let transition = applied(
            reducer
                .apply(&event(EventKind::Activity, "2026-07-13T12:00:01Z"))
                .unwrap(),
        );
        assert!(transition.state_changed());
        assert_eq!(transition.current.unwrap().state, CanonicalState::Working);
    }

    #[test]
    fn new_activity_clears_every_resumable_terminal_state() {
        for terminal_kind in [
            EventKind::TurnCompleted,
            EventKind::TurnFailed,
            EventKind::TurnCancelled,
        ] {
            let mut reducer = Reducer::default();
            applied(
                reducer
                    .apply(&event(terminal_kind, "2026-07-13T12:00:00Z"))
                    .unwrap(),
            );
            let transition = applied(
                reducer
                    .apply(&event(EventKind::Activity, "2026-07-13T12:00:01Z"))
                    .unwrap(),
            );
            assert_eq!(transition.current.unwrap().state, CanonicalState::Working);
        }

        let mut stale = Reducer::new(ReducerConfig {
            stale_after: Duration::from_secs(1),
            ..ReducerConfig::default()
        });
        applied(
            stale
                .apply(&event(EventKind::TurnStarted, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );
        stale.advance_time(at("2026-07-13T12:00:01Z"));
        let transition = applied(
            stale
                .apply(&event(EventKind::Activity, "2026-07-13T12:00:02Z"))
                .unwrap(),
        );
        assert_eq!(transition.current.unwrap().state, CanonicalState::Working);
    }

    #[test]
    fn cancelled_turn_suppresses_trailing_completion_until_work_resumes() {
        let mut reducer = Reducer::default();
        applied(
            reducer
                .apply(&event(EventKind::TurnCancelled, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );
        assert_eq!(
            reducer
                .apply(&event(EventKind::TurnCompleted, "2026-07-13T12:00:01Z"))
                .unwrap(),
            ApplyOutcome::OutOfOrder
        );
        assert_eq!(
            reducer.agents().next().unwrap().state,
            CanonicalState::Stopped
        );

        applied(
            reducer
                .apply(&event(EventKind::TurnStarted, "2026-07-13T12:00:02Z"))
                .unwrap(),
        );
        applied(
            reducer
                .apply(&event(EventKind::TurnCompleted, "2026-07-13T12:00:03Z"))
                .unwrap(),
        );
        assert_eq!(
            reducer.agents().next().unwrap().state,
            CanonicalState::Succeeded
        );
    }

    #[test]
    fn same_state_activity_refreshes_time_without_a_state_change() {
        let mut reducer = Reducer::default();
        applied(
            reducer
                .apply(&event(EventKind::TurnStarted, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );
        let transition = applied(
            reducer
                .apply(&event(EventKind::Activity, "2026-07-13T12:00:01Z"))
                .unwrap(),
        );
        assert!(!transition.state_changed());
        assert!(transition.current.unwrap().occurred_at_unix_nanos > 0);
    }

    #[test]
    fn newer_event_moves_ownership_to_a_different_pane() {
        let mut reducer = Reducer::default();
        applied(
            reducer
                .apply(&event(EventKind::TurnStarted, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );
        let mut moved = event(EventKind::Activity, "2026-07-13T12:00:01Z");
        moved.pane_id = "pane-2".into();
        let transition = applied(reducer.apply(&moved).unwrap());
        assert_eq!(transition.affected_panes(), vec!["pane-1", "pane-2"]);
        assert_eq!(transition.current.unwrap().pane_id, "pane-2");
    }

    #[test]
    fn session_end_removes_instance_and_rejects_late_activity() {
        let mut reducer = Reducer::default();
        applied(
            reducer
                .apply(&event(EventKind::TurnStarted, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );
        let ended = applied(
            reducer
                .apply(&event(EventKind::SessionEnded, "2026-07-13T12:00:02Z"))
                .unwrap(),
        );
        assert!(ended.current.is_none());
        assert!(reducer.is_empty());
        assert_eq!(
            reducer
                .apply(&event(EventKind::Activity, "2026-07-13T12:00:01Z"))
                .unwrap(),
            ApplyOutcome::OutOfOrder
        );
    }

    #[test]
    fn success_expires_while_failure_persists() {
        let config = ReducerConfig {
            success_ttl: Duration::from_secs(30),
            ..ReducerConfig::default()
        };
        let mut success = Reducer::new(config.clone());
        applied(
            success
                .apply(&event(EventKind::TurnCompleted, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );
        let transitions = success.advance_time(at("2026-07-13T12:00:30Z"));
        assert_eq!(transitions[0].cause, TransitionCause::SuccessTtlExpired);
        assert!(success.is_empty());

        let mut failure = Reducer::new(config);
        applied(
            failure
                .apply(&event(EventKind::TurnFailed, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );
        assert!(failure.advance_time(at("2026-07-14T12:00:00Z")).is_empty());
        assert_eq!(failure.len(), 1);
    }

    #[test]
    fn inactive_non_terminal_state_becomes_stale_once() {
        let mut reducer = Reducer::new(ReducerConfig {
            stale_after: Duration::from_secs(10),
            ..ReducerConfig::default()
        });
        applied(
            reducer
                .apply(&event(EventKind::TurnStarted, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );
        assert!(reducer.advance_time(at("2026-07-13T12:00:09Z")).is_empty());
        let transitions = reducer.advance_time(at("2026-07-13T12:00:10Z"));
        assert_eq!(transitions[0].cause, TransitionCause::InactivityTimeout);
        assert_eq!(
            transitions[0].current.as_ref().unwrap().state,
            CanonicalState::Stale
        );
        assert!(reducer.advance_time(at("2026-07-13T12:00:11Z")).is_empty());
    }

    #[test]
    fn pane_closure_clears_only_owned_instances_deterministically() {
        let mut reducer = Reducer::default();
        for (agent, pane) in [("z", "pane-1"), ("a", "pane-1"), ("m", "pane-2")] {
            let mut input = event(EventKind::TurnStarted, "2026-07-13T12:00:00Z");
            input.agent_instance_id = agent.into();
            input.pane_id = pane.into();
            applied(reducer.apply(&input).unwrap());
        }
        let removed = reducer.remove_pane("pane-1");
        assert_eq!(removed.len(), 2);
        assert_eq!(
            removed[0]
                .previous
                .as_ref()
                .unwrap()
                .identity
                .agent_instance_id,
            "a"
        );
        assert_eq!(reducer.len(), 1);
    }

    #[test]
    fn agent_storage_evicts_least_recently_updated_instance() {
        let mut reducer = Reducer::new(ReducerConfig {
            max_agents: 2,
            ..ReducerConfig::default()
        });
        for (index, agent) in ["a", "b"].into_iter().enumerate() {
            let mut input = event(
                EventKind::TurnStarted,
                &format!("2026-07-13T12:00:0{index}Z"),
            );
            input.agent_instance_id = agent.into();
            applied(reducer.apply(&input).unwrap());
        }
        let mut refresh = event(EventKind::Activity, "2026-07-13T12:00:02Z");
        refresh.agent_instance_id = "a".into();
        applied(reducer.apply(&refresh).unwrap());
        let mut third = event(EventKind::TurnStarted, "2026-07-13T12:00:03Z");
        third.agent_instance_id = "c".into();
        let outcome = reducer.apply(&third).unwrap();
        let ApplyOutcome::Applied {
            evicted: Some(evicted),
            ..
        } = outcome
        else {
            panic!("expected capacity eviction");
        };
        assert_eq!(evicted.previous.unwrap().identity.agent_instance_id, "b");
    }

    #[test]
    fn load_profile_tracks_one_hundred_agents_at_twenty_events_per_second() {
        let mut reducer = Reducer::default();
        let started = Instant::now();

        for agent in 0..100 {
            let mut input = event(EventKind::TurnStarted, "2026-07-13T12:00:00Z");
            input.session_id = format!("session-{agent}");
            input.agent_instance_id = format!("agent-{agent}");
            input.pane_id = format!("terminal-{}", agent % 10);
            applied(reducer.apply(&input).expect("initial event reduces"));
        }

        // Model ten sustained seconds at the specified minimum throughput.
        for second in 1..=10 {
            let timestamp = format!("2026-07-13T12:00:{second:02}Z");
            for event_index in 0..20 {
                let agent = ((second - 1) * 20 + event_index) % 100;
                let mut input = event(EventKind::Activity, &timestamp);
                input.session_id = format!("session-{agent}");
                input.agent_instance_id = format!("agent-{agent}");
                input.pane_id = format!("terminal-{}", agent % 10);
                applied(reducer.apply(&input).expect("load event reduces"));
            }
        }

        assert_eq!(reducer.len(), 100);
        assert_eq!(
            aggregate_states(reducer.agents().map(|agent| agent.state)),
            Some(AggregateStatus {
                state: CanonicalState::Working,
                count: 100,
            })
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "pure reducer load profile exceeded two seconds"
        );
    }

    #[test]
    fn aggregate_priority_and_same_state_count_are_deterministic() {
        let aggregate = aggregate_states([
            CanonicalState::Ready,
            CanonicalState::Stale,
            CanonicalState::Succeeded,
            CanonicalState::Working,
            CanonicalState::Failed,
            CanonicalState::WaitingForUser,
            CanonicalState::WaitingForUser,
            CanonicalState::Stopped,
        ]);
        assert_eq!(
            aggregate,
            Some(AggregateStatus {
                state: CanonicalState::WaitingForUser,
                count: 2,
            })
        );
    }

    #[test]
    fn every_aggregate_pair_respects_priority_in_both_orders() {
        let states = [
            CanonicalState::Ready,
            CanonicalState::Stopped,
            CanonicalState::Stale,
            CanonicalState::Succeeded,
            CanonicalState::Working,
            CanonicalState::Failed,
            CanonicalState::WaitingForUser,
        ];
        for left in states {
            for right in states {
                let expected_state = [left, right]
                    .into_iter()
                    .filter(|state| is_visible(*state))
                    .max_by_key(|state| state_priority(*state));
                let expected = expected_state.map(|state| AggregateStatus {
                    state,
                    count: usize::from(left == right)
                        + usize::from(left != right || is_visible(left)),
                });
                assert_eq!(aggregate_states([left, right]), expected);
                assert_eq!(aggregate_states([right, left]), expected);
            }
        }
    }

    #[test]
    fn aggregate_for_panes_excludes_agents_in_other_tabs() {
        let mut reducer = Reducer::default();
        let mut first = event(EventKind::TurnStarted, "2026-07-13T12:00:00Z");
        first.agent_instance_id = "first".into();
        applied(reducer.apply(&first).unwrap());
        let mut second = event(EventKind::InteractionRequired, "2026-07-13T12:00:01Z");
        second.agent_instance_id = "second".into();
        second.pane_id = "pane-2".into();
        applied(reducer.apply(&second).unwrap());
        assert_eq!(
            reducer.aggregate_for_panes(["pane-1"]),
            Some(AggregateStatus {
                state: CanonicalState::Working,
                count: 1,
            })
        );
    }

    #[test]
    fn default_title_format_uses_unicode_icons_without_counts() {
        let title = TitleConfig::default().render(
            "api-refactor",
            Some(AggregateStatus {
                state: CanonicalState::Working,
                count: 2,
            }),
        );
        assert_eq!(title, "● api-refactor");
    }

    #[test]
    fn count_and_ascii_icon_render_in_the_configured_format() {
        let config = TitleConfig {
            format: "[{icon}] {title}".into(),
            icons: Icons::ascii(),
            show_counts: true,
        };
        assert_eq!(
            config.render(
                "review",
                Some(AggregateStatus {
                    state: CanonicalState::WaitingForUser,
                    count: 2,
                })
            ),
            "[?2] review"
        );
    }

    #[test]
    fn invisible_state_preserves_base_title_exactly() {
        assert_eq!(
            TitleConfig::default().render(
                "  custom title  ",
                Some(AggregateStatus {
                    state: CanonicalState::Ready,
                    count: 1,
                })
            ),
            "  custom title  "
        );
    }

    #[test]
    fn unsafe_title_format_falls_back_to_default() {
        let config = TitleConfig {
            format: "{icon}".into(),
            ..TitleConfig::default()
        }
        .with_safe_format();
        assert_eq!(config.format, DEFAULT_TITLE_FORMAT);

        let unsanitized = TitleConfig {
            format: "{icon}".into(),
            ..TitleConfig::default()
        };
        assert_eq!(
            unsanitized.render(
                "safe base",
                Some(AggregateStatus {
                    state: CanonicalState::Working,
                    count: 1,
                })
            ),
            "● safe base"
        );
    }

    #[test]
    fn disabled_reducer_does_not_record_or_apply_events() {
        let mut reducer = Reducer::new(ReducerConfig {
            enabled: false,
            ..ReducerConfig::default()
        });
        let input = event(EventKind::TurnStarted, "2026-07-13T12:00:00Z");
        assert_eq!(reducer.apply(&input).unwrap(), ApplyOutcome::Disabled);
        assert!(reducer.is_empty());
    }

    #[test]
    fn malformed_timestamp_is_rejected_without_mutating_state() {
        let mut reducer = Reducer::default();
        let mut input = event(EventKind::TurnStarted, "2026-07-13T12:00:00Z");
        input.occurred_at = "not-a-time".into();
        assert_eq!(reducer.apply(&input), Err(ReduceError::InvalidOccurredAt));
        assert!(reducer.is_empty());
    }

    #[test]
    fn timers_before_last_event_do_not_change_state() {
        let mut reducer = Reducer::new(ReducerConfig {
            stale_after: Duration::ZERO,
            ..ReducerConfig::default()
        });
        applied(
            reducer
                .apply(&event(EventKind::TurnStarted, "2026-07-13T12:00:10Z"))
                .unwrap(),
        );
        assert!(reducer.advance_time(at("2026-07-13T12:00:09Z")).is_empty());
        assert_eq!(
            reducer.agents().next().unwrap().state,
            CanonicalState::Working
        );
    }

    #[test]
    fn threshold_arithmetic_handles_large_durations() {
        assert_eq!(
            duration_nanos(Duration::MAX),
            i128::try_from(Duration::MAX.as_nanos()).unwrap()
        );
        let now = at("2026-07-13T12:00:00Z");
        assert_eq!(now + TimeDuration::ZERO, now);
    }
}
