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
use tokio::sync::mpsc;
use zbus::{
    fdo::{RequestNameFlags, RequestNameReply},
    object_server::SignalEmitter,
    zvariant::OwnedValue,
};

use crate::{Emitter, Removal};

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

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<&str> {
        vec!["body", "persistence", "actions"]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &mut self,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
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
    let title = if summary.is_empty() {
        app_name.to_owned()
    } else {
        summary
    };
    ActivitySpec {
        body: (!body.is_empty()).then(|| truncate(body)),
        icon: Some("bell".into()),
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

    let reply = conn
        .request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into())
        .await;
    match reply {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            log::info!("notification daemon running")
        }
        _ => bail!("{NAME} is owned by another daemon; stop it to let dmsh show notifications"),
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
    fn truncates_on_char_boundary() {
        let long = "й".repeat(MAX_TEXT_LEN);
        let t = truncate(long);
        assert!(t.len() <= MAX_TEXT_LEN && t.ends_with('…'));
    }
}
