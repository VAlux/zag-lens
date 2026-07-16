# Compatibility

The following versions are the implemented compatibility baselines. `doctor`
reports them but does not parse or enforce version ordering.

| Component | Baseline | Current declaration |
| --- | --- | --- |
| Rust | 1.94.1 | Pinned by `rust-toolchain.toml`. |
| Zellij | 0.44.1 | Plugin API dependency is pinned to 0.44.1. |
| Codex CLI | 0.144.3 | Adapter minimum; no maximum is declared. |
| Claude Code | 2.1.207 | Adapter minimum; no maximum is declared. |
| OpenCode | 1.17.15 | Local-TUI adapter minimum; no maximum is declared. |

Fixtures under `tests/fixtures/codex/0.144.3/` and
`tests/fixtures/claude/2.1.207/`, and
`tests/fixtures/opencode/1.17.15/` define the schemas covered by automated
tests. A newer harness release is not considered verified until a sanitized
fixture is added and adapter tests pass.

## Event Coverage

Codex setup registers `SessionStart`, `UserPromptSubmit`, `PreToolUse`,
`PostToolUse`, `PermissionRequest`, and `Stop`. Tool activity is currently
recognized for `Bash`, `apply_patch`, and `mcp__*` tools. Codex does not expose a
distinct lifecycle hook for every arbitrary assistant question, and Zag Lens
never parses assistant messages to infer one.

Claude setup registers `SessionStart`, `UserPromptSubmit`, `PreToolUse`,
`PostToolUse`, `PostToolUseFailure`, `PermissionRequest`, `Notification`,
`Stop`, `StopFailure`, and `SessionEnd`. Supported notification subtypes are
`permission_prompt`, `idle_prompt`, and `elicitation_dialog`; other subtypes are
ignored.

OpenCode setup installs a global, auto-loaded local plugin. It observes
`session.created`, `session.status`, `permission.*`, `question.*`, completed
assistant `message.updated`, `session.error`, and `session.deleted` events.
`session.idle` is ignored because it follows success, failure, and cancellation.
Only allowlisted identifiers, status/reply enums, completion flags, and error
names reach the native bridge.

OpenCode child sessions participate in normal per-tab aggregation. Local TUIs
started inside Zellij are supported; `opencode serve`, `opencode attach`, web or
desktop clients, and remote cross-pane routing are not yet supported.

Setup tracks main-session lifecycle hooks. A stable `agent_id` present on an
accepted event is retained and participates in normal tab aggregation, including
the optional same-state count. Dedicated `SubagentStart`/`SubagentStop` hooks
and separate subagent presentation are not registered in the MVP.

## Platform Support and Limits

Native builds and automatic desktop notifications target macOS and Linux on
x86_64 and ARM64. Other platforms can use the title feature, bell backend, or a
direct host CLI command if the Rust dependencies compile, but they are not part
of the current support matrix.

The automatic backend uses the built-in AppleScript implementation on macOS and
freedesktop notifications on Linux. The explicit `applescript` backend is
macOS-only.

Desktop notification delivery is best effort. Operating-system notification
settings, a denied Zellij `RunCommands` capability, or an unavailable desktop
service may suppress alerts without affecting tab status. The plugin knows only
whether a Zellij tab is active, not whether the terminal window has OS focus.

The bridge-to-plugin pipe is local and informational, not an authentication
boundary. Delivery is best effort; inactivity timeout, later lifecycle events,
and pane closure repair or clear most missing intermediate state.
