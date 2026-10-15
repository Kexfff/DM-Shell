//! Media (MPRIS). Watches every player on the session bus and shows the most relevant one:
//! the most recently started player that is playing, else the most recently paused one.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use dmsh_core::{ActivitySpec, Placement, Priority};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use zbus::{Connection, fdo::DBusProxy, zvariant::OwnedValue};

use crate::Emitter;

const ID: &str = "media";
const PREFIX: &str = "org.mpris.MediaPlayer2.";
/// playerctld mirrors the active player; showing it too would duplicate it.
const IGNORED: &[&str] = &["org.mpris.MediaPlayer2.playerctld"];
/// A paused player stays in the island this long.
const PAUSED_TTL: Duration = Duration::from_secs(5 * 60);

#[zbus::proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait Player {
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug)]
struct Track {
    status: Status,
    title: Option<String>,
    artists: Vec<String>,
}

struct Player {
    track: Track,
    /// When the playback status last changed; picks between several players.
    status_since: Instant,
}

pub async fn run(emitter: Emitter) -> Result<()> {
    let conn = Connection::session().await?;
    let dbus = DBusProxy::new(&conn).await?;
    let mut owner_changes = dbus.receive_name_owner_changed().await?;
    let (tx, mut updates) = mpsc::unbounded_channel::<(String, Track)>();

    let mut watchers: HashMap<String, AbortHandle> = HashMap::new();
    for name in dbus.list_names().await? {
        if is_player(&name) {
            let name = name.to_string();
            watchers.insert(name.clone(), watch(conn.clone(), name, tx.clone()));
        }
    }

    let mut players: HashMap<String, Player> = HashMap::new();
    let mut shown: Option<ActivitySpec> = None;
    loop {
        tokio::select! {
            Some(signal) = owner_changes.next() => {
                let Ok(args) = signal.args() else { continue };
                let name = args.name().to_string();
                if !is_player(&name) {
                    continue;
                }
                if let Some(handle) = watchers.remove(&name) {
                    handle.abort();
                    players.remove(&name);
                }
                if args.new_owner().is_some() {
                    watchers.insert(name.clone(), watch(conn.clone(), name, tx.clone()));
                }
            }
            Some((name, track)) = updates.recv() => {
                // Ignore late updates from a watcher that was already stopped.
                if !watchers.contains_key(&name) {
                    continue;
                }
                let status_since = match players.get(&name) {
                    Some(p) if p.track.status == track.status => p.status_since,
                    _ => Instant::now(),
                };
                players.insert(name, Player { track, status_since });
            }
            else => break,
        }

        let spec = describe(&players);
        if spec != shown {
            match &spec {
                Some(spec) => emitter.push(spec.clone()),
                None => emitter.dismiss(ID),
            }
            shown = spec;
        }
    }
    anyhow::bail!("session bus closed")
}

fn is_player(name: &str) -> bool {
    name.starts_with(PREFIX) && !IGNORED.contains(&name)
}

/// Follows one player's status and metadata until aborted.
fn watch(
    conn: Connection,
    name: String,
    tx: mpsc::UnboundedSender<(String, Track)>,
) -> AbortHandle {
    tokio::spawn(async move {
        if let Err(e) = follow(&conn, &name, &tx).await {
            log::debug!("media player {name}: {e}");
        }
    })
    .abort_handle()
}

async fn follow(
    conn: &Connection,
    name: &str,
    tx: &mpsc::UnboundedSender<(String, Track)>,
) -> Result<()> {
    let proxy = PlayerProxy::builder(conn)
        .destination(name.to_owned())?
        .build()
        .await?;
    let mut status_changes = proxy.receive_playback_status_changed().await;
    let mut metadata_changes = proxy.receive_metadata_changed().await;
    loop {
        let status = match proxy.playback_status().await?.as_str() {
            "Playing" => Status::Playing,
            "Paused" => Status::Paused,
            _ => Status::Stopped,
        };
        let metadata = proxy.metadata().await.unwrap_or_default();
        let track = Track {
            status,
            title: metadata
                .get("xesam:title")
                .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
                .filter(|t| !t.is_empty()),
            artists: metadata
                .get("xesam:artist")
                .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
                .unwrap_or_default(),
        };
        if tx.send((name.to_owned(), track)).is_err() {
            return Ok(());
        }
        tokio::select! {
            Some(_) = status_changes.next() => {}
            Some(_) = metadata_changes.next() => {}
            else => return Ok(()),
        }
    }
}

fn describe(players: &HashMap<String, Player>) -> Option<ActivitySpec> {
    let newest = |status: Status| {
        players
            .iter()
            .filter(|(_, p)| p.track.status == status)
            .max_by_key(|(_, p)| p.status_since)
    };
    let (name, player) = newest(Status::Playing).or_else(|| newest(Status::Paused))?;
    let track = &player.track;

    let app = name
        .strip_prefix(PREFIX)
        .unwrap_or(name)
        .split('.')
        .next()
        .unwrap_or(name);
    let title = match (&track.title, track.artists.is_empty()) {
        (Some(title), false) => format!("{} — {title}", track.artists.join(", ")),
        (Some(title), true) => title.clone(),
        (None, _) => app.to_owned(),
    };
    Some(ActivitySpec {
        body: Some(app.to_owned()),
        placement: Placement::Center,
        priority: Priority::Low,
        ttl_ms: (track.status == Status::Paused).then_some(PAUSED_TTL.as_millis() as u64),
        ..ActivitySpec::new(ID, title)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(status: Status, title: &str, since: Instant) -> Player {
        Player {
            track: Track {
                status,
                title: Some(title.into()),
                artists: vec!["Artist".into()],
            },
            status_since: since,
        }
    }

    #[test]
    fn playing_beats_paused_and_newest_wins() {
        let t0 = Instant::now();
        let mut players = HashMap::new();
        let later = t0 + Duration::from_secs(1);
        players.insert(
            format!("{PREFIX}a"),
            player(Status::Paused, "paused", later),
        );
        players.insert(format!("{PREFIX}b"), player(Status::Playing, "old", t0));
        players.insert(
            format!("{PREFIX}c.instance42"),
            player(Status::Playing, "new", later),
        );

        let spec = describe(&players).unwrap();
        assert_eq!(spec.title, "Artist — new");
        assert_eq!(spec.body.as_deref(), Some("c"));
        assert_eq!(spec.ttl_ms, None);
    }

    #[test]
    fn paused_only_has_ttl_and_stopped_hides() {
        let t0 = Instant::now();
        let mut players = HashMap::new();
        players.insert(format!("{PREFIX}a"), player(Status::Paused, "x", t0));
        assert!(describe(&players).unwrap().ttl_ms.is_some());

        players.insert(format!("{PREFIX}a"), player(Status::Stopped, "x", t0));
        assert!(describe(&players).is_none());
    }
}
