//! Notification daemon (`org.freedesktop.Notifications`). Each notification becomes a
//! `flow` activity: center for two seconds, then the right island until dismissed.
//!
//! Only one daemon can own the name; if another (mako, swaync, a Quickshell config) has
//! it, this source reports that and stays off.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use dmsh_core::{
    Action, ActivitySpec, Placement, Priority,
    activity::{MAX_ACTIONS, MAX_TEXT_LEN},
};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::{
    fdo::{RequestNameFlags, RequestNameReply},
    object_server::SignalEmitter,
    zvariant::OwnedValue,
};

use crate::{Emitter, Event, Removal};

const PREFIX: &str = "notification:";
const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

/// Reasons from the notification spec.
const CLOSED_EXPIRED: u32 = 1;
const CLOSED_DISMISSED: u32 = 2;
const CLOSED_BY_CALL: u32 = 3;

/// What the shell tells the notification daemon.
#[derive(Debug)]
pub enum Command {
    Removed(u32, Removal),
    Invoked(u32, String),
}

/// The numeric notification id behind an activity id, if it is one of ours.
pub fn parse_id(activity_id: &str) -> Option<u32> {
    activity_id.strip_prefix(PREFIX)?.parse().ok()
}

fn activity_id(id: u32) -> String {
    format!("{PREFIX}{id}")
}

struct Server {
    emitter: Emitter,
    next_id: u32,
    /// Notifications currently shown; shared with the removal listener.
    live: Arc<Mutex<HashSet<u32>>>,
}

impl Server {
    /// The best picture for a notification, most specific first: raw pixels from
    /// `image-data` (e.g. a sender's avatar, sent to the shell as an [`Event::Image`]),
    /// `image-path`, the app's icon, then the app icon in `icon_data` pixels.
    fn image(
        &self,
        id: u32,
        hints: &HashMap<String, OwnedValue>,
        app_icon: &str,
    ) -> Option<String> {
        let hint = |names: &[&str]| names.iter().find_map(|n| hints.get(*n)?.try_clone().ok());
        let pixels = |names: &[&str], icon: bool| {
            let (width, height, rgba) = image_data(hint(names)?)?;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&rgba, &mut hasher);
            let key = format!(
                "data:{}:{:x}",
                activity_id(id),
                std::hash::Hasher::finish(&hasher)
            );
            self.emitter.emit(Event::Image {
                key: key.clone(),
                width,
                height,
                rgba: Arc::new(rgba),
                icon,
            });
            Some(key)
        };
        let path = hint(&["image-path", "image_path"])
            .and_then(|v| String::try_from(v).ok())
            .filter(|p| !p.is_empty());
        pixels(&["image-data", "image_data"], false)
            .or(path)
            .or_else(|| (!app_icon.is_empty()).then(|| app_icon.to_owned()))
            .or_else(|| pixels(&["icon_data"], true))
    }
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<&str> {
        vec!["body", "persistence", "actions", "icon-static"]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &mut self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let mut live = self.live.lock().expect("lock poisoned");
        let id = if replaces_id != 0 && live.contains(&replaces_id) {
            replaces_id
        } else {
            self.next_id = self.next_id.checked_add(1).unwrap_or(1);
            self.next_id
        };
        live.insert(id);
        drop(live);

        let hint = |name: &str| hints.get(name).and_then(|v| v.try_clone().ok());
        let urgency = hint("urgency").and_then(|v| u8::try_from(v).ok());
        let resident = hint("resident").and_then(|v| bool::try_from(v).ok());
        let mut spec = spec(id, &app_name, summary, body, urgency, expire_timeout);
        spec.image = self.image(id, &hints, &app_icon);
        spec.center_ms = self.emitter.config().timing.notification_center.as_millis() as u64;
        spec.actions = parse_actions(&actions);
        spec.keep_on_action = resident.unwrap_or(false);
        self.emitter.push(spec);
        id
    }

    async fn close_notification(
        &mut self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let removed = self.live.lock().expect("lock poisoned").remove(&id);
        if removed {
            self.emitter.dismiss(&activity_id(id));
            Self::notification_closed(&emitter, id, CLOSED_BY_CALL).await?;
        }
        Ok(())
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "dmsh".into(),
            "dmsh".into(),
            env!("CARGO_PKG_VERSION").into(),
            "1.2".into(),
        )
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

