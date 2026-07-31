# Blocking Prompt Tools Implementation Plan

> **For agentic workers:** Steps use checkbox (`- [ ]`) syntax for tracking. Implement task-by-task, running the verification commands as written.

**Goal:** Make `AskUserQuestion` and `ExitPlanMode` decorate their Zellij tab with `?` `waiting_for_user` and fire a notification while they block, instead of appearing as ordinary `●` `working` tool activity.

**Architecture:** Claude Code emits no distinct lifecycle hook for either tool — only `PreToolUse`. The Claude adapter's `classify` therefore splits its `PreToolUse` arm on the stable `tool_name` identifier, emitting `InteractionRequired` for the two blocking tools and `Activity` for everything else. Nothing downstream changes: the reducer, notification, and title layers already handle `InteractionRequired` correctly, and `PostToolUse` already restores `working`.

**Tech Stack:** Rust 2024 edition, `serde_json` for hook payloads, deterministic JSON fixtures under `tests/fixtures/claude/2.1.207/`.

## Global Constraints

- Rust 1.94.1, edition 2024, as pinned by `rust-toolchain.toml`.
- `unsafe_code = "forbid"`; `clippy::all` + `clippy::pedantic` at warn, enforced as `-D warnings` in CI.
- Adapter and hook code must remain fail-open and stdout-silent. A malformed or unexpected payload must never block the harness.
- Only stable lifecycle identifiers are read. Prompt text, transcript paths, `tool_input`, tool results, error details, and assistant messages are never inspected.
- Never derive agent state by scraping terminal output, transcripts, prompts, or assistant message text.
- The blocking tool set is exactly `AskUserQuestion` and `ExitPlanMode`.
- The attention kind is exactly `question`; the summary is exactly `Claude Code requires an answer`.
- `cargo fmt --all --check` must be clean.

---

### Task 1: Classify blocking prompt tools as `interaction_required`

**Files:**
- Modify: `adapters/claude/src/lib.rs` (`classify` at 88-113, helpers at 115-135)
- Create: `tests/fixtures/claude/2.1.207/pre-tool-use-ask-user-question.json`
- Modify: `adapters/claude/tests/adapter.rs` (fixture constants at 11-26, tests appended near the end)
- Modify: `SPECIFICATION.md:406`
- Modify: `docs/compatibility.md:28-32`

**Interfaces:**
- Consumes: existing private helpers `object_payload`, `optional_identifier`, and the `Classification { kind, attention }` struct, all in `adapters/claude/src/lib.rs`.
- Produces: no new public API. Two new private functions, `is_blocking_prompt_tool` and `question_attention`.

- [ ] **Step 1: Add the fixture**

Create `tests/fixtures/claude/2.1.207/pre-tool-use-ask-user-question.json`. It mirrors the existing `pre-tool-use.json` — same `session_id`, same `REDACTED` placeholders so the privacy assertions stay meaningful — differing only in `tool_name`, `tool_input`, and `tool_use_id`:

```json
{
  "session_id": "session-8",
  "transcript_path": "/workspace/REDACTED/transcript.jsonl",
  "cwd": "/workspace/project",
  "hook_event_name": "PreToolUse",
  "tool_name": "AskUserQuestion",
  "tool_input": {
    "questions": "REDACTED"
  },
  "tool_use_id": "toolu_fixture_3"
}
```

- [ ] **Step 2: Write the failing tests**

In `adapters/claude/tests/adapter.rs`, add the fixture constant alongside the others near line 11:

```rust
const PRE_TOOL_USE_ASK_USER_QUESTION: &str =
    include_str!("../../../tests/fixtures/claude/2.1.207/pre-tool-use-ask-user-question.json");
```

Append these tests to the end of the file. They use the existing `native`, `emitted`, and `context` helpers defined at lines 38, 44, and 28:

