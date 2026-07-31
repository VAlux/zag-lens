# Blocking Prompt Tools as `waiting_for_user`

## Problem

When Claude Code calls `AskUserQuestion` or `ExitPlanMode`, the agent stops and
waits for the user. The tab shows `●` working, and no notification fires. The
user has no signal that their input is needed.

Claude Code emits no distinct lifecycle hook for either tool. `PermissionRequest`
does not fire, because neither tool requires permission. The `Notification` hook's
`idle_prompt` subtype fires only after an idle threshold that a user who answers
promptly beats. The sole observed event is `PreToolUse`, which
`adapters/claude/src/lib.rs:92` maps to `EventKind::Activity` → `Working`.

A blocking user-facing prompt is therefore indistinguishable from ordinary tool
activity.

## Goal

`AskUserQuestion` and `ExitPlanMode` decorate their tab with `?`
`waiting_for_user` for as long as they block, fire the same notification any
other interaction does, and return to `●` `working` once answered.

## Non-goals

- Any change to Codex. Its CLI has no equivalent tool, and its adapter's
  `is_supported_tool` allowlist already governs which tools it reports.
- Any change to OpenCode. Its plugin already emits `question.asked` →
  `interaction_required` and `question.replied` → `activity`.
- Any change to the reducer, the notification machinery, or the title layer.
  Each already handles this case; see Mechanisms Already In Place.
- A configurable tool list. The set is a property of Claude Code's tool surface,
  not of the user's preferences.
- Inspecting `tool_input`, tool results, or any assistant text. Only the stable
  `tool_name` identifier is read.

## Design

### Adapter

`classify` in `adapters/claude/src/lib.rs` splits the `PreToolUse` arm on the
tool name and leaves the post-tool arms alone:

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

with a helper mirroring the Codex adapter's existing `is_supported_tool`
(`adapters/codex/src/lib.rs:167`):

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
```

and an attention constructor beside the existing `permission_attention`:

```rust
fn question_attention() -> Attention {
    Attention {
        kind: "question".to_owned(),
        summary: Some("Claude Code requires an answer".to_owned()),
    }
}
```

### Optional, not required, `tool_name`

The Codex adapter reads `tool_name` with `required_identifier`, but its contract
demands the field. The Claude adapter ignores it today, so making it mandatory
would turn any `PreToolUse` lacking the field into a hard `AdapterError` where it
previously emitted `Activity`. Hook code must remain fail-open, so an absent
`tool_name` means "not a blocking prompt" and the event stays `Activity`.

A `tool_name` that is present but not a non-empty string still errors, matching
how `optional_identifier` already treats every other field.

### Attention kind

Kind `"question"`, matching OpenCode's `question.asked` and Claude's own
`idle_prompt` mapping. Both blocking tools share it: they are the same situation
from the user's point of view, and the shared kind means an `idle_prompt` and an
explicit question inside one turn collapse into a single alert rather than two.

The summary differs from `idle_prompt`'s "Claude Code is waiting for input"
because the triggers differ. Only the kind participates in deduplication.

### Mechanisms already in place

No change is needed to any of these; each was verified against the current code.

- **Resume.** `PostToolUse` carries a later `occurred_at` than its `PreToolUse`,
  and `is_out_of_order` (`crates/core/src/lib.rs:390`) compares timestamps before
  falling back to event precedence. The lower-precedence `Activity` is therefore
  applied, returning the tab to `working`. `bridge/src/lib.rs:53` formats
  `occurred_at` with subsecond precision, so the two events cannot collide.
- **Notification.** `plugin/src/attention.rs:404` notifies on
  `TransitionCause::Event(EventKind::InteractionRequired)` with state
  `WaitingForUser`, reading the attention kind for the dedup key and message.
- **Repeat questions.** `clear_if_resumed_or_terminal`
  (`plugin/src/attention.rs:522`) drops the outstanding interaction key when the
  agent returns to `Working`, so a second question in the same turn notifies
  again instead of being suppressed as a duplicate.
- **Priority.** `waiting_for_user` already outranks `working` in
  `state_priority` (`crates/core/src/lib.rs:488`), so the `?` wins its tab.

## Testing

In `adapters/claude/tests/adapter.rs`, against a new fixture
`tests/fixtures/claude/2.1.207/pre-tool-use-ask-user-question.json` shaped like
the existing `pre-tool-use.json` but with `tool_name` of `AskUserQuestion`:

1. `PreToolUse` for `AskUserQuestion` normalizes to `InteractionRequired` /
   `WaitingForUser` with attention kind `question`.
2. The same event with `tool_name` overridden to `ExitPlanMode` produces the
   same result. Overriding in-test follows the style of
   `stable_agent_id_is_used_when_present` and avoids a near-duplicate fixture.
3. `PreToolUse` for `Bash` (the existing fixture) still produces `Activity` /
   `Working`. This is the regression guard for every non-blocking tool.
4. `PreToolUse` with `tool_name` removed still produces `Activity` / `Working`,
   pinning the fail-open decision above.
5. The new fixture joins the `sensitive_native_fields_are_not_transported` loop,
   so the privacy assertions cover it.

## Documentation

- `SPECIFICATION.md:406` splits into two rows using the table's existing
  "Matcher or subtype" column: `PreToolUse` / `AskUserQuestion`, `ExitPlanMode`
  → `interaction_required` / `waiting_for_user`, and `PreToolUse` / other →
  `activity` / `working`.
- `docs/compatibility.md:28` gains a sentence naming the two tools and noting
  that the tool name is the only payload field consulted.
