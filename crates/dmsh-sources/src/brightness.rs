//! Brightness OSD and the brightness slider: inotify on the backlight's `brightness` file,
//! which changes whenever brightnessctl, logind or a desktop tool writes it. The slider
//! sets it through logind (`SetBrightness`), which needs no extra permissions.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use dmsh_core::Level;
use futures_util::StreamExt;
use inotify::{Inotify, WatchMask};
use tokio::sync::mpsc;

use crate::{Emitter, Event, QUIET_AFTER_SET};

const ID: &str = "brightness";
const BACKLIGHT_DIR: &str = "/sys/class/backlight";

pub async fn run(emitter: Emitter, mut commands: mpsc::UnboundedReceiver<f32>) -> Result<()> {
    let device = first_backlight().context("no backlight device")?;
    let name = device
        .file_name()
        .and_then(|n| n.to_str())
        .context("backlight name")?
        .to_owned();
    let max = read_number(&device.join("max_brightness"))?.max(1);
    let path = device.join("brightness");
    let mut last = read_number(&path)?;
    let level = |raw: u32| Level {
        value: raw as f32 / max as f32,
        muted: false,
    };
    emitter.emit(Event::Brightness(level(last)));

    let inotify = Inotify::init()?;
    inotify.watches().add(&path, WatchMask::MODIFY)?;
    let mut events = inotify.into_event_stream([0u8; 256])?;
    let mut system = None;
    // Changes made from the slider don't need an OSD on top of it.
    let mut quiet_until = Instant::now();

    loop {
        tokio::select! {
            event = events.next() => {
                let Some(event) = event else { break };
                event?;
                let now = read_number(&path)?;
                if now != last {
                    last = now;
                    emitter.emit(Event::Brightness(level(now)));
                    if Instant::now() >= quiet_until {
                        emitter.push(emitter.osd(
                            ID,
                            "brightness",
                            "Brightness",
                            Some(now as f32 / max as f32),
                        ));
                    }
                }
            }
            Some(mut value) = commands.recv() => {
                // A slider drag sends many; only the newest matters.
                while let Ok(newer) = commands.try_recv() {
                    value = newer;
                }
                // Never all the way to 0: some panels turn off completely.
                let raw = ((value.clamp(0.0, 1.0) * max as f32).round() as u32).max(1);
                if let Err(e) = set(&mut system, &name, raw).await {
                    log::warn!("setting brightness: {e:#}");
                }
                quiet_until = Instant::now() + QUIET_AFTER_SET;
            }
        }
    }
    anyhow::bail!("inotify stream ended")
}

/// Sets the backlight through logind, connecting to the system bus on first use.
async fn set(system: &mut Option<zbus::Connection>, name: &str, raw: u32) -> Result<()> {
    if system.is_none() {
        *system = Some(zbus::Connection::system().await?);
    }
    let Some(conn) = system.as_ref() else {
        unreachable!()
    };
    conn.call_method(
        Some("org.freedesktop.login1"),
        "/org/freedesktop/login1/session/auto",
        Some("org.freedesktop.login1.Session"),
        "SetBrightness",
        &("backlight", name, raw),
    )
    .await
    .context("logind SetBrightness")?;
    Ok(())
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