fn spec(
    id: u32,
    app_name: &str,
    summary: String,
    body: String,
    urgency: Option<u8>,
    expire_timeout: i32,
) -> ActivitySpec {
    let (summary, body) = (plain_text(&summary), plain_text(&body));
    let title = if summary.is_empty() {
        app_name.to_owned()
    } else {
        summary
    };
    ActivitySpec {
        body: (!body.is_empty()).then(|| truncate(body)),
        // Used when there's no picture (or it can't be loaded).
        icon: Some("bell".into()),
        app: (!app_name.is_empty()).then(|| truncate(app_name.to_owned())),
        placement: Placement::Flow,
        priority: match urgency {
            Some(0) => Priority::Low,
            Some(2) => Priority::Critical,
            _ => Priority::Normal,
        },
        // -1 (server default) and 0 (never) both mean: stay until dismissed.
        ttl_ms: u64::try_from(expire_timeout).ok().filter(|&ms| ms > 0),
        ..ActivitySpec::new(activity_id(id), truncate(title))
    }
}

/// Converts the spec's `(iiibiiay)` image (width, height, rowstride, has alpha, bits per
/// sample, channels, data) to premultiplied RGBA.
fn image_data(value: OwnedValue) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h, stride, _alpha, bits, channels, data): (i32, i32, i32, bool, i32, i32, Vec<u8>) =
        value.try_into().ok()?;
    let (w, h, stride, channels) = (
        usize::try_from(w).ok()?,
        usize::try_from(h).ok()?,
        usize::try_from(stride).ok()?,
        usize::try_from(channels).ok()?,
    );
    // Avatars and icons are small; refuse anything absurd.
    if bits != 8 || !(3..=4).contains(&channels) || w == 0 || h == 0 || w * h > 1 << 20 {
        return None;
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = data.get(y * stride..y * stride + w * channels)?;
        for px in row.chunks_exact(channels) {
            let a = if channels == 4 { px[3] } else { 255 };
            let premul = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
            rgba.extend_from_slice(&[premul(px[0]), premul(px[1]), premul(px[2]), a]);
        }
    }
    Some((w as u32, h as u32, rgba))
}

/// Notification text as plain text: tags removed (`<b>`, `<a href=...>`, `<img .../>`),
/// entities decoded. A `<` that doesn't start a tag (`a < b`) is kept.
fn plain_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' if chars
                .peek()
                .is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!')) =>
            {
                // Skip to the end of the tag.
                for t in chars.by_ref() {
                    if t == '>' {
                        break;
                    }
                }
            }
            '&' => {
                let rest: String = chars.clone().take(10).collect();
                match rest
                    .find(';')
                    .and_then(|end| decode_entity(&rest[..end]).map(|c| (end, c)))
                {
                    Some((end, decoded)) => {
                        out.push(decoded);
                        for _ in 0..=end {
                            chars.next();
                        }
                    }
                    None => out.push('&'),
                }
            }
            _ => out.push(c),
        }
    }
    out.trim().to_owned()
}

fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Actions arrive as a flat `[key, label, key, label, ...]` list.
fn parse_actions(flat: &[String]) -> Vec<Action> {
    flat.as_chunks::<2>()
        .0
        .iter()
        .filter(|pair| !pair[0].is_empty())
        .take(MAX_ACTIONS)
        .map(|pair| Action::new(truncate(pair[0].clone()), truncate(pair[1].clone())))
        .collect()
}

/// Cuts text to the activity limit on a character boundary.
fn truncate(mut s: String) -> String {
    if s.len() > MAX_TEXT_LEN {
        let mut end = MAX_TEXT_LEN - '…'.len_utf8();
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push('…');
    }
    s
}

