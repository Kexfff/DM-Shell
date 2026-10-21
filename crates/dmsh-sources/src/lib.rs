//! Event sources that feed the islands: niri, clock, volume, brightness, media, power,
//! network and notifications.
//!
//! Sources run on their own thread with a single-threaded tokio runtime, so the Wayland
//! thread never blocks on D-Bus or subprocesses. They talk to the shell with the same
//! [`Request`]s IPC clients use, plus a few shell-only events. A source that can't start
//! (service missing, name taken) logs a warning and the others keep running.

mod brightness;
mod camera;
mod clock;
mod media;
mod microphone;
mod network;
mod niri;
mod notifications;
mod power;
pub mod tray;
pub mod volume;

use std::future::Future;
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::Duration;

pub use dmsh_core::ipc::Removal;
use dmsh_core::{ActivitySpec, Placement, Sensor, config::Config, ipc::Request};
use tokio::sync::{Notify, mpsc};

/// What sources tell the shell.
#[derive(Debug)]
pub enum Event {
    Request(Request),
    /// Connector name of the focused output, e.g. `eDP-1`.
    FocusedOutput(Option<String>),
    /// Idle clock text, e.g. `13:08` and `Thursday, 15 October`; sent at every minute
    /// boundary.
    Clock {
        time: String,
        date: String,
    },
    /// The visible tray items, in a stable order.
    Tray(Vec<tray::TrayItem>),
    /// A tray item's menu was fetched (open it), or `None`.
    Menu(Option<tray::Menu>),
    /// Battery state on every change; never sent without a battery.
    Battery(dmsh_core::BatteryView),
    /// Wi-Fi radio on/off; sent only when there is a Wi-Fi device.
    Wifi(bool),
    /// The default audio output's volume, at startup and whenever it changes.
    Volume(Option<dmsh_core::Level>),
    /// The default audio input's (microphone's) volume; `None` without one.
    Mic(Option<dmsh_core::Level>),
    /// The backlight level, at startup and whenever it changes.
    Brightness(dmsh_core::Level),
    /// The screen, a microphone or a camera started or stopped being captured.
    Privacy(dmsh_core::Sensor, bool),
    /// Raw pixels an activity refers to as its `image` (`key`), e.g. a notification's
    /// avatar. Premultiplied RGBA; `icon` marks artwork rather than a photo.
    Image {
        key: String,
        width: u32,
        height: u32,
        rgba: Arc<Vec<u8>>,
        icon: bool,
    },
}

/// What the shell tells sources.
#[derive(Debug)]
pub enum Command {
    /// An activity left the shell without its source asking for it.
    Removed {
        id: String,
        reason: Removal,
    },
    /// One of the activity's actions was invoked (clicked, or via IPC).
    Invoked {
        id: String,
        key: String,
    },
    Tray(tray::Command),
    /// Turn the Wi-Fi radio on or off.
    SetWifi(bool),
    SetVolume(volume::SetVolume),
    /// 0.0 to 1.0.
    SetBrightness(f32),
}

/// Lets the shell send [`Command`]s to the sources thread.
pub struct Handle {
    commands: mpsc::UnboundedSender<Command>,
    config_changed: Arc<Notify>,
}

impl Handle {
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Call after writing a new config into the shared lock; sources re-read it.
    pub fn config_changed(&self) {
        self.config_changed.notify_waiters();
    }
}

/// Starts the enabled sources on a background thread; `emit` is called from that thread.
/// Sources read timings and formats from `config` whenever they use them.
pub fn spawn(
    config: Arc<RwLock<Config>>,
    emit: impl Fn(Event) + Send + Sync + 'static,
) -> std::io::Result<Handle> {
    let (tx, rx) = mpsc::unbounded_channel();
    let config_changed = Arc::new(Notify::new());
    let emitter = Emitter {
        emit: Arc::new(emit),
        config,
        config_changed: config_changed.clone(),
    };
    std::thread::Builder::new()
        .name("dmsh-sources".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match runtime {
                Ok(rt) => rt.block_on(run(emitter, rx)),
                Err(e) => log::error!("starting sources runtime: {e}"),
            }
        })?;
    Ok(Handle {
        commands: tx,
        config_changed,
    })
}

