//! Media (MPRIS). Watches every player on the session bus and shows the most relevant one:
//! the most recently started player that is playing, else the most recently paused one.
//! With its album art (downloaded and cached when it's a URL) and playback position,
//! which the island can seek.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use dmsh_core::{Action, ActivitySpec, Placement, Playback, Priority};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use zbus::{
    Connection,
    fdo::DBusProxy,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue},
};

use crate::Emitter;

pub const ID: &str = "media";
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
    /// Microseconds. Players don't signal it changing (it would every moment), so it's
    /// read fresh rather than cached.
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> zbus::Result<i64>;
    #[zbus(signal)]
    fn seeked(&self, position: i64) -> zbus::Result<()>;
    fn previous(&self) -> zbus::Result<()>;
    fn play_pause(&self) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn set_position(&self, track_id: &ObjectPath<'_>, position: i64) -> zbus::Result<()>;
    fn seek(&self, offset: i64) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.mpris.MediaPlayer2",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait MediaPlayer2 {
    /// The player's own name, e.g. "Spotify" or "Mozilla Firefox".
    #[zbus(property)]
    fn identity(&self) -> zbus::Result<String>;
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
    /// The player's name for itself.
    identity: Option<String>,
    art_url: Option<String>,
    length_us: Option<i64>,
    /// As of `read_at`.
    position_us: Option<i64>,
    read_at: Instant,
}

impl Track {
    /// Where playback is now, if the player says how long the track is.
    fn playback(&self, now: Instant) -> Option<Playback> {
        let length = self.length_us.filter(|l| *l > 0)?;
        let mut position = self.position_us.unwrap_or(0).clamp(0, length);
        if self.status == Status::Playing {
            position += now.saturating_duration_since(self.read_at).as_micros() as i64;
        }
        Some(Playback {
            position_ms: (position.min(length) / 1000) as u64,
            length_ms: (length / 1000) as u64,
            playing: self.status == Status::Playing,
        })
    }
}

struct Player {
    track: Track,
    /// When the playback status last changed; picks between several players.
    status_since: Instant,
}

