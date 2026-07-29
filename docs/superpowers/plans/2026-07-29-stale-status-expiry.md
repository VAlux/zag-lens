# Stale Status Expiry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the `stale` (`!`) tab decoration an expiry path so a silently vanished agent stops decorating its Zellij tab forever.

**Architecture:** `zag_lens_core::Reducer::advance_time` currently removes only `Succeeded` agents on a TTL; `Stale` has no exit. Add a `stale_ttl` duration to `ReducerConfig` and one branch to `advance_time` that removes any inactive-family agent once `stale_after + stale_ttl` has elapsed since its last real event. Expose it as the `stale_ttl_seconds` KDL plugin setting.

**Tech Stack:** Rust 2024 edition, `time` crate for RFC 3339 clock handling, Zellij WASM plugin API, KDL plugin configuration.

## Global Constraints

- Rust 1.94.1, edition 2024, as pinned by `rust-toolchain.toml`.
- `unsafe_code = "forbid"` and `clippy::all` + `clippy::pedantic` at warn, enforced as `-D warnings` in CI.
- `crates/core` must stay free of Zellij APIs and native harness payloads; it takes an explicit clock and normalized events only.
- The reducer must remain deterministic and independently unit-testable.
- Never derive agent state by scraping terminal output, transcripts, prompts, or assistant message text.
- Every task must leave `cargo fmt --all --check` clean.
- The `Stale` decoration lifetime is `stale_after + stale_ttl`, measured from the agent's last real event.
- `stale_ttl_seconds` default `300`, accepted range `0` through `604800`.

---

### Task 1: Core reducer stale expiry

**Files:**
- Modify: `crates/core/src/lib.rs` (constants at 14-19, `ReducerConfig` at 23-43, `TransitionCause` at 74-81, `advance_time` at 258-298, private helpers near 423-428, tests module from 679)
- Modify: `SPECIFICATION.md:359-360`
- Test: `crates/core/src/lib.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `ReducerConfig` gains public field `pub stale_ttl: Duration`, defaulting to `Duration::from_secs(300)`. Task 2 sets this field.
  - `TransitionCause` gains unit variant `StaleTtlExpired`. It is `Clone + Copy + Debug + Eq + PartialEq` like every other variant.
  - `Reducer::advance_time(&mut self, now: OffsetDateTime) -> Vec<Transition>` keeps its signature; it may now emit transitions with `cause: TransitionCause::StaleTtlExpired` and `current: None`.

- [ ] **Step 1: Write the failing tests**

Append these three tests to the `mod tests` block in `crates/core/src/lib.rs`, directly after the existing `inactive_non_terminal_state_becomes_stale_once` test (which currently ends at line 1033). They use the existing test helpers `event`, `applied`, and `at` defined at lines 691, 718, and 725.

```rust
    #[test]
    fn stale_state_expires_after_stale_ttl() {
        let mut reducer = Reducer::new(ReducerConfig {
            stale_after: Duration::from_secs(10),
            stale_ttl: Duration::from_secs(5),
            ..ReducerConfig::default()
        });
        applied(
            reducer
                .apply(&event(EventKind::TurnStarted, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );

        let stale = reducer.advance_time(at("2026-07-13T12:00:10Z"));
        assert_eq!(stale[0].cause, TransitionCause::InactivityTimeout);
        assert_eq!(
            stale[0].current.as_ref().unwrap().state,
            CanonicalState::Stale
        );

        assert!(reducer.advance_time(at("2026-07-13T12:00:14Z")).is_empty());

        let expired = reducer.advance_time(at("2026-07-13T12:00:15Z"));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].cause, TransitionCause::StaleTtlExpired);
        assert!(expired[0].current.is_none());
        assert!(reducer.is_empty());
        assert_eq!(reducer.aggregate_for_panes(["pane-1"]), None);
    }

    #[test]
    fn zero_stale_ttl_removes_instance_without_showing_stale() {
        let mut reducer = Reducer::new(ReducerConfig {
            stale_after: Duration::from_secs(10),
            stale_ttl: Duration::ZERO,
            ..ReducerConfig::default()
        });
        applied(
            reducer
                .apply(&event(EventKind::TurnStarted, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );

        let transitions = reducer.advance_time(at("2026-07-13T12:00:10Z"));
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].cause, TransitionCause::StaleTtlExpired);
        assert_eq!(
            transitions[0].previous.as_ref().unwrap().state,
            CanonicalState::Working
        );
        assert!(transitions[0].current.is_none());
        assert!(reducer.is_empty());
    }

    #[test]
    fn event_during_stale_window_resets_expiry() {
        let mut reducer = Reducer::new(ReducerConfig {
            stale_after: Duration::from_secs(10),
            stale_ttl: Duration::from_secs(5),
            ..ReducerConfig::default()
        });
        applied(
            reducer
                .apply(&event(EventKind::TurnStarted, "2026-07-13T12:00:00Z"))
                .unwrap(),
        );
        reducer.advance_time(at("2026-07-13T12:00:10Z"));

        applied(
            reducer
                .apply(&event(EventKind::Activity, "2026-07-13T12:00:12Z"))
                .unwrap(),
        );
        assert_eq!(
            reducer.agents().next().map(|snapshot| snapshot.state),
            Some(CanonicalState::Working)
        );

        assert!(reducer.advance_time(at("2026-07-13T12:00:15Z")).is_empty());
        assert_eq!(reducer.len(), 1);
    }
