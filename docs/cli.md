# Command line

`dmsh` with no command runs the shell. Every other command talks to the running shell
over its socket (see [docs/ipc.md](ipc.md)), except `config`, `icons`, `schema`, `niri`,
`service`, `claude-code`, `codex`, `mcp` (until a tool is called) and `render`, which work without it.

Global option: `--config <path>` picks the config file (default
`$XDG_CONFIG_HOME/dmsh/config.kdl`). `dmsh <command> --help` shows each command's help.

## Running

| Command | What it does |
|---|---|
| `dmsh` or `dmsh run` | Run the shell. Logs go to stderr; `RUST_LOG=debug` for more. |
| `dmsh reload` | Re-read the config (it also reloads on save). Fails with the error if the file is invalid. |
| `dmsh state` | Print what the islands show and every live activity, as JSON. |

## Activities

Anything on the islands is an *activity*. Pushing the same `--id` again updates it.

```bash
dmsh activity push --id ID --title TITLE [options]
```

| Option | Default | Meaning |
|---|---|---|
| `--body TEXT` | — | Secondary text, shown when the island expands. |
| `--progress 0.0–1.0` | — | A progress bar. |
| `--icon NAME` | dot | A built-in icon (`dmsh icons`). |
| `--image PATH\|NAME` | — | A picture instead of the icon: a PNG or SVG path, or an icon-theme name like `firefox`. Falls back to `--icon` if it can't be loaded. |
| `--app NAME` | — | Who it's from, shown above the title when expanded. |
| `--placement center\|right\|flow` | `flow` | `flow` shows in the center briefly, then moves to the right island. |
| `--priority low\|normal\|high\|critical` | `normal` | Higher priority wins the center. |
| `--ttl DURATION` | until dismissed | Removed this long after the last push, e.g. `1500ms`, `30s`, `5m`. |
| `--center-for DURATION` | `2s` | How long a `flow` activity stays in the center. |
| `--action key:Label` | — | A button; repeatable. The key `default` is invoked by clicking the activity itself. |
| `--keep-on-action` | off | Keep the activity after a button is clicked (otherwise it's dismissed). |
| `--wait` | off | Block until a button is clicked (print its key, exit 0) or the activity is removed (print `dismissed` or `expired`, exit 1). |

| Command | What it does |
|---|---|
| `dmsh activity dismiss ID` | Remove an activity. Exits 1 if there's no such activity. |
| `dmsh activity invoke ID KEY` | Invoke an action, as if its button was clicked. |
| `dmsh activity clear [--right]` | Remove all activities, or only the right island's. |
| `dmsh events` | Stream `action_invoked` and `removed` events as JSON lines until interrupted. |

## Islands and controls

| Command | What it does |
|---|---|
| `dmsh toggle center\|right` | Open the island on the focused monitor with keyboard focus, or close it. See [docs/niri.md](niri.md#keyboard-navigation). |
| `dmsh control wifi [on\|off\|toggle]` | Turn the Wi-Fi radio on or off (NetworkManager). |
| `dmsh control dnd [on\|off\|toggle]` | Do not disturb: notifications go straight to the right island; critical ones still show in the center. |
| `dmsh control keep-awake [on\|off\|toggle]` | Keep the session from going idle (screen off, lock, suspend). |

## Config

| Command | What it does |
|---|---|
| `dmsh config check` | Parse the config; print `ok` or the first error with its line (exit 1). |
| `dmsh config default` | Print the documented default config. |
| `dmsh config path` | Print where the config is read from. |

## Integration and tooling

| Command | What it does |
|---|---|
| `dmsh niri [--no-autostart]` | Print a niri config snippet: autostart and keybinds (with dmsh's full path); `--no-autostart` when the systemd service starts dmsh. |
| `dmsh claude-code [--apply\|--remove]` | Print, add or remove the Claude Code hooks and MCP server. See [docs/agents.md](agents.md#claude-code). |
| `dmsh service [--apply\|--remove]` | Print, install and enable, or remove the systemd user unit (restart on crash). See [docs/niri.md](niri.md#autostart-and-keybinds). |
| `dmsh codex [--apply\|--remove]` | Print, add or remove the Codex MCP server. See [docs/agents.md](agents.md#mcp-server). |
| `dmsh hook EVENT` | Handle a Claude Code hook: `working`, `notification`, `permission`, `stop`, `end` (reads the hook JSON on stdin). |
| `dmsh mcp` | Run the MCP server on stdio. See [docs/agents.md](agents.md#mcp-server). |
| `dmsh icons` | List the built-in icon names. |
| `dmsh schema` | Print the JSON Schema of the IPC protocol. |
| `dmsh msg 'JSON'` | Send one raw request line and print the raw response. |
| `dmsh render [options]` | Draw the islands to a PNG without a display. See [docs/agents.md](agents.md#headless-rendering). |

## Built-in icons

`bell`, `message`, `alert`, `check`, `x`, `terminal`, `download`, `globe`,
`screen-share`, `keyboard`, `music`, `play`, `pause`, `skip-back`, `skip-forward`,
`volume`, `volume-mute`, `brightness`, `battery`, `battery-low`, `battery-charging`,
`wifi`, `wifi-off`, `coffee`, `chevron-left`, `chevron-right`.
