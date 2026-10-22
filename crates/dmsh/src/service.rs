//! `dmsh service`: a systemd user unit that starts dmsh with the graphical session (niri
//! started as `niri-session` / `niri.service` brings up graphical-session.target) and
//! restarts it if it crashes.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

pub fn unit(exe: &Path) -> String {
    let exe = exe.display().to_string();
    // systemd splits ExecStart on spaces unless quoted.
    let exe = if exe.contains(char::is_whitespace) {
        format!("\"{exe}\"")
    } else {
        exe
    };
    format!(
        r#"# dmsh as a systemd user service: starts with the graphical session, restarts on a
# crash. Written by `dmsh service --apply`; `dmsh service --remove` takes it out.

[Unit]
Description=dmsh, a dynamic-island shell for niri
Documentation=https://github.com/Kexfff/DM-Shell
PartOf=graphical-session.target
After=graphical-session.target
Requisite=graphical-session.target
# A crash loop (say, a broken build) gives up after 5 starts in a minute.
StartLimitIntervalSec=60
StartLimitBurst=5

[Service]
ExecStart={exe}
Restart=on-failure
RestartSec=1

[Install]
WantedBy=graphical-session.target
"#
    )
}

pub fn unit_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("systemd/user/dmsh.service"))
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("running systemctl")?;
    anyhow::ensure!(
        status.success(),
        "systemctl --user {} failed",
        args.join(" ")
    );
    Ok(())
}

/// Writes and enables the unit (it starts with the next session; `systemctl --user start
/// dmsh` starts it now), or disables and deletes it.
pub fn apply(exe: &Path, remove: bool) -> Result<PathBuf> {
    let path = unit_path().context("$HOME is not set")?;
    if remove {
        if path.exists() {
            systemctl(&["disable", "dmsh.service"]).ok();
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        systemctl(&["daemon-reload"])?;
        return Ok(path);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, unit(exe)).with_context(|| format!("writing {}", path.display()))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "dmsh.service"])?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_runs_this_binary_and_restarts_it() {
        let unit = unit(Path::new("/home/me/.local/bin/dmsh"));
        assert!(unit.contains("\nExecStart=/home/me/.local/bin/dmsh\n"));
        assert!(unit.contains("\nRestart=on-failure\n"));
        assert!(unit.contains("\nWantedBy=graphical-session.target\n"));
        let spaced = super::unit(Path::new("/opt/my apps/dmsh"));
        assert!(spaced.contains("\nExecStart=\"/opt/my apps/dmsh\"\n"));
    }
}