```

Why each test exists:
- The first pins the whole timeline: stale appears on schedule, survives right up to the deadline, then the agent is removed and the tab aggregate goes empty so the base title is restored.
- The second pins the `0` contract from the spec — stale never becomes visible, and the removed snapshot was still `Working`.
- The third pins that the clock is the agent's last real event, not a one-shot countdown: a resurrected harness clears its own `!` and gets a fresh `stale_after` window.

Two existing tests are the regression guard for the non-goals and must keep passing untouched: `success_expires_while_failure_persists` (line 988) proves `Failed` is still never expired, and `inactive_non_terminal_state_becomes_stale_once` (line 1014) proves the `stale_after` transition itself is unchanged.

- [ ] **Step 2: Run the tests to verify they fail**

```sh
cargo test -p zag-lens-core
```

Expected: compilation failure, not an assertion failure — `ReducerConfig` has no field named `stale_ttl` and `TransitionCause` has no variant `StaleTtlExpired`.

- [ ] **Step 3: Add the config field and transition cause**

In `crates/core/src/lib.rs`, add the constant after line 15:

```rust
const DEFAULT_STALE_TTL_SECONDS: u64 = 300;
```

Add the field to `ReducerConfig` after `stale_after`:

```rust
pub struct ReducerConfig {
    pub enabled: bool,
    pub success_ttl: Duration,
    pub stale_after: Duration,
    pub stale_ttl: Duration,
    pub max_agents: usize,
    pub deduplication_capacity: usize,
    pub identity_cursor_capacity: usize,
}
```

And to its `Default` impl after the `stale_after` line:

```rust
            stale_ttl: Duration::from_secs(DEFAULT_STALE_TTL_SECONDS),
