# Installation

Zag Lens installs into the current user's XDG or `~/.local` directories. Setup
performs semantic KDL/JSON merges, preserves unrelated settings, creates backups
for changed configuration files, and writes an ownership manifest for
uninstall.

## Prerequisites

- Rust 1.94.1 with `rustfmt`, Clippy, and `wasm32-wasip1`
- Zellij 0.44.1 or newer
- Codex CLI 0.144.3 or newer, when enabling Codex hooks
- Claude Code 2.1.207 or newer, when enabling Claude hooks
- macOS or Linux for the automatic desktop notification backend

Run `zellij --version`, `codex --version`, and `claude --version` to inspect the
installed tools. After building Zag Lens, `target/release/zag-lens doctor`
reports these versions and every resolved installation path.

## Install a Release

Download the native archive for the host platform, the matching
`zag-lens-plugin-<version>.wasm`, and `SHA256SUMS` from the same GitHub release.
Verify the files before extracting the archive:

```sh
sha256sum --check SHA256SUMS       # Linux
shasum -a 256 --check SHA256SUMS  # macOS
```

Then run the extracted binary and pass the downloaded WASM asset:

```sh
./zag-lens setup --plugin-wasm ./zag-lens-plugin-0.1.0.wasm --dry-run
./zag-lens setup --plugin-wasm ./zag-lens-plugin-0.1.0.wasm
```

## Build and Setup from Source

```sh
cargo build --release -p zag-lens
cargo build --release -p zag-lens-plugin --target wasm32-wasip1

target/release/zag-lens setup \
  --plugin-wasm target/wasm32-wasip1/release/zag_lens_plugin.wasm \
  --dry-run
target/release/zag-lens setup \
  --plugin-wasm target/wasm32-wasip1/release/zag_lens_plugin.wasm
```

With no component flags, `setup` selects Zellij, Codex, and Claude. Select a
subset with `--zellij`, `--codex`, or `--claude`. `--plugin-wasm` is required
whenever Zellij is selected. Setup is idempotent; a repeated invocation reports
that configuration is already in the requested state.

The installer adds a `zag-lens` plugin alias and background load entry to
Zellij. It adds observational command hooks to the existing Codex and Claude
JSON configuration rather than replacing other hook groups.

After setup:

1. Confirm the generated Zellij alias contains the absolute `host_binary` path.
   Add the binary directory to `PATH` only if you later remove that setting.
2. Restart Zellij and approve `ReadApplicationState` and
   `ChangeApplicationState`.
3. Approve `RunCommands` to enable host notifications, or deny it to retain
   title-only operation.
4. Start Codex, open `/hooks`, inspect the Zag Lens commands, and trust them.

## Resolved Paths

| Asset | Environment override | Default |
| --- | --- | --- |
| Host executable | `XDG_BIN_HOME` | `~/.local/bin/zag-lens` |
| WASM and manifest | `XDG_DATA_HOME` | `~/.local/share/zag-lens/` |
| Zellij config | `XDG_CONFIG_HOME` | `~/.config/zellij/config.kdl` |
| Codex hooks | `CODEX_HOME` | `~/.codex/hooks.json` |
| Claude settings | `CLAUDE_CONFIG_DIR` | `~/.claude/settings.json` |

Existing configuration backups are placed next to the original file with a
`.zag-lens-backup-<timestamp>` suffix. Setup refuses ambiguous ownership,
externally modified owned entries, symlink replacement, and invalid KDL/JSON.

## Uninstall

Preview and remove all installer-owned entries and matching assets:

```sh
~/.local/bin/zag-lens uninstall --all --dry-run
~/.local/bin/zag-lens uninstall --all
```

Component flags are also accepted. Partial uninstall keeps the shared host
executable because another integration may still need it. Uninstall removes
only entries recorded in the ownership manifest and reports conflicts instead
of deleting externally modified content.

## Upgrade

Build or download the new native executable and WASM asset, verify release
checksums, then run the new executable with `setup` and the new `--plugin-wasm`
path. Setup updates installer-owned paths and assets atomically while preserving
unrelated configuration. Restart Zellij after a plugin upgrade. Keep the
generated backups until the new version has passed the live smoke test.
