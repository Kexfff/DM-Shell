# IPC protocol

The CLI is a thin client over a Unix socket; anything that can write a line of JSON to
a socket can drive dmsh.

- **Socket:** `$DMSH_SOCKET`, else `$XDG_RUNTIME_DIR/dmsh-$WAYLAND_DISPLAY.sock` (one
  shell per Wayland session).
- **Framing:** newline-delimited JSON. Send one request object per line; each gets one
  response line. A connection may send several requests. A final request without a
  trailing newline is answered when you close your write side.
- **Schema:** `dmsh schema` prints a JSON Schema covering every request, response and
  event.

```bash
echo '{"cmd":"push","id":"hi","title":"Hello"}' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/dmsh-$WAYLAND_DISPLAY.sock
```

```bash
dmsh msg '{"cmd":"state"}'
```

## Responses

```json
{"ok":{"type":"done"}}
{"error":"`perm` has no action `maybe`"}
```

Successful replies are `{"ok": {"type": ...}}` with type `done`, `subscribed`,
`dismissed` (`{"existed": bool}`), `state` (the state dump) or `version`.

## Requests

| `cmd` | Fields | What it does |
|---|---|---|
| `push` | an activity spec (below) | Create or update an activity. |
| `dismiss` | `id` | Remove an activity; replies `dismissed` with `existed`. |
| `clear` | `island`: `"right"` or `"center"` (optional) | Remove all activities, or one island's. |
| `invoke` | `id`, `key` | Invoke an action as if clicked; dismisses unless `keep_on_action`. |
| `state` | — | What the islands show and every live activity. |
| `subscribe` | — | Keep the connection open and stream events (below). |
| `control` | `control`: `"wifi"` or `"keep-awake"`, `state`: bool (optional, omitted = toggle) | Set a quick control. |
| `toggle` | `island`: `"center"` or `"right"` | Open the island with keyboard focus, or close it. |
| `reload` | — | Re-read the config file. |
| `version` | — | The dmsh version. |

### Activity spec

```json
{
  "cmd": "push",
  "id": "build",
  "title": "cargo build --release",
  "body": "Compiling dmsh-render",
  "progress": 0.4,
  "icon": "terminal",
  "placement": "center",
  "priority": "normal",
  "ttl_ms": 30000,
  "center_ms": 2000,
  "actions": [{"key": "cancel", "label": "Cancel"}],
  "keep_on_action": false
}
```

| Field | Default | Meaning |
|---|---|---|
| `id` | required | Stable identifier; pushing the same id updates the activity. |
| `title` | required | Main text. |
| `body` | — | Secondary text, shown when expanded. |
| `progress` | — | 0.0–1.0 (clamped); draws a bar. |
| `icon` | — | Built-in icon name; without one, a dot in the priority color. |
| `placement` | `flow` | `center`, `right`, or `flow` (center for `center_ms`, then right). |
| `priority` | `normal` | `low`, `normal`, `high`, `critical`. |
| `ttl_ms` | — | Lifetime after the **last** push; absent = until dismissed. |
| `center_ms` | `2000` | Center phase of a `flow` activity, counted from its **first** push. |
| `actions` | `[]` | Up to 8 `{key, label, icon?}`. `key: "default"` = click on the activity itself (not drawn as a button). Icon-only actions (empty label) render as a centered icon row. |
| `keep_on_action` | `false` | Stay after an action is invoked. |

Text fields are limited to 1024 bytes; at most 256 activities live at once.

### Which activity the center shows

1. Higher `priority` wins.
2. At equal priority, transient activities (with a `ttl_ms`, or `flow`) beat persistent
   ones — a volume overlay shows over the playing song.
3. Then the most recently pushed.

Transient center activities appear only on the focused monitor; other monitors fall
back to persistent ones or the clock. The right island shows the newest items (a
`flow` activity counts as new when it arrives there) and a count.

## Events

After `{"cmd":"subscribe"}` the connection replies `{"ok":{"type":"subscribed"}}` and
then streams one event per line:

```json
{"event":"action_invoked","id":"perm","key":"allow"}
{"event":"removed","id":"perm","reason":"dismissed"}
{"event":"removed","id":"osd","reason":"expired"}
```

`removed` follows `action_invoked` when the invoke dismissed the activity. Subscribers
that stop reading are dropped. `dmsh events` prints this stream.

## State

`{"cmd":"state"}` replies with `{"ok":{"type":"state", ...}}` containing:

- `view` — what the focused monitor shows: `center`, `right` (`items`, `total`), `clock`,
  `date`, `tray`, `battery`, `controls`, `menu`;
- `focused_output` — niri's focused monitor;
- `activities` — every live activity with its spec plus `island`, `age_ms` and
  `expires_in_ms`.