```

Add the variant to `TransitionCause` after `SuccessTtlExpired`:

```rust
pub enum TransitionCause {
    Event(EventKind),
    InactivityTimeout,
    SuccessTtlExpired,
    StaleTtlExpired,
    PaneClosed,
    CapacityEviction,
    ExplicitClear,
}
```

- [ ] **Step 4: Add the expiry branch to `advance_time`**

In `crates/core/src/lib.rs`, update the doc comment and body of `advance_time`. Replace the existing doc line and the `stale_after` binding (lines 258 and 266) so the deadline is precomputed:

```rust
    /// Applies success expiry, inactivity, and stale expiry at an explicit time.
    pub fn advance_time(&mut self, now: OffsetDateTime) -> Vec<Transition> {
```

```rust
        let stale_after = duration_nanos(self.config.stale_after);
        let stale_expiry = stale_after.saturating_add(duration_nanos(self.config.stale_ttl));
```

Then replace the `if`/`else if` chain inside the loop (lines 277-295) with:

```rust
            if previous.state == CanonicalState::Succeeded && elapsed >= success_ttl {
                self.agents.remove(&identity);
                remove_from_order(&mut self.agent_order, &identity);
                transitions.push(Transition {
                    previous: Some(previous),
                    current: None,
                    cause: TransitionCause::SuccessTtlExpired,
                });
            } else if is_expiring_inactive(previous.state) && elapsed >= stale_expiry {
                self.agents.remove(&identity);
                remove_from_order(&mut self.agent_order, &identity);
                transitions.push(Transition {
                    previous: Some(previous),
                    current: None,
                    cause: TransitionCause::StaleTtlExpired,
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
```

Branch order matters: the new removal is checked before the stale promotion so that an agent already past total expiry is dropped outright instead of first being promoted to `Stale` and only cleaned up on some later tick.

Add the predicate next to `is_stale_candidate` (after line 428):

```rust
const fn is_expiring_inactive(state: CanonicalState) -> bool {
    is_stale_candidate(state) || matches!(state, CanonicalState::Stale)
}
```

`elapsed` is deliberately not reset when an agent enters `Stale` — the existing promotion clones the snapshot and mutates only `state` and `attention`, leaving `occurred_at_unix_nanos` alone. That makes `stale_after + stale_ttl` an exact deadline with no timer-tick drift and needs no new field on `AgentSnapshot`.

- [ ] **Step 5: Run the tests to verify they pass**

```sh
cargo test -p zag-lens-core
```

Expected: PASS, including every pre-existing test. Four of them exercise `advance_time` and were audited against the new default: `success_expires_while_failure_persists` (line 988), `inactive_non_terminal_state_becomes_stale_once` (1014), the stale leg of `new_activity_clears_every_resumable_terminal_state` (860), and `timers_before_last_event_do_not_change_state` (1332). None reaches `stale_after + 300s`, and the last one passes a `now` earlier than the event, producing a negative `elapsed` that fails the new `>=` comparison. If any of them fails, the branch order in Step 4 is wrong — do not adjust the test.

- [ ] **Step 6: Update the specification's normative statement**

In `SPECIFICATION.md`, replace the paragraph at lines 359-360:

```markdown
The plugin SHOULD mark a non-terminal state `stale` after a configurable
inactivity interval. Closing the owning pane clears the state immediately.
```

with:

```markdown
The plugin SHOULD mark a non-terminal state `stale` after a configurable
inactivity interval, and SHOULD then remove the instance after a second
configurable interval so the tab title is restored without user action. Both
intervals are measured from the instance's last lifecycle event. Closing the
owning pane clears the state immediately.
```

- [ ] **Step 7: Verify formatting and lints, then commit**

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude zag-lens-plugin
```

Expected: all three clean.

```bash
git add crates/core/src/lib.rs SPECIFICATION.md
git commit -m "feat(core): expire stale agents after a configurable TTL"
```

---

### Task 2: Expose `stale_ttl_seconds` as a plugin setting

**Files:**
- Modify: `plugin/src/lib.rs:361-379` (`reducer_config`)
- Modify: `README.md:116`, `README.md:135`
- Modify: `docs/configuration.md:16`, `docs/configuration.md:36`
- Modify: `SPECIFICATION.md:547`
- Test: `plugin/src/lib.rs` (inline `mod tests`, starts at line 761)

**Interfaces:**
- Consumes: `ReducerConfig::stale_ttl: Duration` and `ReducerConfig::default()` from Task 1.
- Produces: the user-facing KDL key `stale_ttl_seconds`. No new Rust items.

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block in `plugin/src/lib.rs`. The module already has `use super::*;` at line 763, which brings `BTreeMap`, `Duration`, `ReducerConfig`, and `reducer_config` into scope.

```rust
    #[test]
    fn stale_ttl_setting_parses_and_falls_back_when_out_of_range() {
        let default = ReducerConfig::default().stale_ttl;
        assert_eq!(default, Duration::from_secs(300));
        assert_eq!(reducer_config(&BTreeMap::new()).stale_ttl, default);

        let mut values = BTreeMap::new();
        values.insert("stale_ttl_seconds".to_owned(), "0".to_owned());
        assert_eq!(reducer_config(&values).stale_ttl, Duration::ZERO);

        values.insert("stale_ttl_seconds".to_owned(), "604800".to_owned());
        assert_eq!(
            reducer_config(&values).stale_ttl,
            Duration::from_secs(604_800)
        );

        values.insert("stale_ttl_seconds".to_owned(), "604801".to_owned());
        assert_eq!(reducer_config(&values).stale_ttl, default);

        values.insert("stale_ttl_seconds".to_owned(), "soon".to_owned());
        assert_eq!(reducer_config(&values).stale_ttl, default);
    }
```

This covers the whole contract in one test because `parse_number` handles the default, both bounds, and the unparseable case through a single expression — splitting it would test the same call site four times.

- [ ] **Step 2: Run the test to verify it fails**

```sh
cargo test -p zag-lens-plugin --bin zag_lens_plugin stale_ttl
```

Expected: FAIL. `reducer_config` does not read `stale_ttl_seconds` yet, so the `"0"` case returns the 300-second default instead of `Duration::ZERO`.

- [ ] **Step 3: Parse the setting**

In `plugin/src/lib.rs`, add the field to `reducer_config` immediately after the `stale_after` entry:

```rust
        stale_ttl: Duration::from_secs(parse_number(
            values.get("stale_ttl_seconds"),
            defaults.stale_ttl.as_secs(),
            0,
            604_800,
        )),
```

The minimum is `0` (stale is never displayed) and the maximum matches `stale_after_seconds`. Out-of-range and unparseable values fall back to the default via `parse_number`, so a bad setting never disables title updates.

- [ ] **Step 4: Run the test to verify it passes**

```sh
cargo test -p zag-lens-plugin --bin zag_lens_plugin stale_ttl
```

Expected: PASS.

- [ ] **Step 5: Document the setting in the README**

In `README.md`, insert after line 116 inside the KDL block:

```kdl
        stale_ttl_seconds "300"
```

Then insert this row into the settings table directly after the `stale_after_seconds` row at line 135. The table is space-aligned to 23/16/59-character columns; copy the row verbatim to preserve alignment:

```markdown
| `stale_ttl_seconds`     | `300`            | Removes a stale status this long after it appears.          |
```

- [ ] **Step 6: Document the setting in the configuration guide and specification**

In `docs/configuration.md`, insert after line 16 inside the KDL block:

```kdl
        stale_ttl_seconds "300"
```

Insert this row after the `stale_after_seconds` row at line 36 (this table is not space-aligned):

```markdown
| `stale_ttl_seconds` | `300` | `0` through `604800`; `0` never displays `stale`. |
```

In `SPECIFICATION.md`, insert this row after the `stale_after_seconds` row at line 547:

```markdown
| `stale_ttl_seconds` | `300` | Additional time a stale state is shown before removal. |
```

- [ ] **Step 7: Run the full verification suite**

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude zag-lens-plugin
cargo test -p zag-lens-plugin --bin zag_lens_plugin
cargo check -p zag-lens-plugin --target wasm32-wasip1
```

Expected: all five clean. The final `cargo check` matters because the plugin is the only crate built for `wasm32-wasip1` and CI gates on it separately.

- [ ] **Step 8: Commit**

```bash
git add plugin/src/lib.rs README.md docs/configuration.md SPECIFICATION.md
git commit -m "feat(plugin): add stale_ttl_seconds setting"
```

---

## Manual verification

Optional, after both tasks. Rebuild and install the plugin, then set aggressive values in the resolved `config.kdl`:

```kdl
        stale_after_seconds "10"
        stale_ttl_seconds "5"
```

Restart Zellij, start a harness in a pane so the tab shows `●`, then kill the harness process without letting it exit cleanly (so no `SessionEnd` fires). The tab should show `●` for 10 seconds, `!` for 5 more, then return to its base title. `scripts/test-smoke-status-tabs.sh` covers the surrounding tab-title behavior.

## Out of scope

`AskUserQuestion` does not produce `waiting_for_user`, because Claude Code fires no `PermissionRequest` for it and `idle_prompt` only fires after an idle threshold the user usually beats by answering. It is deferred to a separate spec, as recorded in `docs/superpowers/specs/2026-07-29-stale-status-expiry-design.md`.
