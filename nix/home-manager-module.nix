self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.dmsh;
  unit = import ./service.nix cfg.package;
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

    settings = lib.mkOption {
      type = lib.types.lines;
      default = "";
      example = ''
        theme {
            right-expanded-width 380
        }
        weather {
            location "Berlin"
        }
      '';
      description = ''
        Contents of `~/.config/dmsh/config.kdl` (see `dmsh config default` for every
        option). Left empty, the file isn't managed, so you can edit it yourself.
      '';
    };

    systemd.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Start dmsh with the graphical session (graphical-session.target, which niri
        brings up when started as a session) and restart it if it crashes. Turn off to
        start it another way, such as niri's `spawn-at-startup`.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."dmsh/config.kdl" = lib.mkIf (cfg.settings != "") {
      text = cfg.settings;
    };

    systemd.user.services.dmsh = lib.mkIf cfg.systemd.enable {
      Unit = {
        Description = unit.description;
        Documentation = unit.documentation;
        PartOf = unit.partOf;
        After = unit.after;
        Requisite = unit.requisite;
        StartLimitIntervalSec = unit.startLimitIntervalSec;
        StartLimitBurst = unit.startLimitBurst;
      };
      Service = unit.serviceConfig;
      Install.WantedBy = unit.wantedBy;
    };
  };
}
