//! Volume OSD. Listens to `pactl subscribe` (works on PipeWire via pipewire-pulse) and reads
//! the default sink with `wpctl` only when a sink or the server changed, so nothing runs
//! while the volume is untouched.

use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::{Emitter, SETTLE, osd};

const ID: &str = "volume";

#[derive(Clone, Copy, Debug, PartialEq)]
struct Volume {
    /// Percent, so tiny float differences don't trigger an OSD.
    percent: u32,
    muted: bool,
}

pub async fn run(emitter: Emitter) -> Result<()> {
    let mut last = read().await?;
    let mut child = Command::new("pactl")
        .arg("subscribe")
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting `pactl subscribe`")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let mut lines = BufReader::new(stdout).lines();

    while let Some(line) = lines.next_line().await? {
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
            let title = if now.muted { "Muted" } else { "Volume" };
            emitter.push(osd(ID, title, Some(now.percent as f32 / 100.0)));
        }
    }
    anyhow::bail!("`pactl subscribe` exited")
}

async fn read() -> Result<Volume> {
    let out = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
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
