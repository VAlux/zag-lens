# Zag Lens

Zag Lens is a background Zellij plugin that reports Codex and Claude Code agent
state in tab titles and notifies the user when an agent needs interaction. It
uses lifecycle hooks and a versioned JSON protocol; it does not scrape terminal
contents or agent transcripts.

This repository contains the Rust host executable, Zellij WASM plugin, adapters,
installer, notification backends, and deterministic test fixtures.

## Prerequisites

- Rust 1.94.1 managed by `rustup`
- the `rustfmt` and `clippy` components
- the `wasm32-wasip1` target
- Zellij 0.44.1 or newer for integration testing
- Codex CLI 0.144.3 and Claude Code 2.1.207 for live adapter smoke tests

The checked-in `rust-toolchain.toml` selects the required Rust components and
target automatically.

## Build and Install

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
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude zag-lens-plugin
cargo test -p zag-lens-plugin --lib
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

Each release includes `SHA256SUMS`. Verify downloads before installation:

```sh
sha256sum --check SHA256SUMS  # Linux
shasum -a 256 --check SHA256SUMS  # macOS
```

Extract the archive for the current platform, then pass the separately
downloaded WASM asset when configuring Zellij:

```sh
./zag-lens setup --plugin-wasm ./zag-lens-plugin-0.1.0.wasm
```

The release workflow rejects a tag whose version does not exactly match the
workspace version in `Cargo.toml`.

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at your
option.
