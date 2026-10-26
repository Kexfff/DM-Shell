# NixOS

The repository is a flake. It provides the package, a NixOS module and a Home Manager
module; both modules start dmsh with your graphical session and restart it if it
crashes.

| Output | What it is |
|---|---|
| `packages.<system>.dmsh` (and `default`) | The `dmsh` binary. Its tests run as part of the build. |
| `nixosModules.default` | `programs.dmsh`: the package, a systemd user service for every user, UPower for the battery. |
| `homeManagerModules.default` | `programs.dmsh`: the package, a user service, and optionally `config.kdl`. |
| `overlays.default` | Adds `pkgs.dmsh`. |
| `devShells.<system>.default` | Rust, clippy, rustfmt, rust-analyzer and the native libraries, for hacking on dmsh. |

## NixOS module

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    dmsh = {
      url = "github:Kexfff/DM-Shell";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, dmsh, ... }: {
    nixosConfigurations.my-laptop = nixpkgs.lib.nixosSystem {
      modules = [
        ./configuration.nix
        dmsh.nixosModules.default
        { programs.dmsh.enable = true; }
      ];
    };
  };
}
```

| Option | Default | Meaning |
|---|---|---|
| `programs.dmsh.enable` | `false` | Install dmsh and enable its user service. |
| `programs.dmsh.package` | this flake's | The package to use. |
| `programs.dmsh.systemd.enable` | `true` | Start dmsh with `graphical-session.target` and restart it after a crash. Turn off to use niri's `spawn-at-startup` instead. |

The service starts with `graphical-session.target`, which niri brings up when it runs as
a session (`programs.niri.enable` and picking niri in your display manager, or
`niri-session`). It applies to every user that logs into a graphical session.

## Home Manager module

```nix
{
  imports = [ dmsh.homeManagerModules.default ];

  programs.dmsh = {
    enable = true;
    # Optional: manage ~/.config/dmsh/config.kdl (see `dmsh config default`).
    settings = ''
      weather {
          location "Berlin"
      }
    '';
  };
}
```

`programs.dmsh.package` and `programs.dmsh.systemd.enable` work as in the NixOS module.
With `settings` left empty, `config.kdl` isn't managed and you can edit it (and have it
reload live) as usual.

## niri keybinds

`dmsh niri --no-autostart` prints the keybinds for your niri config (the service starts
dmsh, so leave out `spawn-at-startup`). On NixOS the binds can call plain `dmsh`, since
niri's `PATH` includes the system and user profiles:

```kdl
binds {
    Mod+N { spawn "dmsh" "toggle" "right"; }
    Mod+Ctrl+N { spawn "dmsh" "activity" "clear" "--right"; }
    Mod+I { spawn "dmsh" "toggle" "center"; }
}
```

## Without flakes, or just trying it

```bash
nix run github:Kexfff/DM-Shell -- render --hover right --battery 76 -o islands.png
nix profile install github:Kexfff/DM-Shell
```

## Runtime tools

dmsh calls a few programs: `fc-match`/`fc-list` (fonts), `pactl` and `wpctl` (volume and
microphone), `curl` (weather), `systemctl`, `sh` and `niri`. The package appends
fontconfig, pulseaudio's `pactl` and curl to dmsh's `PATH`, after your own, so it works
on a system that doesn't install them while preferring yours. `wpctl` comes with
PipeWire (`services.pipewire.enable`), which NixOS desktops have; volume and microphone
also need its PulseAudio compatibility (`services.pipewire.pulse.enable`), the default
alongside it. `install.sh` isn't needed (or used) on NixOS.
