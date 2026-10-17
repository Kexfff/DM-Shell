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

use crate::{Emitter, Event, QUIET_AFTER_SET, SETTLE};

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
}