/// `commands` carries action keys from the media activity's buttons.
pub async fn run(emitter: Emitter, mut commands: mpsc::UnboundedReceiver<String>) -> Result<()> {
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
    // Album art URLs → the cached file, `None` while downloading or if it failed.
    let mut art: HashMap<String, Option<PathBuf>> = HashMap::new();
    let (art_tx, mut art_done) = mpsc::unbounded_channel::<(String, Option<PathBuf>)>();
    loop {
        tokio::select! {
            Some((url, path)) = art_done.recv() => {
                art.insert(url, path);
            }
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
            Some(key) = commands.recv() => {
                if let Some(name) = active(&players) {
                    control(&conn, name, &key).await;
                }
                continue;
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

        // Fetch art the shown track points to on the web (or inline), once.
        if let Some(url) = active(&players).and_then(|n| players[n].track.art_url.clone())
            && (url.starts_with("http") || url.starts_with("data:image/"))
            && !art.contains_key(&url)
        {
            art.insert(url.clone(), None);
            let tx = art_tx.clone();
            tokio::spawn(async move {
                let path = download_art(&url).await;
                let _ = tx.send((url, path));
            });
        }
        let spec = describe(&players, &art, Instant::now());
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
    let identity = async {
        MediaPlayer2Proxy::builder(conn)
            .destination(name.to_owned())?
            .build()
            .await?
            .identity()
            .await
    }
    .await
    .ok()
    .filter(|i| !i.is_empty());
    let mut status_changes = proxy.receive_playback_status_changed().await;
    let mut metadata_changes = proxy.receive_metadata_changed().await;
    let mut seeks = proxy.receive_seeked().await?;
    loop {
        let status = match proxy.playback_status().await?.as_str() {
            "Playing" => Status::Playing,
            "Paused" => Status::Paused,
            _ => Status::Stopped,
        };
        let metadata = proxy.metadata().await.unwrap_or_default();
        let get = |key: &str| metadata.get(key).and_then(|v| v.try_clone().ok());
        let track = Track {
            status,
            title: get("xesam:title")
                .and_then(|v| String::try_from(v).ok())
                .filter(|t| !t.is_empty()),
            artists: get("xesam:artist")
                .and_then(|v| Vec::<String>::try_from(v).ok())
                .unwrap_or_default(),
            identity: identity.clone(),
            art_url: get("mpris:artUrl")
                .and_then(|v| String::try_from(v).ok())
                .filter(|u| !u.is_empty()),
            // Players disagree on the integer type.
            length_us: get("mpris:length").and_then(|v| {
                i64::try_from(v.try_clone().ok()?)
                    .ok()
                    .or_else(|| u64::try_from(v).ok().map(|l| l as i64))
            }),
            position_us: proxy.position().await.ok(),
            read_at: Instant::now(),
        };
        if tx.send((name.to_owned(), track)).is_err() {
            return Ok(());
        }
        tokio::select! {
            Some(_) = status_changes.next() => {}
            Some(_) = metadata_changes.next() => {}
            Some(_) = seeks.next() => {}
            else => return Ok(()),
        }
    }
}

/// Downloads album art to `~/.cache/dmsh/art/` (keeping the newest few), or `None`.
async fn download_art(url: &str) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let dir = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?
        .join("dmsh/art");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    let path = dir.join(format!("{:016x}", hasher.finish()));
    if path.is_file() {
        return Some(path);
    }
    std::fs::create_dir_all(&dir).ok()?;
    // mpv and others put the picture right in the URL.
    if let Some(data) = url.strip_prefix("data:image/") {
        let (_, encoded) = data.split_once(";base64,")?;
        std::fs::write(&path, base64_decode(encoded)?).ok()?;
        prune_art(&dir, ART_KEPT);
        return Some(path);
    }
    let tmp = path.with_extension("part");
    let ok = tokio::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "10",
            "--max-filesize",
            "5000000",
            "-o",
        ])
        .arg(&tmp)
        .arg(url)
        .status()
        .await
        .is_ok_and(|s| s.success());
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        log::debug!("couldn't download album art {url}");
        return None;
    }
    std::fs::rename(&tmp, &path).ok()?;
    prune_art(&dir, ART_KEPT);
    Some(path)
}

/// Standard base64 (whitespace ignored), or `None` if it isn't.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            break;
        }
        bits = (bits << 6) | u32::from(value(c)?);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

/// Album art files kept in the cache.
const ART_KEPT: usize = 30;

fn prune_art(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, path) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

/// Sends a media button press to a player.
async fn control(conn: &Connection, name: &str, key: &str) {
    let result = async {
        let proxy = PlayerProxy::builder(conn)
            .destination(name.to_owned())?
            .build()
            .await?;
        if let Some(ms) = key
            .strip_prefix("seek:")
            .and_then(|ms| ms.parse::<i64>().ok())
        {
            let target = ms * 1000;
            let metadata = proxy.metadata().await.unwrap_or_default();
            let track = metadata
                .get("mpris:trackid")
                .and_then(|v| OwnedObjectPath::try_from(v.try_clone().ok()?).ok());
            return match track {
                Some(track) => proxy.set_position(&track, target).await,
                // Without a track id, seek by the difference instead.
                None => {
                    let now = proxy.position().await?;
                    proxy.seek(target - now).await
                }
            };
        }
        match key {
            "previous" => proxy.previous().await,
            "play-pause" => proxy.play_pause().await,
            "next" => proxy.next().await,
            _ => Ok(()),
        }
    }
    .await;
    if let Err(e) = result {
        log::warn!("media control `{key}` on {name}: {e}");
    }
}

/// The player shown: the most recently started one that is playing, else the most
/// recently paused one.
fn active(players: &HashMap<String, Player>) -> Option<&String> {
    let newest = |status: Status| {
        players
            .iter()
            .filter(|(_, p)| p.track.status == status)
            .max_by_key(|(_, p)| p.status_since)
            .map(|(name, _)| name)
    };
    newest(Status::Playing).or_else(|| newest(Status::Paused))
}

