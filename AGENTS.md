# Repository Guidelines

## Project Structure & Module Organization

This repository is currently in the specification phase. `SPECIFICATION.md` is
the source of truth for behavior, architecture, event schemas, and acceptance
criteria. Keep implementation boundaries aligned with that document:

- `plugin/`: hidden Zellij WASM plugin, state reducer, and tab decoration.
- `bridge/`: fail-open host executable invoked by harness hooks.
- `adapters/`: Codex and Claude Code event normalization.
- `tests/fixtures/`: sanitized native and normalized event payloads.
- `docs/`: installation, configuration, and compatibility notes.

Do not combine harness-specific payload parsing with the core plugin reducer.

## Build, Test, and Development Commands

No build system has been committed yet. A change introducing executable code
must add reproducible build, format, lint, and test commands to the README and
CI in the same pull request. Until then, useful environment checks are:

```sh
zellij --version
codex --version
claude --version
```

Reference versions are Zellij 0.44.1, Codex CLI 0.144.3, and Claude Code
2.1.207; minimums remain undecided.

## Coding Style & Naming Conventions

Wrap Markdown near 80 columns and keep normative requirements explicit with
`MUST`, `SHOULD`, or `MAY`. For Rust, use `rustfmt` defaults and require clean
`clippy` output. Name modules, files, functions, and protocol fields in
`snake_case`; use `PascalCase` for Rust types and native hook names such as
`PermissionRequest`. Keep protocol changes backward-compatible and versioned.

## Testing Guidelines

Prioritize deterministic unit tests for adapters, state transitions,
aggregation, deduplication, title restoration, and notification sanitization.
Store sanitized payloads as fixtures and name tests by observable behavior, for
example `permission_request_transitions_to_waiting`. Integration tests should
cover bridge-to-pipe delivery and multi-pane Zellij mapping. Every bug fix must
include a regression test. No coverage threshold is defined yet.

## Commit & Pull Request Guidelines

There is no Git history from which to infer conventions. Use concise,
imperative Conventional Commit subjects, such as `feat: add codex adapter` or
`fix: restore renamed tab title`. Pull requests should explain user-visible
behavior, reference the relevant specification section, list verification
commands, and disclose protocol or permission changes. Include screenshots only
for visible tab-title or notification changes.

## Architecture, Security & Agent Instructions

Use lifecycle hooks and the `zag-lens:event` Zellij pipe; never infer state by
scraping terminal output, transcripts, or assistant text. Keep hook handlers
fail-open and stdout silent. Do not transport secrets, prompts, tool arguments,
or raw results. For durable project knowledge, always use the explicit Pieria
profile `zag-lens`; never rely on the gateway's default profile.
