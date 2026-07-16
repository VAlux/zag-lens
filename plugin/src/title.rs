//! Pure ownership controller for Zellij tab-title mutations.
//!
//! The controller does not call Zellij directly. It compares observed tab
//! names with the names it owns and emits explicit, coalesced actions for the
//! runtime to execute.

use std::collections::HashMap;
use std::fmt::Write as _;

use zag_lens_core::{AggregateStatus, TitleConfig};
use zag_lens_protocol::CanonicalState;

const JOURNAL_HEADER_V1: &str = "zag-lens-title-journal-v1";
const JOURNAL_HEADER_V2: &str = "zag-lens-title-journal-v2";

/// A tab-title mutation for the Zellij runtime to execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleAction {
    /// Apply or refresh a visible Zag Lens decoration.
    Rename { tab_id: u64, title: String },
    /// Remove Zag Lens decoration and put the current base title back.
    Restore { tab_id: u64, title: String },
}

impl TitleAction {
    /// Stable Zellij tab ID targeted by this action.
    #[must_use]
    pub const fn tab_id(&self) -> u64 {
        match self {
            Self::Rename { tab_id, .. } | Self::Restore { tab_id, .. } => *tab_id,
        }
    }

    /// Exact title the runtime should pass to Zellij.
    #[must_use]
    pub fn title(&self) -> &str {
        match self {
            Self::Rename { title, .. } | Self::Restore { title, .. } => title,
        }
    }
}

/// Observable title ownership retained for one stable tab ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedTitle {
    base_title: String,
    last_rendered_title: String,
    last_observed_title: String,
    aggregate: Option<AggregateStatus>,
    frame_index: usize,
    frame_acknowledged_at_ms: Option<u64>,
    possible_titles: Vec<String>,
    rename_in_flight: bool,
    remove_after_ack: bool,
    recovery_pending: bool,
}

impl ManagedTitle {
    /// User-owned title without Zag Lens decoration.
    #[must_use]
    pub fn base_title(&self) -> &str {
        &self.base_title
    }

    /// Last title requested by the controller.
    #[must_use]
    pub fn last_rendered_title(&self) -> &str {
        &self.last_rendered_title
    }

    /// Aggregate that currently determines the decoration.
    #[must_use]
    pub const fn aggregate(&self) -> Option<AggregateStatus> {
        self.aggregate
    }

    /// Whether an emitted action has not yet appeared in a `TabUpdate`.
    #[must_use]
    pub const fn rename_in_flight(&self) -> bool {
        self.rename_in_flight
    }

    /// Current per-tab animation phase.
    #[must_use]
    pub const fn frame_index(&self) -> usize {
        self.frame_index
    }
}

/// Pure controller for base-title ownership, decoration, and restoration.
#[derive(Clone, Debug)]
pub struct TitleManager {
    config: TitleConfig,
    tabs: HashMap<u64, ManagedTitle>,
    journal_dirty: bool,
}

impl TitleManager {
    /// Creates an empty controller using a safe title format.
    #[must_use]
    pub fn new(config: TitleConfig) -> Self {
        Self {
            config: config.with_safe_format(),
            tabs: HashMap::new(),
            journal_dirty: false,
        }
    }

    /// Reconstructs title ownership from a best-effort journal snapshot.
    ///
    /// Journal entries never revive agent state. On the next observation, an
    /// exact journaled rendered title is restored to its journaled base title;
    /// any other title is treated as an external rename and left untouched.
    #[must_use]
    pub fn from_journal(config: TitleConfig, journal: &str) -> Self {
        let mut manager = Self::new(config);
        let mut lines = journal.lines();
        let header = lines.next();
        if !matches!(header, Some(JOURNAL_HEADER_V1 | JOURNAL_HEADER_V2)) {
            return manager;
        }

        for line in lines {
            let parsed = match header {
                Some(JOURNAL_HEADER_V1) => parse_journal_v1_line(line),
                Some(JOURNAL_HEADER_V2) => parse_journal_v2_line(line),
                _ => None,
            };
            if let Some((tab_id, managed)) = parsed {
                manager.tabs.insert(tab_id, managed);
            }
        }
        manager
    }

