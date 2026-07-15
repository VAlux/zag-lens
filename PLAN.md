# Zag Lens Implementation Plan

## Summary

Implement `SPECIFICATION.md` as an all-Rust workspace containing a Zellij WASM
plugin, host bridge, Codex and Claude Code adapters, notification backends, and a
user-level installer.

All phases begin as `planned`. Multiple phases may be `in progress`
simultaneously when their dependencies are complete.

### Parallel execution waves

1. P0
2. P1, P7, and P8 in parallel
3. P2-P6 in parallel
4. P9 and P10 in parallel
5. P11
6. P12

## Interfaces and Project Layout

Use Rust 1.94.1, edition 2024, and `wasm32-wasip1`. Pin `zellij-tile` 0.44.1 and
declare Zellij 0.44.1, Codex 0.144.3, and Claude Code 2.1.207 as the initial
compatibility baseline.

Workspace components:

- `crates/protocol`: normalized event schema and validation
- `crates/core`: reducer, aggregation, deduplication, title logic
- `crates/notifier`: macOS/Linux, command, bell, and off backends
- `crates/installer`: configuration merging and ownership tracking
- `adapters/codex` and `adapters/claude`: native-event projections
- `bridge`: native `zag-lens` executable
- `plugin`: Zellij WASM plugin
- `tests/fixtures`: versioned harness and protocol fixtures

Public CLI:

```text
zag-lens hook --harness <codex|claude> --event <native-event>
zag-lens notify --backend <auto|command|bell|off> --title ... --body ...
zag-lens setup [--all|--zellij|--codex|--claude] [--dry-run]
zag-lens uninstall [component selectors]
zag-lens doctor
```

Hook commands read one JSON document from stdin, keep stdout empty, use a short
transport timeout, and always fail open. Custom notification commands execute as
argv without a shell; configured prefix arguments are followed by sanitized
title and body arguments.

## Phase Plan

| ID | Phase | Status | Dependencies |
| --- | --- | --- | --- |
| P0 | Repository foundation | `done` | None |
| P1 | Protocol contract and fixtures | `done` | P0 |
| P2 | Core state engine | `done` | P1 |
| P3 | Host bridge and transport | `done` | P1 |
| P4 | Codex adapter | `done` | P1 |
| P5 | Claude Code adapter | `done` | P1 |
| P6 | Zellij runtime substrate | `done` | P1 |
| P7 | Notification backends | `done` | P0 |
| P8 | Installer and configuration engine | `done` | P0 |
| P9 | Tab-title lifecycle integration | `done` | P2, P6 |
| P10 | Attention notification integration | `done` | P2, P6, P7 |
| P11 | End-to-end assembly and packaging | `done` | P3, P4, P5, P8, P9, P10 |
| P12 | Hardening and release | `in progress` | P11 |

### P0 - Repository foundation

Create the Cargo workspace, pinned toolchain, formatting and lint configuration,
dual MIT/Apache-2.0 licenses, CI skeleton, README commands, and empty component
crates.

Exit criteria:

- Native workspace checks compile.
- The plugin skeleton compiles for `wasm32-wasip1`.
- `cargo fmt --all --check` and Clippy run in CI.

### P1 - Protocol contract and fixtures

Implement schema-version-1 types, validation, payload limits, adapter trait,
timestamp/event-ID handling, and fixture loaders. Preserve unknown fields but
reject unsupported schema versions.

Resolve command-notifier serialization with `notification_command` and a JSON
argv-prefix setting. Main sessions are supported in the MVP; subagent fields
remain protocol-compatible but are not displayed independently.

Exit criteria: valid, malformed, oversized, duplicate, and future-version
fixtures have deterministic tests.

### P2 - Core state engine

Implement the pure reducer, terminal-state protections, out-of-order handling,
instance deduplication, inactivity transitions, tab aggregation priority, TTL
expiry, title formatting, and bounded storage.

Exit criteria: exhaustive unit tests cover every transition, aggregation order,
duplicate event, and timer boundary.

### P3 - Host bridge and transport

Implement hook stdin/environment capture, adapter dispatch, normalized
serialization, Zellij session discovery, named-pipe invocation, timeout
enforcement, sanitized debug diagnostics, and fail-open exit behavior.

Exit criteria: fake Zellij executables verify argv, payload, timeout,
missing-session, and transport-failure behavior.

### P4 - Codex adapter

