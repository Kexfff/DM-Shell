# dmsh — design

A minimal shell for [niri](https://github.com/YaLTeR/niri): two "dynamic islands" at the
top of every monitor that show only what is relevant right now. Not a toolkit — one
purpose-built program. Light, fast, and controllable by AI agents.

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
  system tray. Shows the newest item plus a `+N` counter; expands into a list.

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
| notifications | dmsh owns `org.freedesktop.Notifications` | flow activity per notification; urgency → priority; emits `NotificationClosed` with the right reason |

- **Notifications**: only one daemon can own the name. Stop mako/swaync/a Quickshell
  notification server to use dmsh's; otherwise dmsh warns and runs without it.
- **System tray** (planned): StatusNotifierItem host, shown in the right island.

## AI-agent friendliness

1. **Agents drive the shell** (primary). A Unix socket with newline-delimited JSON (same
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

## Phases

0. **Skeleton** — layer surface per output, two static pills, input region, fractional
   scale, headless `dmsh render`. ✓
1. **Activities + arbiter + IPC socket + CLI.** Agents can push activities from here on.
   Until text lands, islands show placeholders: a priority-colored dot, a progress bar,
   and one dim dot per extra item on the right. ✓
2. **Sources**: niri, clock, volume/brightness OSD, MPRIS, notifications, battery, Wi-Fi.
   Transient center activities show only on the focused output. ✓
3. **Text and icons, spring-based morphing, center→right slide, hover expand.** ✓
4. Expanded views: notification list and actions, media controls, tray.
5. Config (KDL), niri keybind integration (e.g. `dmsh focus`).

## Open questions

- Clock after suspend: the minute timer uses a monotonic clock, which stops during
  suspend, so the clock can lag up to a minute after resume. Fix by also refreshing on
  logind's `PrepareForSleep(false)` once the clock is visible (phase 3).
- Multi-monitor focus handling is unit-tested but not yet tried on a second monitor.

- Workspaces indicator: not planned for now (niri's overview covers it). Revisit if missed.
