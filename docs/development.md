# Development and Testing

The workspace separates harness-specific parsing from the protocol and reducer:

- `adapters/`: Codex, Claude Code, and OpenCode normalization
- `bridge/`: fail-open `zag-lens` host CLI and pipe transport
- `crates/protocol`: versioned normalized JSON types and validation
- `crates/core`: deterministic reducer, aggregation, and title formatting
- `crates/installer` and `crates/notifier`: host-side services
- `plugin/`: hidden Zellij runtime, title ownership, and attention policy
- `tests/fixtures/`: sanitized native and normalized payloads

## Automated Checks

Run the same baseline checks used by CI, plus native plugin unit tests:

```sh
cargo fmt --all --check
sh -n scripts/*.sh
shellcheck scripts/*.sh
sh scripts/test-install.sh
sh scripts/test-smoke-status-tabs.sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude zag-lens-plugin
cargo test -p zag-lens-plugin --bin zag_lens_plugin
cargo check -p zag-lens-plugin --target wasm32-wasip1
```

Useful focused commands are:

```sh
cargo test -p zag-lens-protocol
cargo test -p zag-lens-core
cargo test -p zag-lens-codex-adapter
cargo test -p zag-lens-claude-adapter
cargo test -p zag-lens-opencode-adapter
cargo test -p zag-lens-installer
cargo test -p zag-lens-notifier
cargo test -p zag-lens
```

Every adapter schema change requires a sanitized, versioned fixture. Do not add
prompt text, tool arguments, tool results, transcript paths, secrets, or full
native payload captures. A bug fix should include a regression test named for
observable behavior.

## Manual Live Smoke Test

To open one tab for each visible state without assertions or cleanup, run this
from inside Zellij:

```sh
./scripts/smoke-status-tabs.sh
```

The script leaves the tabs open for visual inspection. The successful state
still follows `success_ttl_seconds`, and the waiting state may emit a
notification under the configured notification policy.

1. Build and install using [installation.md](installation.md), then run
   `~/.local/bin/zag-lens doctor` (or the resolved XDG binary path).
2. Start a fresh Zellij session. Approve application-state permissions and, for
   desktop alerts, `RunCommands`.
3. In one tab, start Codex, inspect/trust Zag Lens with `/hooks`, and submit a
   turn. Confirm the tab changes to `●`, a policy-approved permission request
   changes it to `?` and alerts once, and completion shows `✓` for 30 seconds.
4. In another tab, start Claude Code and repeat start, harmless permission, and
   completion flows. Also confirm an explicit failure shows `×` and session end
   removes its status.
5. In a third tab, start the local OpenCode TUI. Confirm busy, permission,
   question, completion, failure, cancellation, and child-session events. A
   rejected question or user abort MUST clear without a completion alert.
6. Trigger only actions that already require approval under your harness policy;
   do not weaken security policy merely to create a prompt.
7. Run the harnesses concurrently in separate panes. Verify that each event
   updates only its owning tab, and that two agents in one tab follow
   `waiting > failed > working > succeeded > stale`.
8. Rename an active managed tab and confirm the new name remains the base title.
   Close the pane and then exit Zellij normally; decorations should be cleared.
9. Repeat after denying `RunCommands`. Titles must still change while desktop
   notifications remain absent.
10. Configure an animated `icons.working` JSON array beside a scalar state such
    as `icons.failed "x"`. Confirm working advances in order without skipped
    frames and failure remains unchanged.

Test a backend independently of Zellij with:

```sh
target/release/zag-lens notify --backend auto \
  --title "Zag Lens smoke" --body "Notification backend is reachable"
```

## Diagnostics

Hook execution is stdout-silent and returns success even for malformed input or
an unavailable Zellij plugin. Set `ZAG_LENS_DEBUG=1` or pass `hook --debug` to
emit a sanitized failure category on stderr. `notify --debug` similarly reports
sanitized backend failures. Never use debug output to print native payloads.
