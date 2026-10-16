# Scripts and agents

dmsh is built to be driven by scripts and AI agents: anything can put an activity on
the islands, give it buttons, and wait for the answer. Everything here uses the CLI;
the same requests work over the socket from any language ([docs/ipc.md](ipc.md)).

## Showing progress

Push the same id to update it; dismiss it when done.

```bash
dmsh activity push --id backup --title "Backing up" --progress 0.1 --icon download --placement center
dmsh activity push --id backup --title "Backing up" --progress 0.7 --icon download --placement center
dmsh activity push --id backup --title "Backup finished" --icon check --placement flow --ttl 10s
```

The last push turns it into a notification-style item: center for two seconds, then
the right island, gone after ten.

## Notifying when something finishes

```bash
long-running-command; dmsh activity push --id done --title "Finished: long-running-command" --icon check
```

Without `--ttl`, it stays on the right island until dismissed.

## Asking a question and waiting

`--wait` blocks until a button is clicked, prints the button's key and exits 0. If the
activity is dismissed or expires first, it prints `dismissed` or `expired` and exits 1.

```bash
answer=$(dmsh activity push --id deploy --title "Deploy to production?" \
    --body "3 commits ahead of origin/main" --icon alert \
    --placement center --priority high \
    --action deploy:Deploy --action cancel:Cancel --ttl 2m --wait) || answer=none
[ "$answer" = deploy ] && ./deploy.sh
```

dmsh subscribes before pushing, so even an instant click isn't missed. The question can
be answered with the mouse, from the keyboard (`dmsh toggle center`, arrows, Enter), or
by another program (`dmsh activity invoke deploy cancel`).

## Watching everything

```bash
dmsh events
```

Prints every `action_invoked` and `removed` event as a JSON line. Useful for activities
with `--keep-on-action`, where buttons are pressed repeatedly.

## Recipe: Claude Code hooks

[Claude Code hooks](https://code.claude.com/docs/en/hooks) run commands on
events. In `~/.claude/settings.json`, these show when Claude wants your attention and
when it finishes:

```json
{
  "hooks": {
    "Notification": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "jq -j .message | xargs -0 dmsh activity push --id claude --title 'Claude Code' --icon terminal --placement center --priority high --ttl 30s --body"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "dmsh activity push --id claude --title 'Claude finished' --icon check" }
        ]
      }
    ]
  }
}
```

The hook receives its event as JSON on stdin; `jq` pulls out the message. A
`PreToolUse` hook can ask in the island before a tool runs, using `--wait` and the hook
exit codes described in Claude Code's documentation.

## Headless rendering

`dmsh render` runs the real layout and animation code on a simulated clock and writes
a PNG — no display or running shell needed. It's how to check a visual change, preview
a theme, or produce screenshots.

```bash
dmsh render --activities fixture.json --at 2500ms -o frame.png
dmsh render --activities fixture.json --at 3s --filmstrip 300ms --crop 560,1085 --background '#2c2c30' -o flow.png
dmsh render --activities fixture.json --hover right --battery 76+ --wifi on -o expanded.png
```

The fixture is a JSON array of activity specs (as in [docs/ipc.md](ipc.md#activity-spec));
an extra `at_ms` field delays an entry:

```json
[
  {"id": "build", "title": "cargo build", "progress": 0.6, "icon": "terminal", "placement": "center"},
  {"at_ms": 300, "id": "msg", "title": "Alice", "body": "Dinner tonight?", "icon": "message"}
]
```

| Option | Default | Meaning |
|---|---|---|
| `-o, --output` | `dmsh.png` | Where to write. |
| `--activities` | none | The fixture file. |
| `--at` | `3s` | How far into the simulation to render. |
| `--filmstrip INTERVAL` | — | Render a frame every interval up to `--at`, stacked vertically. |
| `--hover center\|right` | — | Expand an island; `--hover-at` says when the hover starts. |
| `--pointer X,Y` | — | Pointer position, for hover highlights on buttons. |
| `--battery 76` / `76+` | — | Show a battery (`+` = charging). |
| `--wifi on\|off` | — | Show the Wi-Fi quick control. |
| `--width`, `--scale` | `1645`, `1.75` | Logical output width and scale. |
| `--crop X,WIDTH` | — | Keep a logical x range. |
| `--background #rrggbb` | transparent | Fill behind the islands. |
| `--clock`, `--date` | `13:35`, `Thursday, 15 October` | Fixed clock text, for stable images. |
| `--config` (global) | defaults | Render with a config's theme and font. |

The screenshots in the README were rendered this way, from fixtures like the one above.