    /// Returns the retained ownership state for a tab.
    #[must_use]
    pub fn managed(&self, tab_id: u64) -> Option<&ManagedTitle> {
        self.tabs.get(&tab_id)
    }

    /// Observes a title from `TabUpdate` and returns any required correction.
    ///
    /// Call this before [`Self::set_aggregate`] when first managing a tab.
    pub fn observe_tab(&mut self, tab_id: u64, observed_title: &str) -> Option<TitleAction> {
        self.observe_tab_at(tab_id, observed_title, 0)
    }

    /// Time-aware form of [`Self::observe_tab`] used for animation timing.
    pub fn observe_tab_at(
        &mut self,
        tab_id: u64,
        observed_title: &str,
        now_ms: u64,
    ) -> Option<TitleAction> {
        let Some(managed) = self.tabs.get_mut(&tab_id) else {
            self.tabs.insert(
                tab_id,
                ManagedTitle {
                    base_title: observed_title.to_owned(),
                    last_rendered_title: observed_title.to_owned(),
                    last_observed_title: observed_title.to_owned(),
                    aggregate: None,
                    frame_index: 0,
                    frame_acknowledged_at_ms: None,
                    possible_titles: vec![observed_title.to_owned()],
                    rename_in_flight: false,
                    remove_after_ack: false,
                    recovery_pending: false,
                },
            );
            self.journal_dirty = true;
            return None;
        };

        if managed.recovery_pending {
            managed.recovery_pending = false;
            if managed
                .possible_titles
                .iter()
                .any(|title| title == observed_title)
                && observed_title != managed.base_title
            {
                managed.aggregate = None;
                observed_title.clone_into(&mut managed.last_observed_title);
                managed.remove_after_ack = true;
                return Some(emit_action(tab_id, managed, managed.base_title.clone()));
            }

            // A non-matching name cannot safely be identified as ours.
            self.tabs.remove(&tab_id);
            self.journal_dirty = true;
            return None;
        }

        if observed_title == managed.last_rendered_title {
            observed_title.clone_into(&mut managed.last_observed_title);
            managed.rename_in_flight = false;
            managed.frame_acknowledged_at_ms = Some(now_ms);
            if managed.remove_after_ack {
                self.tabs.remove(&tab_id);
                self.journal_dirty = true;
                return None;
            }
            let desired = self.config.render_frame(
                &managed.base_title,
                managed.aggregate,
                managed.frame_index,
            );
            return (desired != managed.last_rendered_title)
                .then(|| emit_action(tab_id, managed, desired));
        }

        // Every exact title in the current sequence is plugin-owned. This
        // handles delayed frame observations without accepting them as renames.
        if managed
            .possible_titles
            .iter()
            .any(|title| title == observed_title)
        {
            observed_title.clone_into(&mut managed.last_observed_title);
            if managed.rename_in_flight {
                return None;
            }
            let desired = self.config.render_frame(
                &managed.base_title,
                managed.aggregate,
                managed.frame_index,
            );
            return (desired != observed_title).then(|| emit_action(tab_id, managed, desired));
        }

        // Ignore a repeated stale observation while a mutation is in flight.
        if managed.rename_in_flight && observed_title == managed.last_observed_title {
            return None;
        }

        let base_title = strip_exact_frame(
            &self.config,
            observed_title,
            managed.aggregate,
            managed.frame_index,
        );
        let base_changed = base_title != managed.base_title;
        base_title.clone_into(&mut managed.base_title);
        observed_title.clone_into(&mut managed.last_observed_title);
        managed.remove_after_ack = false;
        managed.possible_titles = self
            .config
            .possible_titles(&managed.base_title, managed.aggregate);
        self.journal_dirty |= base_changed;

        let desired =
            self.config
                .render_frame(&managed.base_title, managed.aggregate, managed.frame_index);
        if desired == observed_title {
            managed.last_rendered_title = desired;
            managed.rename_in_flight = false;
            managed.frame_acknowledged_at_ms = Some(now_ms);
            None
        } else if managed.rename_in_flight {
            None
        } else {
            Some(emit_action(tab_id, managed, desired))
        }
    }

