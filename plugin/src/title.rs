//! Pure ownership controller for Zellij tab-title mutations.
//!
//! The controller does not call Zellij directly. It compares observed tab
//! names with the names it owns and emits explicit, coalesced actions for the
//! runtime to execute.

use std::collections::HashMap;
use std::fmt::Write as _;

use zag_lens_core::{AggregateStatus, TitleConfig};
use zag_lens_protocol::CanonicalState;

const JOURNAL_HEADER: &str = "zag-lens-title-journal-v1";

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
}

/// Pure controller for base-title ownership, decoration, and restoration.
#[derive(Clone, Debug)]
pub struct TitleManager {
    config: TitleConfig,
    tabs: HashMap<u64, ManagedTitle>,
}

impl TitleManager {
    /// Creates an empty controller using a safe title format.
    #[must_use]
    pub fn new(config: TitleConfig) -> Self {
        Self {
            config: config.with_safe_format(),
            tabs: HashMap::new(),
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
        if lines.next() != Some(JOURNAL_HEADER) {
            return manager;
        }

        for line in lines {
            if let Some((tab_id, managed)) = parse_journal_line(line) {
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
        let Some(managed) = self.tabs.get_mut(&tab_id) else {
            self.tabs.insert(
                tab_id,
                ManagedTitle {
                    base_title: observed_title.to_owned(),
                    last_rendered_title: observed_title.to_owned(),
                    last_observed_title: observed_title.to_owned(),
                    aggregate: None,
                    rename_in_flight: false,
                    remove_after_ack: false,
                    recovery_pending: false,
                },
            );
            return None;
        };

        if managed.recovery_pending {
            managed.recovery_pending = false;
            if observed_title == managed.last_rendered_title
                && managed.last_rendered_title != managed.base_title
            {
                managed.aggregate = None;
                observed_title.clone_into(&mut managed.last_observed_title);
                managed.remove_after_ack = true;
                return Some(emit_action(tab_id, managed, managed.base_title.clone()));
            }

            // A non-matching name cannot safely be identified as ours.
            self.tabs.remove(&tab_id);
            return None;
        }

        if observed_title == managed.last_rendered_title {
            observed_title.clone_into(&mut managed.last_observed_title);
            managed.rename_in_flight = false;
            if managed.remove_after_ack {
                self.tabs.remove(&tab_id);
            }
            return None;
        }

        // Ignore a repeated stale observation while a mutation is in flight.
        if managed.rename_in_flight && observed_title == managed.last_observed_title {
            return None;
        }

        let base_title = strip_exact_prefix(&self.config, observed_title, managed.aggregate);
        base_title.clone_into(&mut managed.base_title);
        observed_title.clone_into(&mut managed.last_observed_title);
        managed.remove_after_ack = false;

        let desired = self.config.render(&managed.base_title, managed.aggregate);
        if desired == observed_title {
            managed.last_rendered_title = desired;
            managed.rename_in_flight = false;
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
        let managed = self.tabs.get_mut(&tab_id)?;
        managed.recovery_pending = false;
        managed.aggregate = aggregate;
        managed.remove_after_ack = aggregate.is_none();
        let desired = self.config.render(&managed.base_title, aggregate);

        if desired == managed.last_rendered_title {
            if managed.remove_after_ack && !managed.rename_in_flight {
                self.tabs.remove(&tab_id);
            }
            return None;
        }

        Some(emit_action(tab_id, managed, desired))
    }

    /// Forgets a tab that no longer exists without attempting restoration.
    pub fn remove_tab(&mut self, tab_id: u64) {
        self.tabs.remove(&tab_id);
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
        actions
    }

    /// Serializes a small, versioned, dependency-free recovery journal.
    #[must_use]
    pub fn journal_snapshot(&self) -> String {
        let mut tab_ids: Vec<_> = self.tabs.keys().copied().collect();
        tab_ids.sort_unstable();
        let mut output = String::from(JOURNAL_HEADER);
        output.push('\n');

        for tab_id in tab_ids {
            let managed = &self.tabs[&tab_id];
            let (state, count) = managed.aggregate.map_or(("-", 0), |aggregate| {
                (aggregate.state.as_str(), aggregate.count)
            });
            writeln!(
                output,
                "{tab_id}\t{state}\t{count}\t{}\t{}\t{}\t{}",
                u8::from(managed.rename_in_flight),
                hex_encode(&managed.base_title),
                hex_encode(&managed.last_rendered_title),
                hex_encode(&managed.last_observed_title),
            )
            .expect("writing to String cannot fail");
        }
        output
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

fn strip_exact_prefix<'a>(
    config: &TitleConfig,
    observed_title: &'a str,
    aggregate: Option<AggregateStatus>,
) -> &'a str {
    let marker = "\u{0}";
    let rendered_marker = config.render(marker, aggregate);
    let Some(prefix) = rendered_marker.strip_suffix(marker) else {
        return observed_title;
    };
    if prefix.is_empty() || prefix.contains(marker) {
        return observed_title;
    }
    observed_title
        .strip_prefix(prefix)
        .unwrap_or(observed_title)
}

fn parse_journal_line(line: &str) -> Option<(u64, ManagedTitle)> {
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
            last_rendered_title,
            last_observed_title,
            aggregate,
            rename_in_flight,
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
    use zag_lens_core::{Icons, aggregate_states};

    use super::*;

    fn aggregate(state: CanonicalState) -> AggregateStatus {
        AggregateStatus { state, count: 1 }
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
}
