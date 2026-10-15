//! Finding, loading and watching the config file.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dmsh_core::config::{Config, ConfigError};
use inotify::{Inotify, WatchMask};
use smithay_client_toolkit::reexports::calloop::{
    Interest, LoopHandle, Mode, PostAction, generic::Generic,
};

use crate::shell::Shell;

/// Editors save in bursts (write, rename, chmod); reload once things settle.
pub const RELOAD_DELAY: Duration = Duration::from_millis(150);

/// `$XDG_CONFIG_HOME/dmsh/config.kdl` (or `~/.config/dmsh/config.kdl`).
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("dmsh").join("config.kdl"))
}

/// Reads and parses the config; a missing file is the default config.
pub fn load(path: Option<&Path>) -> Result<Config, ConfigError> {
    let Some(path) = path else {
        return Ok(Config::default());
    };
    match std::fs::read_to_string(path) {
        Ok(text) => Config::parse(&text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(ConfigError {
            line: 0,
            message: format!("reading {}: {e}", path.display()),
        }),
    }
}

/// Calls [`Shell::schedule_reload`] whenever the config file is written, created,
/// replaced or removed. Watches the directory, since editors often replace the file.
pub fn watch(handle: &LoopHandle<'static, Shell>, path: &Path) -> io::Result<()> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let inotify = Inotify::init()?;
    inotify.watches().add(
        dir,
        WatchMask::CLOSE_WRITE
            | WatchMask::MOVED_TO
            | WatchMask::CREATE
            | WatchMask::DELETE
            | WatchMask::MOVED_FROM,
    )?;
    let name = name.to_owned();
    handle
        .insert_source(
            Generic::new(inotify, Interest::READ, Mode::Level),
            move |_, inotify, shell| {
                let mut buf = [0u8; 4096];
                let mut ours = false;
                // SAFETY: we only read events; the fd is neither replaced nor closed.
                let inotify = unsafe { inotify.get_mut() };
                loop {
                    match inotify.read_events(&mut buf) {
                        Ok(events) => {
                            let mut any = false;
                            for event in events {
                                any = true;
                                ours |= event.name == Some(name.as_os_str());
                            }
                            if !any {
                                break;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            log::warn!("watching config: {e}");
                            break;
                        }
                    }
                }
                if ours {
                    shell.schedule_reload();
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(())
}