pub async fn run(emitter: Emitter, mut commands: mpsc::UnboundedReceiver<Command>) -> Result<()> {
    let live = Arc::new(Mutex::new(HashSet::new()));
    let server = Server {
        emitter,
        next_id: 0,
        live: live.clone(),
    };
    let conn = zbus::connection::Builder::session()?
        .serve_at(PATH, server)?
        .build()
        .await?;

    // Queue for the name: if another daemon has it, dmsh takes over when it exits.
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    let mut acquired = dbus.receive_name_acquired().await?;
    match conn
        .request_name_with_flags(NAME, enumflags2::BitFlags::<RequestNameFlags>::empty())
        .await?
    {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {
            log::info!("notification daemon running")
        }
        RequestNameReply::InQueue => {
            log::info!("{NAME} is owned by another daemon; dmsh takes over when it exits");
            tokio::spawn(async move {
                while let Some(signal) = acquired.next().await {
                    if signal.args().is_ok_and(|a| a.name() == NAME) {
                        log::info!("notification daemon running (took over {NAME})");
                        return;
                    }
                }
            });
        }
        RequestNameReply::Exists => bail!("{NAME} is owned by another daemon"),
    }

    let iface = conn.object_server().interface::<_, Server>(PATH).await?;
    while let Some(command) = commands.recv().await {
        let emitter = iface.signal_emitter();
        let result = match command {
            Command::Invoked(id, key) => {
                if !live.lock().expect("lock poisoned").contains(&id) {
                    continue;
                }
                Server::action_invoked(emitter, id, &key).await
            }
            Command::Removed(id, reason) => {
                if !live.lock().expect("lock poisoned").remove(&id) {
                    continue;
                }
                let reason = match reason {
                    Removal::Expired => CLOSED_EXPIRED,
                    Removal::Dismissed => CLOSED_DISMISSED,
                };
                Server::notification_closed(emitter, id, reason).await
            }
        };
        if let Err(e) = result {
            log::debug!("emitting notification signal: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        assert_eq!(parse_id(&activity_id(42)), Some(42));
        assert_eq!(parse_id("volume"), None);
    }

    #[test]
    fn spec_maps_urgency_timeout_and_empty_summary() {
        let s = spec(1, "Telegram", String::new(), "hi".into(), Some(2), -1);
        assert_eq!(s.title, "Telegram");
        assert_eq!(s.priority, Priority::Critical);
        assert_eq!(s.ttl_ms, None);
        assert_eq!(s.placement, Placement::Flow);

        let s = spec(1, "app", "Done".into(), String::new(), None, 5000);
        assert_eq!((s.body, s.ttl_ms), (None, Some(5000)));
    }

    #[test]
    fn actions_are_paired() {
        let flat: Vec<String> = ["default", "Open", "reply", "Reply", "dangling"]
            .map(String::from)
            .into();
        let actions = parse_actions(&flat);
        assert_eq!(actions.len(), 2);
        assert_eq!(
            (actions[1].key.as_str(), actions[1].label.as_str()),
            ("reply", "Reply")
        );
    }

    #[test]
    fn markup_becomes_plain_text() {
        assert_eq!(plain_text("<b>Bold</b> and <i>it</i>"), "Bold and it");
        assert_eq!(
            plain_text(r#"see <a href="https://x.y">the link</a>"#),
            "see the link"
        );
        assert_eq!(
            plain_text("Tom &amp; Jerry &lt;3 &#127829; &#x41;"),
            "Tom & Jerry <3 🍕 A"
        );
        assert_eq!(plain_text("a < b && c"), "a < b && c");
        assert_eq!(plain_text("<img src=\"x.png\"/>caption"), "caption");
    }

    #[test]
    fn image_data_is_premultiplied_rgba() {
        // 2x1 RGBA, rowstride 8: opaque red, half-transparent white.
        let value = zbus::zvariant::Value::from((
            2i32,
            1i32,
            8i32,
            true,
            8i32,
            4i32,
            vec![255u8, 0, 0, 255, 255, 255, 255, 128],
        ));
        let (w, h, rgba) = image_data(OwnedValue::try_from(value).unwrap()).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgba, [255, 0, 0, 255, 128, 128, 128, 128]);

        // RGB with row padding.
        let value = zbus::zvariant::Value::from((
            1i32,
            2i32,
            4i32,
            false,
            8i32,
            3i32,
            vec![1u8, 2, 3, 0, 4, 5, 6, 0],
        ));
        let (_, _, rgba) = image_data(OwnedValue::try_from(value).unwrap()).unwrap();
        assert_eq!(rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn truncates_on_char_boundary() {
        let long = "й".repeat(MAX_TEXT_LEN);
        let t = truncate(long);
        assert!(t.len() <= MAX_TEXT_LEN && t.ends_with('…'));
    }
}