```rust
#[test]
fn blocking_prompt_tools_wait_for_the_user() {
    for tool_name in ["AskUserQuestion", "ExitPlanMode"] {
        let mut input = native("PreToolUse", PRE_TOOL_USE_ASK_USER_QUESTION);
        input.payload["tool_name"] = Value::String(tool_name.to_owned());

        let decision = ClaudeAdapter
            .normalize(&input, &context())
            .expect("blocking prompt tool must normalize");
        let AdapterDecision::Emit(event) = decision else {
            panic!("blocking prompt tool must emit");
        };

        assert_eq!(event.kind, EventKind::InteractionRequired, "{tool_name}");
        assert_eq!(event.state, CanonicalState::WaitingForUser, "{tool_name}");
        let attention = event.attention.expect("blocking prompt must carry attention");
        assert_eq!(attention.kind, "question", "{tool_name}");
        assert_eq!(
            attention.summary.as_deref(),
            Some("Claude Code requires an answer"),
            "{tool_name}"
        );
    }
}

#[test]
fn ordinary_tools_carry_no_attention() {
    // `lifecycle_fixtures_map_to_expected_states` already pins this fixture
    // (tool_name `Bash`) to activity/working. This adds only the half of the
    // contract that test does not assert.
    assert!(emitted("PreToolUse", PRE_TOOL_USE).attention.is_none());
}

#[test]
fn absent_tool_name_stays_activity() {
    let mut input = native("PreToolUse", PRE_TOOL_USE_ASK_USER_QUESTION);
    input
        .payload
        .as_object_mut()
        .expect("fixture is a JSON object")
        .remove("tool_name");

    let decision = ClaudeAdapter
        .normalize(&input, &context())
        .expect("a PreToolUse without tool_name must stay fail-open");
    let AdapterDecision::Emit(event) = decision else {
        panic!("supported event must emit");
    };

    assert_eq!(event.kind, EventKind::Activity);
    assert_eq!(event.state, CanonicalState::Working);
}
```

Then add the new fixture to the privacy loop in `sensitive_native_fields_are_not_transported` (the array starting at line 238), immediately after the existing `("PreToolUse", PRE_TOOL_USE),` entry:

```rust
        ("PreToolUse", PRE_TOOL_USE_ASK_USER_QUESTION),
```

Why each test exists:
- The first pins both tools and the full attention contract — kind and summary are what the notification layer reads.
- The second adds only what the existing suite lacks. `lifecycle_fixtures_map_to_expected_states` (line 67) already asserts `PreToolUse`/`PRE_TOOL_USE` → `Activity`/`Working` and is the real regression guard for non-blocking tools; it must stay green untouched. It does not check `attention`, so an implementation that wrongly attached attention to every `PreToolUse` while leaving the kind alone would slip past it. Do not restate the kind/state assertions here.
- The third pins the fail-open decision from the spec. Without it, someone "tidying" `optional_identifier` into `required_identifier` would turn a missing field into a hard `AdapterError` in hook code that must never block Claude.
- The privacy-loop addition ensures the new fixture's `REDACTED` markers and `tool_input` are asserted absent from the normalized output, exactly as for every other fixture.

- [ ] **Step 3: Run the tests to verify they fail**

```sh
cargo test -p zag-lens-claude-adapter
```

Expected: `blocking_prompt_tools_wait_for_the_user` fails on the first assertion — the adapter currently emits `EventKind::Activity` for every `PreToolUse`, so the event is `Activity`/`Working` with no attention. `ordinary_tools_carry_no_attention` and `absent_tool_name_stays_activity` should already pass, since they assert today's behavior; that is expected and correct, and both must still pass after Step 4.

- [ ] **Step 4: Implement the classification split**

In `adapters/claude/src/lib.rs`, replace the combined tool arm in `classify` (line 92):

```rust
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => (EventKind::Activity, None),
```

with:

```rust
        "PreToolUse" => {
            if is_blocking_prompt_tool(object_payload(&event.payload)?)? {
                (EventKind::InteractionRequired, Some(question_attention()))
            } else {
                (EventKind::Activity, None)
            }
        }
        "PostToolUse" | "PostToolUseFailure" => (EventKind::Activity, None),
```

Add the two helpers immediately after the existing `permission_attention` function (which ends at line 120):