    /// Updates one tab's aggregate and emits at most one title mutation.
    ///
    /// An absent aggregate clears title ownership after restoration is
    /// acknowledged. If the tab has not been observed yet, this is a no-op.
    pub fn set_aggregate(
        &mut self,
        tab_id: u64,
        aggregate: Option<AggregateStatus>,
    ) -> Option<TitleAction> {
        self.set_aggregate_at(tab_id, aggregate, 0)
    }

    /// Time-aware form of [`Self::set_aggregate`] used by the runtime.
    pub fn set_aggregate_at(
        &mut self,
        tab_id: u64,
        aggregate: Option<AggregateStatus>,
        now_ms: u64,
    ) -> Option<TitleAction> {
        let managed = self.tabs.get_mut(&tab_id)?;
        managed.recovery_pending = false;
        let previous = managed.aggregate;
        let state_changed = previous.map(|value| value.state) != aggregate.map(|value| value.state);
        if state_changed {
            managed.frame_index = 0;
            managed.frame_acknowledged_at_ms = None;
        }
        managed.aggregate = aggregate;
        managed.remove_after_ack = aggregate.is_none();
        managed.possible_titles = self.config.possible_titles(&managed.base_title, aggregate);
        if previous != aggregate {
            self.journal_dirty = true;
        }
        let desired = self
            .config
            .render_frame(&managed.base_title, aggregate, managed.frame_index);

        if desired == managed.last_rendered_title {
            if !managed.rename_in_flight {
                managed.frame_acknowledged_at_ms.get_or_insert(now_ms);
            }
            if managed.remove_after_ack && !managed.rename_in_flight {
                self.tabs.remove(&tab_id);
                self.journal_dirty = true;
            }
            return None;
        }

        if managed.rename_in_flight {
            return None;
        }

        Some(emit_action(tab_id, managed, desired))
    }

    /// Advances each acknowledged animated tab by exactly one frame.
    pub fn advance_animations(&mut self, now_ms: u64, interval_ms: u64) -> Vec<TitleAction> {
        let mut tab_ids: Vec<_> = self.tabs.keys().copied().collect();
        tab_ids.sort_unstable();
        let mut actions = Vec::new();
        for tab_id in tab_ids {
            let Some(managed) = self.tabs.get_mut(&tab_id) else {
                continue;
            };
            let frame_count = self.config.frame_count(managed.aggregate);
            let ready = !managed.rename_in_flight
                && frame_count > 1
                && managed
                    .frame_acknowledged_at_ms
                    .is_some_and(|acknowledged| now_ms.saturating_sub(acknowledged) >= interval_ms);
            if !ready {
                continue;
            }
            managed.frame_index = (managed.frame_index + 1) % frame_count;
            managed.frame_acknowledged_at_ms = None;
            let desired = self.config.render_frame(
                &managed.base_title,
                managed.aggregate,
                managed.frame_index,
            );
            actions.push(emit_action(tab_id, managed, desired));
        }
        actions
    }

    /// Whether at least one managed tab currently has multiple icon frames.
    #[must_use]
    pub fn has_active_animation(&self) -> bool {
        self.tabs.values().any(|managed| {
            !managed.remove_after_ack && self.config.frame_count(managed.aggregate) > 1
        })
    }

    /// Forgets a tab that no longer exists without attempting restoration.
    pub fn remove_tab(&mut self, tab_id: u64) {
        if self.tabs.remove(&tab_id).is_some() {
            self.journal_dirty = true;
        }
    }

    /// Emits deterministic base-title restorations for normal plugin shutdown.
    pub fn shutdown_actions(&mut self) -> Vec<TitleAction> {
        let mut tab_ids: Vec<_> = self.tabs.keys().copied().collect();
        tab_ids.sort_unstable();
        let mut actions = Vec::new();

        for tab_id in tab_ids {
            let Some(managed) = self.tabs.get_mut(&tab_id) else {
                continue;
            };
            managed.aggregate = None;
            managed.remove_after_ack = true;
            if managed.last_rendered_title != managed.base_title {
                let base_title = managed.base_title.clone();
                actions.push(emit_action(tab_id, managed, base_title));
            }
        }
        if !self.tabs.is_empty() {
            self.journal_dirty = true;
        }
        actions
    }

