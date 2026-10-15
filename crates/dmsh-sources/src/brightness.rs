//! Brightness OSD: inotify on the backlight's `brightness` file, which changes whenever
//! brightnessctl, logind or a desktop tool writes it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use inotify::{Inotify, WatchMask};

use crate::{Emitter, osd};

const ID: &str = "brightness";
const BACKLIGHT_DIR: &str = "/sys/class/backlight";

pub async fn run(emitter: Emitter) -> Result<()> {
    let device = first_backlight().context("no backlight device")?;
    let max = read_number(&device.join("max_brightness"))?.max(1);
    let path = device.join("brightness");
    let mut last = read_number(&path)?;

    let inotify = Inotify::init()?;
    inotify.watches().add(&path, WatchMask::MODIFY)?;
    let mut events = inotify.into_event_stream([0u8; 256])?;

    while let Some(event) = events.next().await {
        event?;
        let now = read_number(&path)?;
        if now != last {
            last = now;
            emitter.push(osd(ID, "Brightness", Some(now as f32 / max as f32)));
        }
    }
    anyhow::bail!("inotify stream ended")
}

fn first_backlight() -> Option<PathBuf> {
    let mut devices: Vec<PathBuf> = std::fs::read_dir(BACKLIGHT_DIR)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    devices.sort();
    devices.into_iter().next()
}

fn read_number(path: &Path) -> Result<u32> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    text.trim()
        .parse()
        .with_context(|| format!("parsing {}", path.display()))
}
