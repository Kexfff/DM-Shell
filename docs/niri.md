# niri integration

## Autostart and keybinds

`install.sh` sets this up (see the README). By hand: `dmsh niri` prints a snippet; save
it as `~/.config/niri/dmsh.kdl` and add `include "dmsh.kdl"` to `config.kdl`. It refers to dmsh by the
full path of the binary you ran it with, because niri starts programs with its own
`PATH`, which usually doesn't include `~/.cargo/bin`. With `cargo install` it looks like
this:

```kdl
spawn-at-startup "/home/you/.cargo/bin/dmsh"

binds {
    Mod+N hotkey-overlay-title="dmsh: notifications" { spawn "/home/you/.cargo/bin/dmsh" "toggle" "right"; }
    Mod+Ctrl+N hotkey-overlay-title="dmsh: clear notifications" { spawn "/home/you/.cargo/bin/dmsh" "activity" "clear" "--right"; }
    Mod+I hotkey-overlay-title="dmsh: center island" { spawn "/home/you/.cargo/bin/dmsh" "toggle" "center"; }
}
```

Merge the binds into your existing `binds {}` block and pick keys that are free in
your config.

**Restart on crash.** Instead of `spawn-at-startup`, `dmsh service --apply` writes a
systemd user unit (`~/.config/systemd/user/dmsh.service`) and enables it: dmsh starts
with `graphical-session.target` and is restarted a second after a crash (giving up after
5 crashes in a minute). `dmsh niri --no-autostart` prints the snippet without the
`spawn-at-startup` line; use one or the other, not both. The target is only there when
niri runs as a session (`niri-session`, or your display manager's niri entry). Logs go
to `journalctl --user -u dmsh`; `systemctl --user restart dmsh` restarts it, and
stopping it (`systemctl --user stop dmsh`, or `pkill dmsh`) keeps it stopped.

## What dmsh takes from niri

dmsh reads niri's event stream (`$NIRI_SOCKET`):

- **Focused monitor** — transient center events (notifications, OSDs) appear only on
  the focused monitor; the right island is the same on every monitor.
- **Keyboard layout** — an overlay when the layout switches.
- **Screencasts** — a red screen icon on the right island while a niri screencast is
  active, with "Screen is being shared" for a few seconds when it starts. (Microphone
  and camera get the same treatment, from PipeWire and `/dev/video*`.)

## Keyboard navigation

`dmsh toggle right` and `dmsh toggle center` open that island on the focused monitor,
keep it expanded, and take keyboard focus. Running the same toggle again closes it.

| Key | Action |
|---|---|
| ↑ ↓ ← →, `j` `k` `h` `l`, Tab, Shift+Tab | Move the selection |
| Enter, Space | Activate (button, notification, toggle, tray icon, menu entry) |
| Delete, Backspace, `d` | Dismiss the selected notification |
| `m`, Menu | Open the selected tray icon's menu |
| Esc | Close the menu (tray or session), then the island |

Activating something that hands focus to an app — opening a notification, a tray app,
a menu entry, a center button — closes the island, so the app gets the keyboard.
Toggles, dismissals and "Clear all" keep it open.

With a volume, microphone or brightness slider selected, ← → (`h` `l`) move it by 5%
instead of the selection; ↑ ↓ and Tab still move the selection. Enter on the volume or
microphone button opens its slider when released quickly and mutes when held.

Session menu options (behind the power button) run only while Enter is **held** until
the option fills up; a quick press does nothing.

## Other useful binds

Any dmsh command works in a bind (use the full path, as above; the examples shorten it
to `dmsh` only if it's somewhere on niri's `PATH`, like `/usr/bin`):

```kdl
binds {
    Mod+Shift+K { spawn "dmsh" "control" "keep-awake"; }
    Mod+Shift+F { spawn "dmsh" "control" "wifi"; }
}
```

Volume, brightness and media keys don't need dmsh at all: bind them to `wpctl`,
`brightnessctl` and `playerctl`, and dmsh shows the overlays when the values change.

```kdl
binds {
    XF86AudioRaiseVolume allow-when-locked=true { spawn "wpctl" "set-volume" "-l" "1.0" "@DEFAULT_AUDIO_SINK@" "5%+"; }
    XF86AudioLowerVolume allow-when-locked=true { spawn "wpctl" "set-volume" "@DEFAULT_AUDIO_SINK@" "5%-"; }
    XF86AudioMute        allow-when-locked=true { spawn "wpctl" "set-mute" "@DEFAULT_AUDIO_SINK@" "toggle"; }
    XF86MonBrightnessUp   allow-when-locked=true { spawn "brightnessctl" "set" "5%+"; }
    XF86MonBrightnessDown allow-when-locked=true { spawn "brightnessctl" "set" "5%-"; }
    XF86AudioPlay { spawn "playerctl" "play-pause"; }
    XF86AudioNext { spawn "playerctl" "next"; }
    XF86AudioPrev { spawn "playerctl" "previous"; }
}
```
