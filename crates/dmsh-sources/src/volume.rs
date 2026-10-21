//! Volume OSDs and the volume and microphone sliders. Listens to `pactl subscribe` (works
//! on PipeWire via pipewire-pulse) and reads the default output or input with `wpctl`
//! only when a sink, a source or the server changed, so nothing runs while the volume is
//! untouched. Changes from the sliders are set with `wpctl` too.

use std::process::Stdio;
use std::time::Instant;

use anyhow::{Context, Result};
use dmsh_core::Level;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::{Emitter, Event, QUIET_AFTER_SET, Retry, SETTLE};

/// The default output (speakers, headphones) or input (microphone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Output,
    Input,
}

impl Device {
    const ALL: [Device; 2] = [Device::Output, Device::Input];

    fn target(self) -> &'static str {
        match self {
            Device::Output => "@DEFAULT_AUDIO_SINK@",
            Device::Input => "@DEFAULT_AUDIO_SOURCE@",
        }
    }

    /// The `pactl subscribe` events about this kind of device.
    fn event(self) -> &'static str {
        match self {
            Device::Output => " on sink #",
            Device::Input => " on source #",
        }
    }

    fn emit(self, emitter: &Emitter, volume: Option<Volume>) {
        let level = volume.map(Volume::level);
        emitter.emit(match self {
            Device::Output => Event::Volume(level),
            Device::Input => Event::Mic(level),
        });
    }

    fn osd(self, emitter: &Emitter, v: Volume) {
        let (id, icon, title) = match (self, v.muted) {
            (Device::Output, false) => ("volume", "volume", "Volume"),
            (Device::Output, true) => ("volume", "volume-mute", "Muted"),
            (Device::Input, false) => ("mic-volume", "mic", "Microphone"),
            (Device::Input, true) => ("mic-volume", "mic-off", "Microphone muted"),
        };
        emitter.push(emitter.osd(id, icon, title, Some(v.percent as f32 / 100.0)));
    }
}

/// Changes asked for by the islands.
#[derive(Debug)]
pub enum SetVolume {
    /// 0.0 to 1.0; also unmutes.
    Level(Device, f32),
    ToggleMute(Device),
}

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

/// Follows the output and input volume until the audio server goes away.
async fn watch(emitter: &Emitter, commands: &mut mpsc::UnboundedReceiver<SetVolume>) -> Result<()> {
    // Requests made while the server was away are stale now.
    while commands.try_recv().is_ok() {}
    // An output is required; a machine without a microphone just has no input.
    let mut last = [
        Some(read(Device::Output).await?),
        read(Device::Input).await.ok(),
    ];
    for (device, volume) in Device::ALL.into_iter().zip(last) {
        device.emit(emitter, volume);
    }
    // Changes made from the sliders don't need an OSD on top of them.
    let mut quiet_until = [Instant::now(); 2];
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
                // A slider drag sends many; only the newest level per device matters.
                let mut wanted: [(Option<f32>, bool); 2] = [(None, false); 2];
                let mut next = Some(command);
                while let Some(command) = next {
                    match command {
                        SetVolume::Level(d, v) => wanted[d as usize].0 = Some(v),
                        SetVolume::ToggleMute(d) => wanted[d as usize].1 ^= true,
                    }
                    next = commands.try_recv().ok();
                }
                for device in Device::ALL {
                    let (level, toggle) = wanted[device as usize];
                    if level.is_none() && !toggle {
                        continue;
                    }
                    let muted = last[device as usize].is_some_and(|v| v.muted);
                    if let Err(e) = set(device, level, toggle, muted).await {
                        log::warn!("setting {device:?} volume: {e:#}");
                    }
                    quiet_until[device as usize] = Instant::now() + QUIET_AFTER_SET;
                }
                continue;
            }
        };
        // "Event 'change' on sink #56", "... on source #60", or "... on server" (a
        // default device changed). Coalesce a burst (a held volume key) into one read.
        let mut changed = [false; 2];
        let mut note = |line: &str| {
            for device in Device::ALL {
                if line.contains(device.event()) || line.contains(" on server") {
                    changed[device as usize] = true;
                }
            }
        };
        note(&line);
        while let Ok(Ok(Some(line))) = tokio::time::timeout(SETTLE, lines.next_line()).await {
            note(&line);
        }
        for device in Device::ALL.into_iter().filter(|d| changed[*d as usize]) {
            let now = match read(device).await {
                Ok(v) => Some(v),
                // The microphone was unplugged; the output stays (it may be switching).
                Err(_) if device == Device::Input => None,
                Err(e) => {
                    log::debug!("reading volume: {e:#}");
                    continue;
                }
            };
            let slot = &mut last[device as usize];
            if now == *slot {
                continue;
            }
            // Only a change of the same device is worth an OSD, not one appearing.
            let was_there = slot.is_some();
            *slot = now;
            device.emit(emitter, now);
            if let Some(v) = now
                && was_there
                && Instant::now() >= quiet_until[device as usize]
            {
                device.osd(emitter, v);
            }
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

async fn set(device: Device, level: Option<f32>, toggle_mute: bool, muted: bool) -> Result<()> {
    let target = device.target();
    if let Some(v) = level {
        let v = format!("{:.3}", v.clamp(0.0, 1.0));
        wpctl(&["set-volume", target, &v]).await?;
        // Turning it up while muted means "I want it on".
        if muted && !toggle_mute {
            wpctl(&["set-mute", target, "0"]).await?;
        }
    }
    if toggle_mute {
        wpctl(&["set-mute", target, "toggle"]).await?;
    }
    Ok(())
}

async fn read(device: Device) -> Result<Volume> {
    let out = Command::new("wpctl")
        .args(["get-volume", device.target()])
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
                if let Event::Volume(Some(level)) = e {
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
