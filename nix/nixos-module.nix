self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.dmsh;
in
{
  options.programs.dmsh = {
    enable = lib.mkEnableOption "dmsh, a dynamic-island shell for niri";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.dmsh;
      defaultText = lib.literalExpression "dmsh.packages.\${system}.dmsh";
      description = "The dmsh package to use.";
    };

    systemd.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Start dmsh with every user's graphical session (graphical-session.target, which
        niri brings up when started as a session) and restart it if it crashes. Turn off
        to start it another way, such as niri's `spawn-at-startup`.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    systemd.user.services.dmsh = lib.mkIf cfg.systemd.enable (
      import ./service.nix cfg.package
      // {
        # NixOS gives services a bare PATH (coreutils, grep, sed, systemd); dmsh runs
        # wpctl, niri, systemctl and your session-menu commands, so it gets a login
        # session's instead. (%h and %u are the user's home and name.)
        environment.PATH = lib.mkForce (
          lib.concatStringsSep ":" [
            "/run/wrappers/bin"
            "%h/.nix-profile/bin"
            "/etc/profiles/per-user/%u/bin"
            "/nix/var/nix/profiles/default/bin"
            "/run/current-system/sw/bin"
          ]
        );
      }
    );
    # The battery on the right island.
    services.upower.enable = lib.mkDefault true;
  };
}