async fn run(emitter: Emitter, mut commands: mpsc::UnboundedReceiver<Command>) {
    let (notifications_tx, notifications_rx) = mpsc::unbounded_channel();
    let (media_tx, media_rx) = mpsc::unbounded_channel();
    let (tray_tx, tray_rx) = mpsc::unbounded_channel();
    let (network_tx, network_rx) = mpsc::unbounded_channel();
    let (volume_tx, volume_rx) = mpsc::unbounded_channel();
    let (brightness_tx, brightness_rx) = mpsc::unbounded_channel();

    let enabled = emitter.config().sources.clone();
    tokio::spawn(supervise("clock", clock::run(emitter.clone())));
    tokio::spawn(supervise("niri", niri::run(emitter.clone())));
    if enabled.volume {
        tokio::spawn(supervise("volume", volume::run(emitter.clone(), volume_rx)));
    }
    if enabled.brightness {
        tokio::spawn(supervise(
            "brightness",
            brightness::run(emitter.clone(), brightness_rx),
        ));
    }
    if enabled.microphone {
        tokio::spawn(supervise("microphone", microphone::run(emitter.clone())));
    }
    if enabled.camera {
        tokio::spawn(supervise("camera", camera::run(emitter.clone())));
    }
    if enabled.media {
        tokio::spawn(supervise("media", media::run(emitter.clone(), media_rx)));
    }
    if enabled.power {
        tokio::spawn(supervise("power", power::run(emitter.clone())));
    }
    if enabled.network {
        tokio::spawn(supervise(
            "network",
            network::run(emitter.clone(), network_rx),
        ));
    }
    if enabled.tray {
        tokio::spawn(supervise("tray", tray::run(emitter.clone(), tray_rx)));
    }
    if enabled.notifications {
        tokio::spawn(supervise(
            "notifications",
            notifications::run(emitter, notifications_rx),
        ));
    }

    // Route each command to the source that owns the activity.
    while let Some(command) = commands.recv().await {
        match command {
            Command::Removed { id, reason } => {
                if let Some(n) = notifications::parse_id(&id) {
                    let _ = notifications_tx.send(notifications::Command::Removed(n, reason));
                }
            }
            Command::Invoked { id, key } => {
                if let Some(n) = notifications::parse_id(&id) {
                    let _ = notifications_tx.send(notifications::Command::Invoked(n, key));
                } else if id == media::ID {
                    let _ = media_tx.send(key);
                }
            }
            Command::Tray(command) => {
                let _ = tray_tx.send(command);
            }
            Command::SetWifi(on) => {
                let _ = network_tx.send(on);
            }
            Command::SetVolume(set) => {
                let _ = volume_tx.send(set);
            }
            Command::SetBrightness(value) => {
                let _ = brightness_tx.send(value);
            }
        }
    }
}

async fn supervise(name: &str, source: impl Future<Output = anyhow::Result<()>>) {
    match source.await {
        Ok(()) => log::info!("{name} source stopped"),
        Err(e) => log::warn!("{name} source unavailable: {e:#}"),
    }
}

/// Sends events to the shell and gives sources the current config.
#[derive(Clone)]
struct Emitter {
    emit: Arc<dyn Fn(Event) + Send + Sync>,
    config: Arc<RwLock<Config>>,
    config_changed: Arc<Notify>,
}

impl Emitter {
    fn emit(&self, event: Event) {
        (self.emit)(event)
    }

    fn push(&self, spec: ActivitySpec) {
        self.emit(Event::Request(Request::Push(spec)));
    }

