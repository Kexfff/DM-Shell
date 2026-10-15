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
- **Exclusive zone = thin strip** (windows tile below it). The surface itself is taller
  than the strip so expanded islands float over windows.
- **Input region = the pill shapes only**; clicks elsewhere fall through to windows.
- The surface is never resized during animations (resizes need a compositor round trip
  and stutter). The pill shapes change inside a fixed buffer.
- CPU rasterization with `tiny-skia` into `wl_shm` buffers. Redraw only on frame
  callbacks while something is animating, and only the damaged region.
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

### System integrations (planned)

- **Notifications**: dmsh owns `org.freedesktop.Notifications` (disable mako/swaync). If
  the name is taken it warns and runs without notifications.
- **System tray**: StatusNotifierItem host, shown in the right island.
- **Media**: MPRIS.

## AI-agent friendliness

1. **Agents drive the shell** (primary). A Unix socket with newline-delimited JSON (same
   style as niri's IPC) and a CLI over it, e.g.
   `dmsh activity push --id build --title "cargo build" --progress 0.4 --placement center --ttl 30s`.
   JSON Schema generated from the Rust types; an MCP wrapper can come later. Example use:
   Claude Code hooks push "needs permission" / "task done" to the island.
2. **Agents work on the code.** `dmsh render` draws the islands headlessly to PNG so a
   change can be checked without a display; snapshot tests; `dmsh state --json` dump.

## Phases

0. **Skeleton** — layer surface per output, two static pills, input region, fractional
   scale, headless `dmsh render`. ← current
1. Activities + arbiter + IPC socket + CLI. Agents can push activities from here on.
2. Sources: niri, clock, volume/brightness OSD, MPRIS, notifications, battery, Wi-Fi.
3. Text and icons, spring-based morphing, center→right slide, hover expand.
4. Expanded views: notification list and actions, media controls, tray.
5. Config (KDL), niri keybind integration (e.g. `dmsh focus`).

## Open questions

- Workspaces indicator: not planned for now (niri's overview covers it). Revisit if missed.