    /// Serializes a small, versioned, dependency-free recovery journal.
    #[must_use]
    pub fn journal_snapshot(&self) -> String {
        let mut tab_ids: Vec<_> = self.tabs.keys().copied().collect();
        tab_ids.sort_unstable();
        let mut output = String::from(JOURNAL_HEADER_V2);
        output.push('\n');

        for tab_id in tab_ids {
            let managed = &self.tabs[&tab_id];
            let (state, count) = managed.aggregate.map_or(("-", 0), |aggregate| {
                (aggregate.state.as_str(), aggregate.count)
            });
            writeln!(
                output,
                "{tab_id}\t{state}\t{count}\t{}\t{}",
                hex_encode(&managed.base_title),
                managed
                    .possible_titles
                    .iter()
                    .map(|title| hex_encode(title))
                    .collect::<Vec<_>>()
                    .join(","),
            )
            .expect("writing to String cannot fail");
        }
        output
    }

    /// Returns a journal snapshot only after durable ownership metadata changed.
    pub fn take_dirty_journal_snapshot(&mut self) -> Option<String> {
        if !self.journal_dirty {
            return None;
        }
        self.journal_dirty = false;
        Some(self.journal_snapshot())
    }

    /// Schedules another persistence attempt after an I/O failure.
    pub fn mark_journal_dirty(&mut self) {
        self.journal_dirty = true;
    }
}

impl Default for TitleManager {
    fn default() -> Self {
        Self::new(TitleConfig::default())
    }
}

fn emit_action(tab_id: u64, managed: &mut ManagedTitle, title: String) -> TitleAction {
    managed.last_rendered_title.clone_from(&title);
    managed.rename_in_flight = true;
    if title == managed.base_title {
        TitleAction::Restore { tab_id, title }
    } else {
        TitleAction::Rename { tab_id, title }
    }
}

fn strip_exact_frame<'a>(
    config: &TitleConfig,
    observed_title: &'a str,
    aggregate: Option<AggregateStatus>,
    frame_index: usize,
) -> &'a str {
    let marker = "\u{0}";
    let rendered_marker = config.render_frame(marker, aggregate, frame_index);
    let Some((prefix, suffix)) = rendered_marker.split_once(marker) else {
        return observed_title;
    };
    if (prefix.is_empty() && suffix.is_empty()) || suffix.contains(marker) {
        return observed_title;
    }
    observed_title
        .strip_prefix(prefix)
        .and_then(|title| title.strip_suffix(suffix))
        .unwrap_or(observed_title)
}

fn parse_journal_v1_line(line: &str) -> Option<(u64, ManagedTitle)> {
    let mut fields = line.split('\t');
    let tab_id = fields.next()?.parse().ok()?;
    let state = fields.next()?;
    let count = fields.next()?.parse().ok()?;
    let rename_in_flight = match fields.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    let base_title = hex_decode(fields.next()?)?;
    let last_rendered_title = hex_decode(fields.next()?)?;
    let last_observed_title = hex_decode(fields.next()?)?;
    if fields.next().is_some() {
        return None;
    }
    let aggregate = if state == "-" {
        if count != 0 {
            return None;
        }
        None
    } else {
        if count == 0 {
            return None;
        }
        Some(AggregateStatus {
            state: parse_state(state)?,
            count,
        })
    };

    Some((
        tab_id,
        ManagedTitle {
            base_title,
            possible_titles: vec![last_rendered_title.clone()],
            last_rendered_title,
            last_observed_title,
            aggregate,
            frame_index: 0,
            frame_acknowledged_at_ms: None,
            rename_in_flight,
            remove_after_ack: false,
            recovery_pending: true,
        },
    ))
}

