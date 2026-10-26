# The systemd user unit both modules install, in NixOS's `systemd.user.services` shape
# (Home Manager's is converted from it): start with the graphical session (niri started
# as a session brings up graphical-session.target), restart a second after a crash, and
# give up after 5 crashes in a minute.
package: {
  description = "dmsh, a dynamic-island shell for niri";
  documentation = [ "https://github.com/Kexfff/DM-Shell" ];
  partOf = [ "graphical-session.target" ];
  after = [ "graphical-session.target" ];
  requisite = [ "graphical-session.target" ];
  wantedBy = [ "graphical-session.target" ];
  startLimitIntervalSec = 60;
  startLimitBurst = 5;
  serviceConfig = {
    ExecStart = "${package}/bin/dmsh";
    Restart = "on-failure";
    RestartSec = 1;
  };
}
