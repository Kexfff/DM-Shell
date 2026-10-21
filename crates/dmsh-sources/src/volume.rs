//! Volume OSD and the volume slider. Listens to `pactl subscribe` (works on PipeWire via
//! pipewire-pulse) and reads the default sink with `wpctl` only when a sink or the server
//! changed, so nothing runs while the volume is untouched. Changes from the slider are
//! set with `wpctl` too.

use std::process::Stdio;
use std::time::Instant;

use anyhow::{Context, Result};
use dmsh_core::Level;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::{Emitter, Event, QUIET_AFTER_SET, Retry, SETTLE};

/// Changes asked for by the islands.
#[derive(Debug)]
pub enum SetVolume {
    /// 0.0 to 1.0; also unmutes.
    Level(f32),
    ToggleMute,
}

const SINK: &str = "@DEFAULT_AUDIO_SINK@";

const ID: &str = "volume";

#[derive(Clone, Copy, Debug, PartialEq)]
struct Volume {
    /// Percent, so tiny float differences don't trigger an OSD.
    percent: u32,
    muted: bool,
}

impl Volume {
    fn level(self) -> Level {
        Level {
            value: self.percent as f32 / 100.0,
            muted: self.muted,
        }
    }
}

pub(crate) async fn run(
    emitter: Emitter,
    mut commands: mpsc::UnboundedReceiver<SetVolume>,
) -> Result<()> {
    let mut retry = Retry::new("volume");
    loop {
        retry.start();
        if let Err(e) = watch(&emitter, &mut commands).await {
            retry.after(e).await?;
        }
    }
}

/// Follows the volume until the audio server goes away.
async fn watch(emitter: &Emitter, commands: &mut mpsc::UnboundedReceiver<SetVolume>) -> Result<()> {
    // Requests made while the server was away are stale now.
    while commands.try_recv().is_ok() {}
    let mut last = read().await?;
    emitter.emit(Event::Volume(last.level()));
    // Changes made from the slider don't need an OSD on top of it.
    let mut quiet_until = Instant::now();
    let mut child = Command::new("pactl")
        .arg("subscribe")
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting `pactl subscribe`")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let mut lines = BufReader::new(stdout).lines();

    loop {
        let line = tokio::select! {
            line = lines.next_line() => match line? {
                Some(line) => line,
                None => break,
            },
            Some(command) = commands.recv() => {
                // A slider drag sends many; only the newest level matters.
                let (mut level, mut toggle) = (None, false);
                let mut next = Some(command);
                while let Some(command) = next {
                    match command {
                        SetVolume::Level(v) => level = Some(v),
                        SetVolume::ToggleMute => toggle = !toggle,
                    }
                    next = commands.try_recv().ok();
                }
                if let Err(e) = set(level, toggle, last.muted).await {
                    log::warn!("setting volume: {e:#}");
                }
                quiet_until = Instant::now() + QUIET_AFTER_SET;
                continue;
            }
        };
        // "Event 'change' on sink #56" or "... on server" (default sink changed).
        if !(line.contains(" on sink #") || line.contains(" on server")) {
            continue;
        }
        // Coalesce a burst (a held volume key) into one read.
        while let Ok(Ok(Some(_))) = tokio::time::timeout(SETTLE, lines.next_line()).await {}

        let now = match read().await {
            Ok(v) => v,
            Err(e) => {
                log::debug!("reading volume: {e:#}");
                continue;
            }
        };
        if now != last {
            last = now;
            emitter.emit(Event::Volume(now.level()));
            if Instant::now() < quiet_until {
                continue;
            }
            let (icon, title) = if now.muted {
                ("volume-mute", "Muted")
            } else {
                ("volume", "Volume")
            };
            emitter.push(emitter.osd(ID, icon, title, Some(now.percent as f32 / 100.0)));
        }
    }
    anyhow::bail!("`pactl subscribe` exited")
}

async fn wpctl(args: &[&str]) -> Result<()> {
    let status = Command::new("wpctl")
        .args(args)
        .status()
        .await
        .context("running wpctl")?;
    anyhow::ensure!(
        status.success(),
        "wpctl {} failed: {status}",
        args.join(" ")
    );
    Ok(())
}

async fn set(level: Option<f32>, toggle_mute: bool, muted: bool) -> Result<()> {
    if let Some(v) = level {
        let v = format!("{:.3}", v.clamp(0.0, 1.0));
        wpctl(&["set-volume", SINK, &v]).await?;
        // Turning the volume up while muted means "I want to hear it".
        if muted && !toggle_mute {
            wpctl(&["set-mute", SINK, "0"]).await?;
        }
    }
    if toggle_mute {
        wpctl(&["set-mute", SINK, "toggle"]).await?;
    }
    Ok(())
}

async fn read() -> Result<Volume> {
    let out = Command::new("wpctl")
        .args(["get-volume", SINK])
        .output()
        .await
        .context("running wpctl")?;
    let text = String::from_utf8_lossy(&out.stdout);
    parse(&text).with_context(|| format!("unexpected wpctl output: {text:?}"))
}

/// Parses `Volume: 0.40` or `Volume: 0.40 [MUTED]`.
fn parse(text: &str) -> Option<Volume> {
    let rest = text.trim().strip_prefix("Volume:")?.trim();
    let number = rest.split_whitespace().next()?;
    let value: f32 = number.parse().ok()?;
    Some(Volume {
        percent: (value * 100.0).round() as u32,
        muted: rest.contains("[MUTED]"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_wpctl_output() {
        assert_eq!(
            parse("Volume: 0.40\n"),
            Some(Volume {
                percent: 40,
                muted: false
            })
        );
        assert_eq!(
            parse("Volume: 1.25 [MUTED]"),
            Some(Volume {
                percent: 125,
                muted: true
            })
        );
        assert_eq!(parse("garbage"), None);
    }

    /// At login dmsh can start before the audio server is ready; the volume must show up
    /// once it is, not be given up on. Fake `wpctl`/`pactl` stand in for PipeWire.
    #[tokio::test(flavor = "current_thread")]
    async fn waits_for_the_audio_server() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, Mutex, RwLock};

        let dir = std::env::temp_dir().join(format!("dmsh-volume-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ready = dir.join("ready");
        let scripts = [
            (
                "wpctl",
                format!(
                    "#!/bin/sh\n[ -e {} ] || {{ echo 'Could not connect to PipeWire' >&2; exit 1; }}\necho 'Volume: 0.40'\n",
                    ready.display()
                ),
            ),
            ("pactl", "#!/bin/sh\nexec sleep 30\n".to_owned()),
        ];
        for (name, body) in scripts {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        // SAFETY: only this test runs `wpctl` or `pactl`; the fake directory holds nothing else.
        unsafe { std::env::set_var("PATH", path) };

        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let emitter = Emitter {
            emit: Arc::new(move |e| {
                if let Event::Volume(level) = e {
                    sink.lock().unwrap().push(level);
                }
            }),
            config: Arc::new(RwLock::new(Default::default())),
            config_changed: Default::default(),
        };
        let (_tx, rx) = mpsc::unbounded_channel();
        let source = tokio::spawn(run(emitter, rx));

        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(events.lock().unwrap().is_empty(), "no server yet");
        std::fs::write(&ready, "").unwrap();
        // The first retry comes a second after the first failure.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(
            events.lock().unwrap().first(),
            Some(&Level {
                value: 0.4,
                muted: false
            })
        );
        assert!(!source.is_finished(), "still following the volume");
        source.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