fn control_action(key: &str, icon: &str) -> Action {
    Action {
        icon: Some(icon.into()),
        ..Action::new(key, "")
    }
}

fn describe(
    players: &HashMap<String, Player>,
    art: &HashMap<String, Option<PathBuf>>,
    now: Instant,
) -> Option<ActivitySpec> {
    let name = active(players)?;
    let track = &players[name].track;

    let app = track.identity.clone().unwrap_or_else(|| {
        name.strip_prefix(PREFIX)
            .unwrap_or(name)
            .split('.')
            .next()
            .unwrap_or(name)
            .to_owned()
    });
    let title = track.title.clone().unwrap_or_else(|| app.clone());
    let artists = (!track.artists.is_empty()).then(|| track.artists.join(", "));
    // Local art as is; web art once it's downloaded.
    let image = track.art_url.as_ref().and_then(|url| {
        if url.starts_with("file://") {
            Some(url.clone())
        } else {
            art.get(url)?.as_ref().map(|p| p.display().to_string())
        }
    });
    Some(ActivitySpec {
        body: artists,
        app: Some(app),
        image,
        playback: track.playback(now),
        icon: Some("music".into()),
        actions: vec![
            control_action("previous", "skip-back"),
            control_action(
                "play-pause",
                if track.status == Status::Playing {
                    "pause"
                } else {
                    "play"
                },
            ),
            control_action("next", "skip-forward"),
        ],
        keep_on_action: true,
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
                identity: None,
                art_url: None,
                length_us: Some(200_000_000),
                position_us: Some(30_000_000),
                read_at: since,
            },
            status_since: since,
        }
    }

    #[test]
    fn decodes_inline_art() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVs\nbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("/9j/4A==").unwrap(), [0xFF, 0xD8, 0xFF, 0xE0]);
        assert!(base64_decode("not*base64").is_none());
    }

    #[test]
    fn describes_title_artist_player_art_and_position() {
        let t0 = Instant::now();
        let mut p = player(Status::Playing, "Instant Crush", t0);
        p.track.identity = Some("Spotify".into());
        p.track.art_url = Some("https://i.scdn.co/image/abc".into());
        let mut players = HashMap::new();
        players.insert(format!("{PREFIX}spotify"), p);
        let mut art = HashMap::new();

        let spec = describe(&players, &art, t0 + Duration::from_secs(2)).unwrap();
        assert_eq!(spec.title, "Instant Crush");
        assert_eq!(spec.body.as_deref(), Some("Artist"));
        assert_eq!(spec.app.as_deref(), Some("Spotify"));
        assert_eq!(spec.image, None, "not downloaded yet");
        let playback = spec.playback.unwrap();
        assert_eq!(playback.position_ms, 32_000, "moved on while playing");
        assert_eq!(playback.length_ms, 200_000);

        art.insert(
            "https://i.scdn.co/image/abc".to_owned(),
            Some(PathBuf::from("/cache/art/1")),
        );
        let spec = describe(&players, &art, t0).unwrap();
        assert_eq!(spec.image.as_deref(), Some("/cache/art/1"));
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

        let spec = describe(&players, &HashMap::new(), t0).unwrap();
        assert_eq!(spec.title, "new");
        assert_eq!(spec.app.as_deref(), Some("c"));
        assert_eq!(spec.ttl_ms, None);
    }

    #[test]
    fn paused_only_has_ttl_and_stopped_hides() {
        let t0 = Instant::now();
        let mut players = HashMap::new();
        players.insert(format!("{PREFIX}a"), player(Status::Paused, "x", t0));
        assert!(
            describe(&players, &HashMap::new(), t0)
                .unwrap()
                .ttl_ms
                .is_some()
        );

        players.insert(format!("{PREFIX}a"), player(Status::Stopped, "x", t0));
        assert!(describe(&players, &HashMap::new(), t0).is_none());
    }
}