fn parse_journal_v2_line(line: &str) -> Option<(u64, ManagedTitle)> {
    let mut fields = line.split('\t');
    let tab_id = fields.next()?.parse().ok()?;
    let state = fields.next()?;
    let count = fields.next()?.parse().ok()?;
    let base_title = hex_decode(fields.next()?)?;
    let possible_titles: Vec<_> = fields
        .next()?
        .split(',')
        .map(hex_decode)
        .collect::<Option<_>>()?;
    if fields.next().is_some() || possible_titles.is_empty() {
        return None;
    }
    let aggregate = if state == "-" {
        if count != 0 {
            return None;
        }
        None
    } else {
        if count == 0 {
            return None;
        }
        Some(AggregateStatus {
            state: parse_state(state)?,
            count,
        })
    };
    let last_rendered_title = possible_titles[0].clone();
    Some((
        tab_id,
        ManagedTitle {
            base_title,
            last_rendered_title: last_rendered_title.clone(),
            last_observed_title: last_rendered_title,
            aggregate,
            frame_index: 0,
            frame_acknowledged_at_ms: None,
            possible_titles,
            rename_in_flight: false,
            remove_after_ack: false,
            recovery_pending: true,
        },
    ))
}

fn parse_state(value: &str) -> Option<CanonicalState> {
    match value {
        "ready" => Some(CanonicalState::Ready),
        "working" => Some(CanonicalState::Working),
        "waiting_for_user" => Some(CanonicalState::WaitingForUser),
        "succeeded" => Some(CanonicalState::Succeeded),
        "failed" => Some(CanonicalState::Failed),
        "stale" => Some(CanonicalState::Stale),
        "stopped" => Some(CanonicalState::Stopped),
        _ => None,
    }
}