Map the specified Codex lifecycle events without inspecting transcripts or
assistant output. Use pane/session environment metadata and project only
allow-listed fields.

Exit criteria: every supported native event has a versioned fixture; unsupported
events are ignored safely.

### P5 - Claude Code adapter

Map Claude lifecycle, permission, notification, failure, and session-end hooks.
Treat permission and elicitation notifications as interaction requests without
producing hook decisions.

Exit criteria: fixture coverage demonstrates all specified states and silent
fail-open handling.

### P6 - Zellij runtime substrate

Implement the hidden plugin, permissions, `zag-lens:event` pipe subscription,
payload validation, `PaneUpdate`/`TabUpdate` mapping, two-second unmapped-event
queue, pane cleanup, timers, and bounded diagnostics.

Exit criteria: plugin-level tests cover parsing, mapping timeout, permission
denial, pane closure, and malformed input.

### P7 - Notification backends

Implement a notifier trait with:

- `auto`: native macOS Notification Center or Linux freedesktop notification
- `command`: executable plus argv prefix, without a shell
- `bell`: best-effort terminal bell
- `off`: no operation

Sanitize control sequences, cap field lengths, and isolate backend failures.

Exit criteria: fake-backend tests cover emission, sanitization, asynchronous
failure, and unavailable desktop services.

### P8 - Installer and configuration engine

Implement user-level, idempotent setup for:

- Zellij plugin alias and background loading
- Codex hook configuration
- Claude Code hook configuration

Use `$XDG_BIN_HOME`/`$XDG_DATA_HOME` with `~/.local` fallbacks. Parse KDL/JSON
semantically, preserve unrelated entries, create timestamped backups, record
owned changes in an install manifest, support dry runs, and remove only owned
entries during uninstall. Do not automatically trust Codex hooks; document the
required review step.

Exit criteria: temporary-HOME tests prove repeatability, conflict reporting,
backup recovery, and non-destructive uninstall.

### P9 - Tab-title lifecycle integration

Connect the reducer to pane/tab ownership and title commands. Preserve user
renames, coalesce unchanged titles, apply success TTLs, retain failures, and keep
a best-effort `/data` journal for restoration during the plugin lifecycle.

Exit criteria: tests cover multiple panes, multiple harnesses, custom titles,
reload repair, shutdown restoration, and aggregation.

### P10 - Attention notification integration

Connect successful state transitions to notification policy, active-tab
suppression, and deduplication. Default to `waiting-only` and `inactive-tab`; use
`(agent instance, turn, attention kind)` as the outstanding-interaction
deduplication key.

Exit criteria: each waiting transition emits at most once, completion policy is
opt-in, and denied `RunCommands` leaves title updates operational.

### P11 - End-to-end assembly and packaging

Assemble the CLI, adapter registry, plugin, installer, and notifier. Add isolated
Zellij integration tests, installation documentation, configuration examples,
diagnostics guidance, and a compatibility table.

Exit criteria: the ten MVP acceptance criteria in `SPECIFICATION.md` pass,
including concurrent Codex/Claude sessions and absence of output scraping.

### P12 - Hardening and release

Run malformed-input, load, privacy, stale-session, abrupt-exit, and
capability-denial tests. Verify 100 tracked agents and 20 events per second
without observable input latency.

Publish v0.1.0 through GitHub Actions as:

- macOS x86_64 and ARM64 native archives
- Linux x86_64 and ARM64 native archives
- portable Zellij WASM asset
- SHA-256 checksums, licenses, compatibility notes, and upgrade/uninstall
  instructions

## Verification and Status Rules

Required checks:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude zag-lens-plugin
cargo check -p zag-lens-plugin --target wasm32-wasip1
cargo build -p zag-lens-plugin --release --target wasm32-wasip1
```

A phase moves from `planned` to `in progress` when implementation begins. It
becomes `done` only after its exit criteria pass and its documentation is
updated. Failed or incomplete checks keep the phase `in progress`; dependent
phases may not begin.

## Assumptions

- `PLAN.md` is the living implementation tracker.
- MVP installation manages user-level configuration only.
- Custom tab-bar rendering and visible subagent counts are post-MVP.
- Desktop support is limited to macOS and Linux.
- Desktop notification failure never affects agent state, title updates, or
  harness exit status.
- `SPECIFICATION.md` remains authoritative when this plan and the protocol
  disagree.
