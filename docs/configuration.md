# Configuration

The installer creates the Zellij plugin alias. Add settings as child nodes of
that alias in the resolved `config.kdl`; Zellij supplies them to the plugin as
string values.

```kdl
plugins {
    zag-lens location="file:/home/alice/.local/share/zag-lens/zag-lens.wasm" {
        host_binary "/home/alice/.local/bin/zag-lens"
        notification_policy "waiting-only"
        notification_focus "inactive-tab"
        notification_backend "auto"
        icon_set "unicode"
        icons.working "[\"◐\",\"◓\",\"◑\",\"◒\"]"
        animation_interval_ms "250"
        success_ttl_seconds "30"
    }
}
```

Use absolute paths for `location` and `host_binary`. If `host_binary` is absent,
the plugin invokes `zag-lens` through the environment inherited by Zellij.

## Settings

| Key | Default | Accepted behavior |
| --- | --- | --- |
| `enabled` | `true` | Enables pipe event processing. |
| `title_format` | `{icon} {title}` | Must contain both placeholders. |
| `icon_set` | `unicode` | `unicode` or `ascii`; other values use Unicode. |
| `icons.<state>` | built in | Static string or JSON frame array for `working`, `waiting_for_user`, `succeeded`, `failed`, or `stale`. |
| `animation_interval_ms` | `250` | Shared animation delay; `100` through `60000`. |
| `show_counts` | `false` | Adds a count when multiple agents contribute the winning state. |
| `success_ttl_seconds` | `30` | `0` through `86400`. |
| `stale_after_seconds` | `1800` | `1` through `604800`. |
| `mapping_timeout_ms` | `2000` | `1` through `60000`. |
| `notification_policy` | `waiting-only` | `waiting-only`, `waiting-and-complete`, or `off`. |
| `notification_focus` | `inactive-tab` | `inactive-tab`, `always`, or `never`. |
| `notification_backend` | `auto` | `auto`, `applescript`, `command`, `bell`, or `off`. |
| `notification_command` | none | Trusted executable required by the `command` backend. |
| `notification_command_args` | `[]` | JSON array containing up to 64 fixed arguments. |
| `include_message_details` | `false` | Includes only the adapter's normalized coarse summary. |
| `max_payload_bytes` | `65536` | `1` through `65536`; the protocol hard limit remains 64 KiB. |
| `debug` | `false` | Retains bounded, sanitized plugin diagnostics. |
| `host_binary` | `zag-lens` | Host executable used for asynchronous notifications. |

Invalid values fall back to safe defaults. An invalid notification setting does
not disable title updates.

## Icon Animation

Icon animation is opt-in through the icon value itself; there is no separate
enable flag. An ordinary string remains static, as does a JSON array containing
one frame. A JSON array with two or more string frames animates independently
for each managed tab:

```kdl
icons.working "[\"◐\",\"◓\",\"◑\",\"◒\"]"
icons.waiting_for_user "?"
icons.succeeded "[\"✓\"]"
animation_interval_ms "250"
```

Arrays are supported for every visible state. Frames may contain Unicode or
multiple characters. Prefer frames with equal display width so the tab bar does
not jitter as the icon changes. Counts and `title_format` are applied to every
frame in the same way.

An empty array, empty frame, non-string array element, or malformed value that
starts like a JSON array is rejected for that state. The plugin records only a
sanitized configuration diagnostic and uses the selected Unicode or ASCII
built-in icon instead. An invalid `animation_interval_ms` similarly falls back
to `250`. Built-in icon sets and scalar overrides remain entirely static.

Animation timing is best-effort. The configured interval is a minimum delay
measured after Zellij acknowledges the previous frame, so a delayed timer never
causes skipped frames.

`waiting-only` emits once for each outstanding interaction. Duplicate Claude
`PermissionRequest` and `permission_prompt` events are deduplicated by agent,
turn, and coarse interaction kind. `inactive-tab` suppresses delivery when the
affected Zellij tab is active; it does not detect operating-system window focus.

On macOS, `auto` uses the built-in AppleScript backend. Select it explicitly
with one setting when desired; it requires no executable or argument
configuration:

```kdl
notification_backend "applescript"
```

The backend invokes `/usr/bin/osascript` directly without a shell and passes the
sanitized title and body as separate arguments to constant script source. It is
unsupported on non-macOS platforms.

Configure a custom command backend with an executable and a JSON argv prefix:

```kdl
notification_backend "command"
notification_command "/usr/bin/my-notifier"
notification_command_args "[\"--urgency=normal\",\"literal value\"]"
```

The plugin forwards these values to the host CLI. The sanitized notification
title and body are appended after the fixed arguments. Commands execute
directly as an argv array without a shell. Configuration is rejected safely if
the executable is empty, an argument contains NUL, there are more than 64
arguments, or one argument exceeds 1024 characters.

The same backend can be tested directly:

```sh
zag-lens notify --backend command --command /usr/bin/my-notifier \
  --command-arg --fixed-option --title "Zag Lens" --body "Test"
```

## Privacy and Permissions

`include_message_details` never reads native prompt or transcript text. Current
adapters generate only constant summaries such as “requires permission.” Every
title and body is stripped of terminal control sequences and length-limited.

Title operation needs `ReadApplicationState` and `ChangeApplicationState`.
Notification delivery additionally needs `RunCommands`. Set the notification
policy to `off`, focus to `never`, or backend to `off` to avoid requesting the
optional command permission.