fn hex_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn hex_decode(value: &str) -> Option<String> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let mut decoded = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        decoded.push((hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?);
    }
    String::from_utf8(decoded).ok()
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use zag_lens_core::{IconFrames, Icons, aggregate_states};

    use super::*;

    fn aggregate(state: CanonicalState) -> AggregateStatus {
        AggregateStatus { state, count: 1 }
    }

    fn animated_manager() -> TitleManager {
        let mut icons = Icons::unicode();
        icons.working = IconFrames::new(vec!["◐".into(), "◓".into(), "◑".into()]).unwrap();
        TitleManager::new(TitleConfig {
            icons,
            ..TitleConfig::default()
        })
    }

    #[test]
    fn multiple_states_render_only_the_aggregate_winner_and_count() {
        let mut manager = TitleManager::new(TitleConfig {
            show_counts: true,
            ..TitleConfig::default()
        });
        manager.observe_tab(7, "review");
        let status = aggregate_states([
            CanonicalState::Working,
            CanonicalState::WaitingForUser,
            CanonicalState::WaitingForUser,
            CanonicalState::Failed,
        ]);

        assert_eq!(
            manager.set_aggregate(7, status),
            Some(TitleAction::Rename {
                tab_id: 7,
                title: "?2 review".into(),
            })
        );
        assert_eq!(manager.observe_tab(7, "?2 review"), None);
        assert_eq!(manager.managed(7).unwrap().base_title(), "review");
    }

    #[test]
    fn external_user_rename_becomes_the_base_and_is_redecorated_once() {
        let mut manager = TitleManager::default();
        manager.observe_tab(1, "old");
        manager.set_aggregate(1, Some(aggregate(CanonicalState::Working)));
        manager.observe_tab(1, "● old");

        assert_eq!(
            manager.observe_tab(1, "new"),
            Some(TitleAction::Rename {
                tab_id: 1,
                title: "● new".into(),
            })
        );
        assert_eq!(manager.observe_tab(1, "● new"), None);
        assert_eq!(manager.managed(1).unwrap().base_title(), "new");
        assert!(!manager.managed(1).unwrap().rename_in_flight());
    }

    #[test]
    fn changing_state_never_accumulates_status_prefixes() {
        let mut manager = TitleManager::default();
        manager.observe_tab(2, "build");
        manager.set_aggregate(2, Some(aggregate(CanonicalState::Working)));
        manager.observe_tab(2, "● build");

        assert_eq!(
            manager.set_aggregate(2, Some(aggregate(CanonicalState::Failed))),
            Some(TitleAction::Rename {
                tab_id: 2,
                title: "× build".into(),
            })
        );
        assert_eq!(manager.observe_tab(2, "× build"), None);
        assert_eq!(manager.managed(2).unwrap().base_title(), "build");
    }

    #[test]
    fn clearing_last_state_restores_and_then_releases_title_ownership() {
        let mut manager = TitleManager::default();
        manager.observe_tab(3, "tests");
        manager.set_aggregate(3, Some(aggregate(CanonicalState::Succeeded)));
        manager.observe_tab(3, "✓ tests");

        assert_eq!(
            manager.set_aggregate(3, None),
            Some(TitleAction::Restore {
                tab_id: 3,
                title: "tests".into(),
            })
        );
        assert_eq!(manager.observe_tab(3, "tests"), None);
        assert!(manager.managed(3).is_none());
    }

    #[test]
    fn normal_shutdown_restores_each_decorated_tab_in_stable_order() {
        let mut manager = TitleManager::default();
        manager.observe_tab(20, "second");
        manager.observe_tab(10, "first");
        manager.set_aggregate(20, Some(aggregate(CanonicalState::Failed)));
        manager.set_aggregate(10, Some(aggregate(CanonicalState::Working)));

        assert_eq!(
            manager.shutdown_actions(),
            [
                TitleAction::Restore {
                    tab_id: 10,
                    title: "first".into(),
                },
                TitleAction::Restore {
                    tab_id: 20,
                    title: "second".into(),
                },
            ]
        );
        assert!(manager.shutdown_actions().is_empty());
    }

    #[test]
    fn reload_repairs_an_exact_journaled_rendered_title() {
        let mut original = TitleManager::default();
        original.observe_tab(4, "api");
        original.set_aggregate(4, Some(aggregate(CanonicalState::Working)));
        original.observe_tab(4, "● api");
        let journal = original.journal_snapshot();

        let mut recovered = TitleManager::from_journal(TitleConfig::default(), &journal);
        assert_eq!(
            recovered.observe_tab(4, "● api"),
            Some(TitleAction::Restore {
                tab_id: 4,
                title: "api".into(),
            })
        );
        assert_eq!(recovered.observe_tab(4, "api"), None);
        assert!(recovered.managed(4).is_none());
    }

    #[test]
    fn reload_never_rewrites_a_nonmatching_external_title() {
        let mut original = TitleManager::default();
        original.observe_tab(5, "api");
        original.set_aggregate(5, Some(aggregate(CanonicalState::Working)));
        let journal = original.journal_snapshot();

        let mut recovered = TitleManager::from_journal(TitleConfig::default(), &journal);
        assert_eq!(recovered.observe_tab(5, "! user-title"), None);
        assert!(recovered.managed(5).is_none());
    }

    #[test]
    fn only_the_exact_current_decoration_prefix_is_removed() {
        let mut manager = TitleManager::default();
        manager.observe_tab(6, "base");
        manager.set_aggregate(6, Some(aggregate(CanonicalState::Working)));
        manager.observe_tab(6, "● base");

        assert_eq!(
            manager.observe_tab(6, "× intentional"),
            Some(TitleAction::Rename {
                tab_id: 6,
                title: "● × intentional".into(),
            })
        );
        assert_eq!(manager.managed(6).unwrap().base_title(), "× intentional");
        assert_eq!(manager.observe_tab(6, "● exact"), None);
        assert_eq!(manager.managed(6).unwrap().base_title(), "exact");
    }

    #[test]
    fn suffix_formats_do_not_trigger_broad_prefix_stripping() {
        let mut manager = TitleManager::new(TitleConfig {
            format: "{icon} {title} [agent]".into(),
            icons: Icons::ascii(),
            show_counts: false,
        });
        manager.observe_tab(8, "base");
        manager.set_aggregate(8, Some(aggregate(CanonicalState::Working)));
        manager.observe_tab(8, "* base [agent]");

        manager.observe_tab(8, "* intentional");
        assert_eq!(manager.managed(8).unwrap().base_title(), "* intentional");
    }

    #[test]
    fn unchanged_and_stale_in_flight_observations_are_coalesced() {
        let mut manager = TitleManager::default();
        manager.observe_tab(9, "work");
        assert!(
            manager
                .set_aggregate(9, Some(aggregate(CanonicalState::Working)))
                .is_some()
        );
        assert_eq!(manager.observe_tab(9, "work"), None);
        assert_eq!(
            manager.set_aggregate(9, Some(aggregate(CanonicalState::Working))),
            None
        );
    }

    #[test]
    fn journal_round_trips_unicode_tabs_and_skips_malformed_entries() {
        let mut manager = TitleManager::default();
        manager.observe_tab(11, "міграції 🚀");
        manager.set_aggregate(11, Some(aggregate(CanonicalState::WaitingForUser)));
        let mut journal = manager.journal_snapshot();
        journal.push_str("bad\tentry\n");

        let recovered = TitleManager::from_journal(TitleConfig::default(), &journal);
        let title = recovered.managed(11).unwrap();
        assert_eq!(title.base_title(), "міграції 🚀");
        assert_eq!(
            title.aggregate().unwrap().state,
            CanonicalState::WaitingForUser
        );
    }

    #[test]
    fn animation_advances_in_order_wraps_and_waits_for_each_acknowledgement() {
        let mut manager = animated_manager();
        manager.observe_tab_at(1, "work", 0);
        assert_eq!(
            manager.set_aggregate_at(1, Some(aggregate(CanonicalState::Working)), 0),
            Some(TitleAction::Rename {
                tab_id: 1,
                title: "◐ work".into(),
            })
        );
        assert!(manager.advance_animations(1_000, 250).is_empty());
        manager.observe_tab_at(1, "◐ work", 100);
        assert!(manager.advance_animations(349, 250).is_empty());
        assert_eq!(
            manager.advance_animations(350, 250),
            [TitleAction::Rename {
                tab_id: 1,
                title: "◓ work".into(),
            }]
        );
        assert!(manager.advance_animations(5_000, 250).is_empty());
        manager.observe_tab_at(1, "◓ work", 5_000);
        assert_eq!(
            manager.advance_animations(6_000, 250),
            [TitleAction::Rename {
                tab_id: 1,
                title: "◑ work".into(),
            }]
        );
        manager.observe_tab_at(1, "◑ work", 6_000);
        assert_eq!(
            manager.advance_animations(6_250, 250),
            [TitleAction::Rename {
                tab_id: 1,
                title: "◐ work".into(),
            }]
        );
    }

    #[test]
    fn tabs_animate_independently_and_static_icons_never_tick() {
        let mut manager = animated_manager();
        manager.observe_tab_at(1, "one", 0);
        manager.observe_tab_at(2, "two", 0);
        manager.observe_tab_at(3, "three", 0);
        manager.set_aggregate_at(1, Some(aggregate(CanonicalState::Working)), 0);
        manager.set_aggregate_at(2, Some(aggregate(CanonicalState::Working)), 0);
        manager.set_aggregate_at(3, Some(aggregate(CanonicalState::Failed)), 0);
        manager.observe_tab_at(1, "◐ one", 100);
        manager.observe_tab_at(2, "◐ two", 200);
        manager.observe_tab_at(3, "× three", 100);

        assert_eq!(
            manager.advance_animations(350, 250),
            [TitleAction::Rename {
                tab_id: 1,
                title: "◓ one".into(),
            }]
        );
        assert_eq!(
            manager.advance_animations(450, 250),
            [TitleAction::Rename {
                tab_id: 2,
                title: "◓ two".into(),
            }]
        );
        assert!(manager.managed(3).is_some());
    }

    #[test]
    fn state_changes_restart_while_count_and_base_renames_keep_phase() {
        let mut manager = animated_manager();
        manager.config.show_counts = true;
        manager.observe_tab_at(1, "work", 0);
        manager.set_aggregate_at(1, Some(aggregate(CanonicalState::Working)), 0);
        manager.observe_tab_at(1, "◐ work", 0);
        let frame_one = manager.advance_animations(250, 250).pop().unwrap();
        assert_eq!(frame_one.title(), "◓ work");
        manager.observe_tab_at(1, "◓ work", 250);

        assert_eq!(
            manager
                .set_aggregate_at(
                    1,
                    Some(AggregateStatus {
                        state: CanonicalState::Working,
                        count: 2,
                    }),
                    300,
                )
                .unwrap()
                .title(),
            "◓2 work"
        );
        manager.observe_tab_at(1, "◓2 work", 300);
        assert_eq!(
            manager.observe_tab_at(1, "renamed", 350).unwrap().title(),
            "◓2 renamed"
        );
        manager.observe_tab_at(1, "◓2 renamed", 350);

        assert_eq!(
            manager
                .set_aggregate_at(1, Some(aggregate(CanonicalState::Succeeded)), 400)
                .unwrap()
                .title(),
            "✓ renamed"
        );
        manager.observe_tab_at(1, "✓ renamed", 400);
        assert_eq!(
            manager
                .set_aggregate_at(1, Some(aggregate(CanonicalState::Working)), 450)
                .unwrap()
                .title(),
            "◐ renamed"
        );
        assert_eq!(manager.managed(1).unwrap().frame_index(), 0);
    }

    #[test]
    fn state_change_during_in_flight_rename_is_sent_only_after_ack() {
        let mut manager = animated_manager();
        manager.observe_tab_at(1, "work", 0);
        manager.set_aggregate_at(1, Some(aggregate(CanonicalState::Working)), 0);
        assert_eq!(
            manager.set_aggregate_at(1, Some(aggregate(CanonicalState::Failed)), 10),
            None
        );
        assert_eq!(
            manager.observe_tab_at(1, "◐ work", 20),
            Some(TitleAction::Rename {
                tab_id: 1,
                title: "× work".into(),
            })
        );
    }

    #[test]
    fn delayed_owned_frames_are_not_user_renames() {
        let mut manager = animated_manager();
        manager.observe_tab_at(1, "work", 0);
        manager.set_aggregate_at(1, Some(aggregate(CanonicalState::Working)), 0);
        manager.observe_tab_at(1, "◐ work", 0);
        manager.advance_animations(250, 250);
        manager.observe_tab_at(1, "◓ work", 250);

        assert_eq!(
            manager.observe_tab_at(1, "◐ work", 260),
            Some(TitleAction::Rename {
                tab_id: 1,
                title: "◓ work".into(),
            })
        );
        assert_eq!(manager.managed(1).unwrap().base_title(), "work");
    }

    #[test]
    fn recovery_accepts_every_possible_frame_and_frame_acks_do_not_dirty_journal() {
        let mut original = animated_manager();
        original.observe_tab_at(1, "work", 0);
        original.set_aggregate_at(1, Some(aggregate(CanonicalState::Working)), 0);
        let journal = original.take_dirty_journal_snapshot().unwrap();
        assert!(original.take_dirty_journal_snapshot().is_none());
        original.observe_tab_at(1, "◐ work", 0);
        original.advance_animations(250, 250);
        original.observe_tab_at(1, "◓ work", 250);
        assert!(original.take_dirty_journal_snapshot().is_none());

        for frame in ["◐ work", "◓ work", "◑ work"] {
            let mut recovered = TitleManager::from_journal(animated_manager().config, &journal);
            assert_eq!(recovered.observe_tab(1, frame).unwrap().title(), "work");
        }
    }

    #[test]
    fn version_one_journals_remain_recoverable() {
        let base = hex_encode("api");
        let rendered = hex_encode("● api");
        let journal =
            format!("{JOURNAL_HEADER_V1}\n4\tworking\t1\t0\t{base}\t{rendered}\t{rendered}\n");
        let mut recovered = TitleManager::from_journal(TitleConfig::default(), &journal);
        assert_eq!(
            recovered.observe_tab(4, "● api"),
            Some(TitleAction::Restore {
                tab_id: 4,
                title: "api".into(),
            })
        );
    }
}