    fn dismiss(&self, id: &str) {
        self.emit(Event::Request(Request::Dismiss { id: id.into() }));
    }

    fn config(&self) -> RwLockReadGuard<'_, Config> {
        self.config.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Resolves when the shell reloads the config.
    async fn config_changed(&self) {
        self.config_changed.notified().await
    }

    /// A short center overlay (volume, brightness, keyboard layout).
    fn osd(
        &self,
        id: &str,
        icon: &str,
        title: impl Into<String>,
        progress: Option<f32>,
    ) -> ActivitySpec {
        ActivitySpec {
            progress,
            icon: Some(icon.into()),
            placement: Placement::Center,
            ttl_ms: Some(self.config().timing.osd.as_millis() as u64),
            ..ActivitySpec::new(id, title)
        }
    }

    /// Something started capturing the user: the indicator icon, plus "… in use" on the
    /// right island for `timing.status`. `on: false` takes both away.
    fn capture(&self, sensor: Sensor, on: bool, apps: &[String]) {
        let (id, icon, title) = match sensor {
            Sensor::Screen => ("privacy:screen", "screen-share", "Screen is being shared"),
            Sensor::Mic => ("privacy:mic", "mic", "Microphone in use"),
            Sensor::Camera => ("privacy:camera", "camera", "Camera in use"),
        };
        self.emit(Event::Privacy(sensor, on));
        if !on {
            self.dismiss(id);
            return;
        }
        self.push(ActivitySpec {
            icon: Some(icon.into()),
            app: (!apps.is_empty()).then(|| apps.join(", ")),
            placement: Placement::Right,
            priority: dmsh_core::Priority::High,
            ttl_ms: Some(self.config().timing.status.as_millis() as u64),
            ..ActivitySpec::new(id, title)
        });
    }

    /// A short right-island status message (Wi-Fi, charger).
    fn status(&self, id: &str, icon: &str, title: impl Into<String>) -> ActivitySpec {
        ActivitySpec {
            icon: Some(icon.into()),
            placement: Placement::Right,
            priority: dmsh_core::Priority::Low,
            ttl_ms: Some(self.config().timing.status.as_millis() as u64),
            ..ActivitySpec::new(id, title)
        }
    }
}

/// Bursty sources (a held volume key, a flapping connection) wait this long after an
/// event for more before reporting once.
const SETTLE: Duration = Duration::from_millis(15);

/// Restarts a source that lost its service: the audio server may come up a moment after
/// dmsh at login, or restart later. Waits 1 s, doubling up to 30 s, and starts over after
/// a run that lasted a minute. A missing program (no `pactl`) is final.
struct Retry {
    name: &'static str,
    delay: Duration,
    started: tokio::time::Instant,
}

impl Retry {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            delay: Duration::from_secs(1),
            started: tokio::time::Instant::now(),
        }
    }

    /// Call when a run starts.
    fn start(&mut self) {
        self.started = tokio::time::Instant::now();
    }

    /// Call when a run ended with `error`; returns it back if retrying is pointless.
    async fn after(&mut self, error: anyhow::Error) -> anyhow::Result<()> {
        let missing = error.chain().any(|e| {
            e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        });
        if missing {
            return Err(error);
        }
        if self.started.elapsed() > Duration::from_secs(60) {
            self.delay = Duration::from_secs(1);
        }
        // Loud once per outage, quiet while it lasts.
        if self.delay == Duration::from_secs(1) {
            log::warn!("{}: {error:#}; retrying", self.name);
        } else {
            log::debug!("{}: {error:#}; retrying in {:?}", self.name, self.delay);
        }
        tokio::time::sleep(self.delay).await;
        self.delay = (self.delay * 2).min(Duration::from_secs(30));
        Ok(())
    }
}

/// After the islands change the volume or brightness, changes for this long don't show
/// an OSD (the slider already shows them).
const QUIET_AFTER_SET: Duration = Duration::from_secs(1);
