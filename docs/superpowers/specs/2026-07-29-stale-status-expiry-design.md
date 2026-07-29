# Stale Status Expiry

## Problem

A `stale` (`!`) tab decoration never clears. `Succeeded` is the only canonical
state with an expiry path: `advance_time` removes it once `success_ttl` elapses
(`crates/core/src/lib.rs:277`). `Stale` is not a stale candidate
(`crates/core/src/lib.rs:423`) and has no TTL, so once an agent reaches it the
`!` prefix stays on the tab title until the pane closes or a new event for that
agent arrives.

`Stale` is reached only when a harness stops emitting without a terminal event.
A clean shutdown produces `SessionEnded` → `Stopped`, which is invisible and
restores the base title. So `!` marks an agent that died silently: a crashed
harness, a broken hook chain, or a suspended machine. That signal is worth
showing, but it has no reason to be permanent — after the agent is gone, the
decoration is stale information about stale information.

## Goal

A vanished agent's decoration disappears on its own and the tab returns to its
base title, without the user closing the pane or restarting Zellij.

## Non-goals

- Liveness probing of the pane's process. Zellij's plugin API exposes no
  reliable exit signal for a shell pane running a harness, and the absence of
  events is already the signal that produced `Stale`.
- Expiring `failed` (`×`). A failure is something the user should notice even
  after looking away, and pinning it is defensible. It stays as it is.
- Any change to `success_ttl`, `stale_after`, notifications, or the OpenCode and
  Codex adapters.

## Design

### Configuration

One new plugin setting, parsed alongside `stale_after_seconds` in
`reducer_config` (`plugin/src/lib.rs:371`):

| Setting             | Default | Bounds        |
| ------------------- | ------- | ------------- |
| `stale_ttl_seconds` | `300`   | `0`–`604_800` |

`stale_ttl_seconds` is time *in addition to* `stale_after_seconds`. With both
defaults, a silently vanished agent decorates its tab with `●`/`?` for 30
minutes, then `!` for 5 minutes, then disappears.

`0` means stale is never displayed: the agent is removed on the same tick it
would otherwise have entered `Stale`. The maximum, `604_800` (7 days, matching
`stale_after_seconds`), is the escape hatch for anyone who wants the current
indefinite behavior back. There is no separate "never" sentinel.

`ReducerConfig` gains `pub stale_ttl: Duration`, defaulting from a new
`DEFAULT_STALE_TTL_SECONDS: u64 = 300` constant next to the existing defaults at
`crates/core/src/lib.rs:14`.

### Reducer

`advance_time` gains one branch, ordered before the existing stale transition so
that an agent already past total expiry is removed rather than first being
promoted to `Stale`:

```rust
let stale_expiry = stale_after.saturating_add(duration_nanos(self.config.stale_ttl));

if previous.state == CanonicalState::Succeeded && elapsed >= success_ttl {
    // remove — TransitionCause::SuccessTtlExpired   (unchanged)
} else if is_expiring_inactive(previous.state) && elapsed >= stale_expiry {
    // remove — TransitionCause::StaleTtlExpired     (new)
} else if is_stale_candidate(previous.state) && elapsed >= stale_after {
    // → CanonicalState::Stale — InactivityTimeout   (unchanged)
}
```

with a new private predicate:

```rust
const fn is_expiring_inactive(state: CanonicalState) -> bool {
    is_stale_candidate(state) || matches!(state, CanonicalState::Stale)
}
```

Removal reuses the existing `remove_from_order` + `agents.remove` pair already
used by the success-TTL branch, and pushes a `Transition` with `current: None`.

### Clock semantics

`elapsed` is measured from `occurred_at_unix_nanos` — the last real event — not
from the moment the agent entered `Stale`. The existing stale transition clones
the snapshot and mutates only `state` and `attention`
(`crates/core/src/lib.rs:286`), leaving `occurred_at_unix_nanos` untouched, so
`stale_after + stale_ttl` is an exact, tick-drift-free deadline.

This is deliberate: it needs no new field on `AgentSnapshot`, and it makes the
total decoration lifetime of a vanished agent a simple sum the user can reason
about from two config values. Measuring from stale entry would instead depend on
which timer tick happened to observe the threshold.

Any real event for the agent updates `occurred_at_unix_nanos` through `apply`,
which resets both deadlines — a resurrected harness clears its own `!`.

### Transition cause

`TransitionCause` gains a `StaleTtlExpired` variant. `plugin/src/attention.rs`
matches only on `TransitionCause::Event(..)` (lines 406 and 427), so a removal
transition cannot fire a notification; the new variant takes the same path
`SuccessTtlExpired` already takes. `handle_transition` in `plugin/src/lib.rs`
already restores the base title for a transition with `current: None`, so no
title-layer change is required.

## Testing

Unit tests in `crates/core/src/lib.rs`, mirroring the existing success-TTL test
at line 991:

1. An agent that has gone stale is removed once `stale_after + stale_ttl`
   elapses, with cause `StaleTtlExpired` and a resulting empty aggregate.
2. The same agent is *not* removed one nanosecond before that deadline, and is
   still reported as `Stale`.
3. With `stale_ttl` of zero, a `Working` agent is removed at `stale_after`
   without ever being observed as `Stale`.
4. An event arriving during the stale window resets the deadline and restores a
   visible non-stale state.

Config parsing is covered by an existing-style assertion that out-of-range and
non-numeric `stale_ttl_seconds` values fall back to the default without
disabling title updates.

## Documentation

- `README.md:115` KDL example and the settings table at `README.md:134`.
- `docs/configuration.md:17` example and the bounds table at line 36.
- `SPECIFICATION.md:547`, adding the setting beside `stale_after_seconds`.

## Deferred

`AskUserQuestion` does not produce a `waiting_for_user` status or a
notification. Claude Code fires no `PermissionRequest` for it (it needs no
permission) and `idle_prompt` only fires after an idle threshold the user
usually beats by answering, so the only observed event is `PreToolUse`, which
`adapters/claude/src/lib.rs:92` maps to `Activity` → `Working`. Fixing it means
classifying `PreToolUse` as `InteractionRequired` for known blocking-prompt tool
names, which raises its own privacy-contract and cross-harness questions. It is
tracked as a separate spec.
