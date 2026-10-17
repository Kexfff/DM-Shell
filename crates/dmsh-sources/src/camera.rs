//! Camera indicator: a camera is in use while some process has a `/dev/video*` open.
//! inotify reports opens and closes of the device nodes (and new ones appearing in /dev);
//! once things settle, /proc says who still has one open. Nothing runs in between.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use dmsh_core::Sensor;
use futures_util::StreamExt;
use inotify::{Inotify, WatchMask};

use crate::Emitter;

/// Apps and PipeWire open cameras briefly to probe them; only a camera still open after
/// this long counts.
const SETTLE: Duration = Duration::from_secs(1);

/// They hold the camera for whichever app asked; not worth naming.
const MIDDLEMEN: [&str; 2] = ["pipewire", "wireplumber"];

pub async fn run(emitter: Emitter) -> Result<()> {
    let inotify = Inotify::init()?;
    let watches = inotify.watches();
    let mut watches_for_new = watches.clone();
    let dev = watches.clone().add("/dev", WatchMask::CREATE)?;
    for device in video_devices() {
        watch(&mut watches_for_new, &device);
    }
    let mut events = inotify.into_event_stream([0u8; 1024])?;

    let mut apps = users();
    if let Some(apps) = &apps {
        emitter.capture(Sensor::Camera, true, apps);
    }
    loop {
        let mut next = events.next().await;
        loop {
            let Some(event) = next else {
                anyhow::bail!("inotify stream ended");
            };
            let event = event?;
            // A camera plugged in: watch it too.
            if event.wd == dev
                && let Some(name) = event.name.as_ref().and_then(|n| n.to_str())
                && name.starts_with("video")
            {
                watch(&mut watches_for_new, &Path::new("/dev").join(name));
            }
            match tokio::time::timeout(SETTLE, events.next()).await {
                Ok(more) => next = more,
                Err(_) => break,
            }
        }
        let now = users();
        let joined = match (&apps, &now) {
            (Some(old), Some(new)) => new.iter().any(|a| !old.contains(a)),
            (None, Some(_)) => true,
            _ => false,
        };
        if joined {
            emitter.capture(Sensor::Camera, true, now.as_deref().unwrap_or_default());
        } else if apps.is_some() && now.is_none() {
            emitter.capture(Sensor::Camera, false, &[]);
        }
        apps = now;
    }
}

fn watch(watches: &mut inotify::Watches, device: &Path) {
    if let Err(e) = watches.add(device, WatchMask::OPEN | WatchMask::CLOSE) {
        log::debug!("watching {}: {e}", device.display());
    }
}

fn video_devices() -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("video"))
        .map(|e| e.path())
        .collect()
}

/// Processes (ours; others' fds aren't readable) with a camera open, by name, or `None`
/// when no camera is open.
fn users() -> Option<Vec<String>> {
    let mut any = false;
    let mut apps: Vec<String> = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()?.filter_map(|e| e.ok()) {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        let has_camera = fds.filter_map(|fd| fd.ok()).any(|fd| {
            std::fs::read_link(fd.path())
                .is_ok_and(|target| target.to_string_lossy().starts_with("/dev/video"))
        });
        if !has_camera {
            continue;
        }
        any = true;
        if let Some(name) = process_name(pid)
            && !MIDDLEMEN.contains(&name.as_str())
            && !apps.contains(&name)
        {
            apps.push(name);
        }
    }
    apps.sort();
    any.then_some(apps)
}

/// A process's short name (`comm`).
pub(crate) fn process_name(pid: i32) -> Option<String> {
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(name.trim().to_owned()).filter(|n| !n.is_empty())
}
