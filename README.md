# Zag Lens

[![Release](https://github.com/VAlux/zag-lens/actions/workflows/release.yml/badge.svg)](https://github.com/VAlux/zag-lens/actions/workflows/release.yml)

![Example](screenshots/tab_bar_example.png)

Zag Lens is a background Zellij plugin that reports Codex and Claude Code agent
state in tab titles and notifies the user when an agent needs interaction. It
uses lifecycle hooks and a versioned JSON protocol; it does not scrape terminal
contents or agent transcripts.

This repository contains the Rust host executable, Zellij WASM plugin, adapters,
installer, notification backends, and deterministic test fixtures.

## Quick Start

Prebuilt releases support macOS and Linux on Intel and ARM. Installing one does
not require Rust or a source checkout. You need Zellij, `curl`, `tar`, and at
least one supported agent harness.

Review the
[installer script](https://github.com/VAlux/zag-lens/blob/main/scripts/install.sh),
then run:

```sh
curl -fsSL https://raw.githubusercontent.com/VAlux/zag-lens/main/scripts/install.sh | sh
```

The script detects the host platform, downloads the native binary and WASM
plugin from the [latest release](https://github.com/VAlux/zag-lens/releases/latest),
verifies both against `SHA256SUMS`, and runs the user-level installer.

By default, setup configures Zellij, Codex, and Claude Code while preserving
unrelated configuration. Then restart Zellij, approve the requested
application-state permissions, and inspect and trust the Zag Lens commands in
Codex with `/hooks`. Claude Code does not require a separate hook-trust step.

The installer places the host executable and WASM plugin in user-level XDG or
`~/.local` directories. Confirm the result with:

```sh
~/.local/bin/zag-lens doctor
```

## Updating

Rerun the installer to download and install the latest release:

```sh
curl -fsSL https://raw.githubusercontent.com/VAlux/zag-lens/main/scripts/install.sh | sh
```

The installer atomically replaces the native executable and WASM plugin while
preserving existing configuration and hooks. Setup is idempotent, so rerunning
it when Zag Lens is already current leaves the configuration unchanged. Restart
Zellij afterward to load the updated plugin.

## Tab Statuses

By default, Zag Lens prefixes the tab's existing title with the highest-priority
visible agent status:

| State               | Icon | Example          | Meaning                                        |
| ------------------- | ---- | ---------------- | ---------------------------------------------- |
| `working`           | `●`  | `● api-refactor` | The agent is processing a turn or using tools. |
| `waiting_for_user`  | `?`  | `? migrations`   | The agent needs user interaction.              |
| `succeeded`         | `✓`  | `✓ tests`        | The most recent turn completed successfully.   |
| `failed`            | `×`  | `× deploy`       | The most recent turn or session failed.        |
| `stale`             | `!`  | `! review`       | Activity stopped without a terminal event.     |
| `ready` / `stopped` | none | `project`        | No active status is displayed.                 |

Icons and the title format are configurable; see
[configuration](docs/configuration.md).

## Source-build Prerequisites

- Rust 1.94.1 managed by `rustup`
- the `rustfmt` and `clippy` components
- the `wasm32-wasip1` target
- Zellij 0.44.1 or newer for integration testing
- Codex CLI 0.144.3 and Claude Code 2.1.207 for live adapter smoke tests

The checked-in `rust-toolchain.toml` selects the required Rust components and
target automatically.

## Build and Install from Source

Build the native executable and WASM plugin:

```sh
cargo build --release -p zag-lens
cargo build --release -p zag-lens-plugin --target wasm32-wasip1
```

Preview the user-level configuration changes, then apply them:

```sh
target/release/zag-lens setup \
  --plugin-wasm target/wasm32-wasip1/release/zag_lens_plugin.wasm \
  --dry-run
target/release/zag-lens setup \
  --plugin-wasm target/wasm32-wasip1/release/zag_lens_plugin.wasm
```

Setup preserves unrelated Zellij and harness configuration and records its own
entries for safe uninstall. Restart Zellij after setup. In Codex, inspect and
trust the installed commands with `/hooks` before expecting events.

See [installation](docs/installation.md) and
[configuration](docs/configuration.md) for paths, component selection,
permissions, and uninstall instructions.

## Development

```sh
cargo fmt --all --check
sh -n scripts/install.sh scripts/test-install.sh
shellcheck scripts/install.sh scripts/test-install.sh
sh scripts/test-install.sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude zag-lens-plugin
cargo test -p zag-lens-plugin --bin zag_lens_plugin
cargo check -p zag-lens-plugin --target wasm32-wasip1
cargo build -p zag-lens-plugin --release --target wasm32-wasip1
```

Hook and adapter code must remain fail-open and stdout-silent. Never derive
agent state by scraping terminal output, transcripts, prompts, or assistant
messages.

The [development guide](docs/development.md) covers package-specific tests and
manual live smoke tests. See [compatibility](docs/compatibility.md) for the
tested versions, event coverage, and current limitations. `SPECIFICATION.md`
defines the behavior contract; `PLAN.md` tracks implementation phases.

## Privacy and Permissions

The plugin requests `ReadApplicationState` and `ChangeApplicationState` for tab
mapping and titles. It requests `RunCommands` only when notifications are
enabled. Denying that optional permission leaves title status operational.

Zag Lens transports normalized lifecycle metadata, never full prompts, tool
arguments, tool results, command output, or transcripts. Notification text is
sanitized and bounded before delivery.

## Release Artifacts

Tags named `v<workspace-version>`, for example `v0.1.0`, publish four native
archives and one portable Zellij plugin:

- `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin` and `aarch64-apple-darwin`
- `zag-lens-plugin-<version>.wasm`

Each release includes `SHA256SUMS`. The installer script filters that manifest
to the native archive and WASM asset for the current platform and verifies both
before installation.

The release workflow rejects a tag whose version does not exactly match the
workspace version in `Cargo.toml`.

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at your
option.
