//! Event sources that feed the islands: niri, clock, volume, brightness, media, power,
//! network and notifications.
//!
//! Sources run on their own thread with a single-threaded tokio runtime, so the Wayland
//! thread never blocks on D-Bus or subprocesses. They talk to the shell with the same
//! [`Request`]s IPC clients use, plus a few shell-only events. A source that can't start
//! (service missing, name taken) logs a warning and the others keep running.

mod brightness;
mod clock;
mod media;
mod network;
mod niri;
mod notifications;
mod power;
mod volume;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

pub use dmsh_core::ipc::Removal;
use dmsh_core::{ActivitySpec, Placement, ipc::Request};
use tokio::sync::mpsc;

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
}

/// What the shell tells sources.
#[derive(Debug)]
pub enum Command {
    /// An activity left the shell without its source asking for it.
    Removed { id: String, reason: Removal },
    /// One of the activity's actions was invoked (clicked, or via IPC).
    Invoked { id: String, key: String },
}

/// Lets the shell send [`Command`]s to the sources thread.
pub struct Handle {
    commands: mpsc::UnboundedSender<Command>,
}

impl Handle {
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

/// Starts all sources on a background thread; `emit` is called from that thread.
pub fn spawn(emit: impl Fn(Event) + Send + Sync + 'static) -> std::io::Result<Handle> {
    let (tx, rx) = mpsc::unbounded_channel();
    let emitter = Emitter(Arc::new(emit));
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
    Ok(Handle { commands: tx })
}

async fn run(emitter: Emitter, mut commands: mpsc::UnboundedReceiver<Command>) {
    let (notifications_tx, notifications_rx) = mpsc::unbounded_channel();
    let (media_tx, media_rx) = mpsc::unbounded_channel();

    tokio::spawn(supervise("clock", clock::run(emitter.clone())));
    tokio::spawn(supervise("niri", niri::run(emitter.clone())));
    tokio::spawn(supervise("volume", volume::run(emitter.clone())));
    tokio::spawn(supervise("brightness", brightness::run(emitter.clone())));
    tokio::spawn(supervise("media", media::run(emitter.clone(), media_rx)));
    tokio::spawn(supervise("power", power::run(emitter.clone())));
    tokio::spawn(supervise("network", network::run(emitter.clone())));
    tokio::spawn(supervise(
        "notifications",
        notifications::run(emitter, notifications_rx),
    ));

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
        }
    }
}

async fn supervise(name: &str, source: impl Future<Output = anyhow::Result<()>>) {
    match source.await {
        Ok(()) => log::info!("{name} source stopped"),
        Err(e) => log::warn!("{name} source unavailable: {e:#}"),
    }
}

/// Sends events to the shell.
#[derive(Clone)]
struct Emitter(Arc<dyn Fn(Event) + Send + Sync>);

impl Emitter {
    fn emit(&self, event: Event) {
        (self.0)(event)
    }

    fn push(&self, spec: ActivitySpec) {
        self.emit(Event::Request(Request::Push(spec)));
    }

    fn dismiss(&self, id: &str) {
        self.emit(Event::Request(Request::Dismiss { id: id.into() }));
    }
}

/// A short center overlay (volume, brightness, keyboard layout).
fn osd(id: &str, icon: &str, title: impl Into<String>, progress: Option<f32>) -> ActivitySpec {
    ActivitySpec {
        progress,
        icon: Some(icon.into()),
        placement: Placement::Center,
        ttl_ms: Some(1500),
        ..ActivitySpec::new(id, title)
    }
}

/// A short right-island status message (Wi-Fi, charger).
fn status(id: &str, icon: &str, title: impl Into<String>) -> ActivitySpec {
    ActivitySpec {
        icon: Some(icon.into()),
        placement: Placement::Right,
        priority: dmsh_core::Priority::Low,
        ttl_ms: Some(4000),
        ..ActivitySpec::new(id, title)
    }
}

/// Bursty sources (a held volume key, a flapping connection) wait this long after an
/// event for more before reporting once.
const SETTLE: Duration = Duration::from_millis(15);
