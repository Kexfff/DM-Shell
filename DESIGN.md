# dmsh — design

A minimal shell for [niri](https://github.com/YaLTeR/niri): two "dynamic islands" at the
top of every monitor that show only what is relevant right now. Not a toolkit — one
purpose-built program. Light, fast, and controllable by AI agents.

## Scope

dmsh is the shell (islands, notifications, OSDs, tray, battery, quick controls) for a
**bare niri config or the user's own**. It doesn't assume any other shell or desktop
layer is installed. Desktop pieces — polkit agent, lock screen and idle policy,
wallpaper, clipboard history, launcher, screenshot tools — are out of scope and set up
separately (install scripts). If another shell is running, dmsh takes over notifications
and the tray when it exits, but running on top of another shell isn't a goal.

## Goals and budgets

| Budget | Target |
|---|---|
| Resident memory | < 20 MB |
| CPU when nothing changes | 0 % (no redraws, no polling) |
| Time to first frame | < 50 ms |

These are enforced, not aspirational: regressions are bugs.

## The two islands

Both are solid black, Apple-style pills (the first and default theme).

- **Center island** — the single most important thing right now. Idle content: clock.
  Expands (floating over windows) on hover/click. When media is playing or paused, the
  expanded center shows media controls.
- **Right island** — less important, ambient things: chat messages, Wi-Fi changes, the
  system tray, and the battery (always visible on laptops: a fill-level icon and percent,
  orange when low, red when critical, green bolt while charging). Shows the newest item
  plus a `+N` counter; expands into a header row — round icon buttons for the **quick
  controls** (Wi-Fi on/off via NetworkManager, "Keep awake" via the Wayland idle-inhibit
  protocol), a power button for the **session menu** (Shut down, Reboot, Log out —
  each runs only when held until it fills, ~0.8 s, mouse or Enter; commands
  configurable), tray icons, battery — and the list. Dark monochrome tray icons (from icon
  themes made for light panels) are tinted light so they show on the black pill.
  Critical battery also takes the center until charging. **Do not disturb** sends `flow`
  activities below critical straight to the right island (a moon shows there). The list
  shows 6 rows and scrolls (wheel, or "N newer" / "N more" rows that the keyboard can
  reach); the view carries up to 32 items.

### Placement and lifecycle

Every piece of content is an **Activity** with a placement and a lifecycle:

| Example | Lifecycle |
|---|---|
| Notification | center for 2 s → slides to the right island → stays until read/dismissed |
| Wi-Fi connected | right island → fades after a few seconds |
| Volume / brightness | center for ~1.5 s → gone (never goes right) |
| Agent needs permission | center, sticky until handled |
| Media playing | center (compact), media controls when expanded |

The **arbiter** in `dmsh-core` decides what each island shows from the set of live
activities. It is pure logic with no I/O, so it is unit-testable without a display.

### Multi-monitor

- One surface per monitor, all fed by one shared state.
- Transient center events (notifications, OSDs) appear on the **focused monitor only**
  (niri IPC tells us which). The right island is mirrored on every monitor.

## Rendering model

- One **full-width, transparent layer-shell surface per output**, anchored top, on the
  `top` layer. Both islands live in this one buffer, so the center→right slide is a plain
  in-buffer animation, not a handoff between windows.
- **Exclusive zone = thin strip** (windows tile below it). It never changes, so windows
  never move.
- **The canvas is sized to its content**: strip height while collapsed (≈ 0.7 MB per
  buffer at 1645 px / 1.75×), taller only while an island floats over windows. Growing
  costs one configure round trip (about a frame), so it happens *before* an expand
  animation starts and shrinking happens *after* a collapse finishes — the canvas never
  resizes mid-animation.
- Each output has its own buffer pool; after a collapse, an oversized pool is swapped for
  a small one so the memory is actually returned.
- **Input region = the pill shapes only**; clicks elsewhere fall through to windows.
- CPU rasterization with `tiny-skia` into `wl_shm` buffers; text with `cosmic-text`
  (shaping, wrapping, ellipsis, color emoji). Fonts come from fontconfig: Inter if
  installed, else the system sans-serif, plus the emoji font — nothing else is loaded.
  Shaped text is cached, so animation frames only blend cached glyphs.
- Built-in stroke icons (24×24 path data, `dmsh icons` lists them).
- **Animation**: island geometry follows springs (response 0.42 s, damping 0.78 — a
  little overshoot); content cross-fades when an island shows a different thing. A
  `flow` activity leaving the center *travels* to the right island: the right island
  flies out of the center if it was hidden, otherwise a ghost pill carries the content
  over. Frames are driven by `wl_surface.frame` callbacks only while something moves;
  when everything settles, no callbacks are requested and CPU returns to zero.
- **Hover** expands an island (pointer enter → expanded content; leave → collapse). The
  idle center expands to a large clock with the date; an activity shows its body (3
  lines) and full-width progress; the right island becomes a list of up to 6 items.
- Fractional scaling via `wp_fractional_scale_v1` + `wp_viewporter` (integer
  `buffer_scale` fallback), so pills are crisp at e.g. 1.75×.

## Architecture

```
crates/
  dmsh-core     pure logic: geometry, theme, layout, (later) activities, arbiter, springs
  dmsh-render   scene → pixels with tiny-skia; shared by Wayland and headless PNG output
  dmsh          binary: Wayland loop (sctk + calloop), (later) IPC server + CLI client
  dmsh-sources  (later) niri, MPRIS, notifications, UPower, audio, network, tray
```

Stack: Rust, `smithay-client-toolkit`, `calloop`, `tiny-skia`; later `cosmic-text`
(text), `resvg` (icons), `zbus` (D-Bus), `niri-ipc`, KDL config.

### Sources

`dmsh-sources` runs on its own thread (single-threaded tokio runtime) so the Wayland
thread never waits on D-Bus or subprocesses. Sources speak the same `Request`s as IPC
clients, plus focused-output and clock events. A source that can't start logs a warning;
the rest keep running. Nothing polls: every source is event-driven.

| Source | Mechanism | Shows |
|---|---|---|
| niri | event stream on `$NIRI_SOCKET` | focused output; keyboard layout OSD; "Screen is being shared" (right, while a cast is active) |
| clock | one wakeup per minute boundary | idle center text (`view.clock`) |
| volume | `pactl subscribe`, then `wpctl get-volume` only on sink/server events | center OSD, 1.5 s |
| brightness | inotify on `/sys/class/backlight/*/brightness` | center OSD, 1.5 s |
| media | MPRIS on the session bus (playerctld ignored) | center, low priority; newest playing player, else newest paused (kept 5 min) |
| power | UPower `DisplayDevice` | right: charging / on battery / fully charged; battery low ≤ 20 % (flow, high), critical ≤ 5 % (center, critical) |
| network | NetworkManager `PrimaryConnection`, settled for 1.5 s | right: "Wi-Fi: <name>", "Disconnected" |
| notifications | dmsh owns `org.freedesktop.Notifications` | flow activity per notification; urgency → priority; actions; app name; picture from `image-data` → `image-path` → app icon → `icon_data`; markup stripped to plain text; emits `ActionInvoked` / `NotificationClosed` with the right reason |
| tray | StatusNotifierItem host (or watcher) + dbusmenu | icons on the right island, menus inside it |

- **Notifications**: only one daemon can own the name. Stop mako/swaync/a Quickshell
  notification server to use dmsh's; otherwise dmsh warns and runs without it.
- **System tray**: StatusNotifierItem host. If another program owns
  `org.kde.StatusNotifierWatcher` (e.g. a Quickshell bar), dmsh registers with it as a
  host; otherwise dmsh runs the watcher itself. Icons come from the item's pixmap or the
  icon theme (GTK `gtk-icon-theme-name`, its `Inherits`, hicolor, pixmaps), drawn as
  PNG/SVG at exact pixel size. Icons sit at the end of the right island; left click
  activates (menu-only items open their menu), right click opens the item's
  `com.canonical.dbusmenu` menu *inside the right island* (submenus navigate in place,
  leaving the island closes it), middle click is secondary activate.

## AI-agent friendliness

1. **Agents drive the shell** (primary). Claude Code hooks (`dmsh hook`, set up with
   `dmsh claude-code --apply`) show each session's status and route permission prompts
   to Allow / Deny buttons; `dmsh mcp` gives MCP clients `show_status`, `notify`,
   `ask_user` and `clear_status`. Underneath, everything is the socket protocol: A Unix socket with newline-delimited JSON (same
   style as niri's IPC) and a CLI over it, e.g.
   `dmsh activity push --id build --title "cargo build" --progress 0.4 --placement center --ttl 30s`.
   JSON Schema generated from the Rust types; an MCP wrapper can come later. Example use:
   Claude Code hooks push "needs permission" / "task done" to the island.
   Implemented today:
   ```sh
   dmsh activity push --id build --title "cargo build" --progress 0.4 --placement center
   dmsh activity push --id perm --title "Claude needs permission" --placement center --priority critical --icon alert
   dmsh activity dismiss build
   dmsh state                                   # JSON: view + every live activity
   dmsh msg '{"cmd":"push","id":"x","title":"Hi","ttl_ms":3000}'   # raw protocol
   dmsh schema                                  # JSON Schema of requests/responses
   ```
   Actions turn an activity into a question an agent can wait on:
   ```sh
   # Prints the clicked key (exit 0), or "dismissed"/"expired" (exit 1).
   dmsh activity push --id perm --title "Claude needs permission" --placement center \
       --priority critical --icon alert --action allow:Allow --action deny:Deny --wait
   dmsh activity invoke perm allow     # same as clicking the button
   dmsh events                         # stream action_invoked / removed events as JSON
   ```
   The `default` action is invoked by clicking the activity itself (not drawn as a
   button). Invoking dismisses the activity unless it has `keep_on_action` (media).

   Lifecycle rules: `ttl` counts from the last push of an id (a repeatedly pushed OSD stays
   up); a `flow` activity's center phase counts from its first push (updating a
   notification doesn't pop it back). Center winner: priority, then transient over
   persistent, then most recently updated. Default placement is `flow`.
2. **Agents work on the code.** `dmsh render` simulates activities and animations on a
   fake clock and draws the islands to PNG, so a change can be checked without a
   display:
   ```sh
   dmsh render --activities fixture.json --at 2500ms            # one frame
   dmsh render --activities fixture.json --at 3s --filmstrip 200ms --crop 560,1085 \
               --background '#3c3c40'                              # a frame every 200 ms
   dmsh render --activities fixture.json --hover center             # expanded state
   ```
   Fixture entries are activity specs; an `at_ms` field delays one. The layout and
   animation code in `dmsh-core` is unit-tested with a fake text measurer.

## Config

`~/.config/dmsh/config.kdl` (or `--config <path>`). `dmsh config default` prints the
documented defaults (`crates/dmsh-core/src/default-config.kdl`, kept in sync with the code
by a test); `dmsh config check` validates; `dmsh config path` locates it.

- Sections: `theme` (colors, font, sizes), `clock` (strftime formats), `animation`
  (spring response/damping, `off`), `outputs` (`only` / `except`), `sources` (on/off),
  `timing` (OSD, status, notification-in-center durations).
- Parsing is strict: unknown sections/options and bad values are errors with line
  numbers. KDL v2 and v1 (niri's syntax) both parse.
- Saving the file reloads it (inotify on the directory, debounced 150 ms; also
  `dmsh reload`). An invalid file keeps the previous config and shows a "Config error"
  activity in the center with the message.
- Applied live: theme, font, animation, outputs, timings, clock formats. Turning sources
  on or off takes effect on restart.
- `dmsh render --config <path>` renders with a config's theme, for checking it headless.

## Keybinds (niri)

`dmsh niri` prints a snippet for niri's config (autostart plus binds). The binds use:

- `dmsh toggle right` / `dmsh toggle center`: opens the island on the focused output,
  pinned (stays expanded) with an exclusive keyboard grab, or closes it. Keys: arrows /
  `j` `k` / Tab move the selection, Enter or Space activates, Delete dismisses a
  notification row, `m` opens a tray icon's menu, Esc closes the menu, then the island.
  Activating something that hands focus to an app (a notification, tray app, menu entry,
  a center button) closes the island so the app gets the keyboard.
- `dmsh activity clear --right`: clears the notification list.

## Running it

`dist/dmsh.service` is a systemd user unit bound to `graphical-session.target` (which
`niri-session` starts); alternatively niri's `spawn-at-startup "dmsh"` (see `dmsh niri`).

Quick controls are also available to scripts and agents:
`dmsh control wifi on|off|toggle`, `dmsh control keep-awake on|off|toggle`.

## Install

`install.sh` installs dependencies, the binary (`~/.local/bin/dmsh`) and the niri
snippet (`~/.config/niri/dmsh.kdl` + an `include` line), and nothing else; the rest of a
desktop belongs to a separate setup repo. No workspace indicator: niri's overview covers
it.

## Phases

0. **Skeleton** — layer surface per output, two static pills, input region, fractional
   scale, headless `dmsh render`. ✓
1. **Activities + arbiter + IPC socket + CLI.** Agents can push activities from here on.
   Until text lands, islands show placeholders: a priority-colored dot, a progress bar,
   and one dim dot per extra item on the right. ✓
2. **Sources**: niri, clock, volume/brightness OSD, MPRIS, notifications, battery, Wi-Fi.
   Transient center activities show only on the focused output. ✓
3. **Text and icons, spring-based morphing, center→right slide, hover expand.** ✓
4. **Expanded views**: clickable buttons with hover highlights, notification actions
   (`ActionInvoked`, `resident`), right-island list with per-row close and "Clear all",
   media controls (previous / play-pause / next over MPRIS), system tray with menus. ✓
5. **Config (KDL)** with live reload; **niri keybind integration** (toggle with keyboard
   navigation, `dmsh niri` snippet). ✓
6. **Daily-driver gaps**: always-visible battery, quick controls (Wi-Fi, keep awake),
   clock refresh on resume (logind `PrepareForSleep`), takeover from a previous shell,
   systemd unit. ✓
7. **Install script and agent integration** (Claude Code hooks with island permission
   prompts, MCP server). ✓

## Open questions

- Startup: fontconfig lookups (10–25 ms per `fc-match`) dominated; the font choice is
  now cached in `~/.cache/dmsh/fonts` (a week, while its files exist). In the VM: first
  frame 78 ms cold, 13–28 ms after.
- Verified in a clean niri VM: first frame in 34–38 ms, ~15 MB, 2 threads at rest; keep
  awake holds off idle (swayidle) and releases it; the clock is right one second after
  resuming from a 3-minute suspend.
- Wi-Fi toggle: only "on while on" was exercised live; turning the radio off is untested
  (the VM has no Wi-Fi device).
- Multi-monitor is unit-tested only; the VM has one display head.
- Multi-monitor focus handling is unit-tested but not yet tried on a second monitor.