```rust
/// Tools that block the turn on a user response. `PreToolUse` is the only
/// lifecycle signal Claude Code emits for them, so the stable tool name —
/// never the tool input — selects the waiting state.
fn is_blocking_prompt_tool(payload: &Map<String, Value>) -> Result<bool, AdapterError> {
    let Some(tool_name) = optional_identifier(payload, "tool_name")? else {
        return Ok(false);
    };
    Ok(matches!(tool_name.as_str(), "AskUserQuestion" | "ExitPlanMode"))
}

fn question_attention() -> Attention {
    Attention {
        kind: "question".to_owned(),
        summary: Some("Claude Code requires an answer".to_owned()),
    }
}
```

`optional_identifier` is deliberate, not an oversight: the Codex adapter's equivalent uses `required_identifier`, but its contract demands `tool_name`. This adapter has always ignored the field, so requiring it would turn a `PreToolUse` lacking it into a hard `AdapterError` in code that must stay fail-open. Do not "tighten" this.

Also update the module doc comment at the top of the file (lines 2-5) so it stays accurate — it currently says tool inputs are never inspected, which remains true, but it should now note that the tool name is read to identify blocking prompts. Add one sentence in the existing style; do not restructure the comment.

- [ ] **Step 5: Run the tests to verify they pass**

```sh
cargo test -p zag-lens-claude-adapter
```

Expected: PASS, including every pre-existing test in the file. `lifecycle_fixtures_map_to_expected_states` still asserts `PreToolUse`/`PRE_TOOL_USE` → `Activity`, and that fixture's `tool_name` is `Bash`, so it must remain green untouched. If it fails, the allowlist is wrong — fix the code, never the test.

- [ ] **Step 6: Update the specification table**

In `SPECIFICATION.md`, replace the single `PreToolUse` row at line 406:

```markdown
| `PreToolUse` | any | `activity` | `working` |
```

with two rows that use the table's existing "Matcher or subtype" column:

```markdown
| `PreToolUse` | `AskUserQuestion`, `ExitPlanMode` | `interaction_required` | `waiting_for_user` |
| `PreToolUse` | any other tool | `activity` | `working` |
```

- [ ] **Step 7: Update the compatibility guide**

In `docs/compatibility.md`, the Claude paragraph at lines 28-32 currently ends:

```markdown
`Stop`, `StopFailure`, and `SessionEnd`. Supported notification subtypes are
`permission_prompt`, `idle_prompt`, and `elicitation_dialog`; other subtypes are
ignored.
```

Append a sentence after `ignored.`, keeping the file's ~79-column wrapping:

```markdown
`PreToolUse` for the blocking prompt tools `AskUserQuestion` and `ExitPlanMode`
reports `waiting_for_user`; the tool name is the only payload field consulted,
and `PostToolUse` returns the tab to `working` once the user answers.
```

- [ ] **Step 8: Run the full verification suite**

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude zag-lens-plugin
cargo test -p zag-lens-plugin --bin zag_lens_plugin
cargo check -p zag-lens-plugin --target wasm32-wasip1
```

Expected: all five clean. `cargo test --workspace --exclude zag-lens-plugin` totalled 121 passing before this change and should total 124 after (three new tests); `cargo test -p zag-lens-plugin --bin zag_lens_plugin` should be 50. Report the real numbers.

- [ ] **Step 9: Commit**

```bash
git add adapters/claude/src/lib.rs adapters/claude/tests/adapter.rs \
    tests/fixtures/claude/2.1.207/pre-tool-use-ask-user-question.json \
    SPECIFICATION.md docs/compatibility.md
git commit -m "feat(claude): report blocking prompt tools as waiting_for_user"
```

---

## Manual verification

Optional. Rebuild and reinstall, restart Zellij, then in a Claude Code pane on an inactive tab trigger a question (any prompt that makes Claude call `AskUserQuestion`, or `/plan` then plan approval for `ExitPlanMode`). The tab should switch from `●` to `?` when the prompt appears, fire a notification under the default `waiting-only` policy, and return to `●` once answered.

## Out of scope

Codex (no equivalent tool; its `is_supported_tool` allowlist already governs what it reports) and OpenCode (already emits `question.asked` → `interaction_required` and `question.replied` → `activity`).
