//! Network (NetworkManager): reports when the primary connection changes, e.g. Wi-Fi
//! connected to a new network or disconnected.

use std::time::Duration;

use anyhow::{Result, bail};
use futures_util::StreamExt;
use zbus::{Connection, zvariant::OwnedObjectPath};

use crate::{Emitter, status};

const ID: &str = "network";
/// The primary connection flaps during roaming and reconnects; report only where it
/// settles.
const SETTLE: Duration = Duration::from_millis(1500);

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    #[zbus(property)]
    fn primary_connection(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait ActiveConnection {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
    #[zbus(property, name = "Type")]
    fn connection_type(&self) -> zbus::Result<String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Primary {
    name: String,
    kind: String,
}

pub async fn run(emitter: Emitter) -> Result<()> {
    let conn = Connection::system().await?;
    let nm = NetworkManagerProxy::new(&conn).await?;
    let mut changes = nm.receive_primary_connection_changed().await;

    let mut last = primary(&conn, &nm).await;
    loop {
        if changes.next().await.is_none() {
            bail!("NetworkManager stopped");
        }
        while let Ok(Some(_)) = tokio::time::timeout(SETTLE, changes.next()).await {}

        let now = primary(&conn, &nm).await;
        if now != last {
            if let Some(title) = message(last.as_ref(), now.as_ref()) {
                emitter.push(status(ID, icon(now.as_ref()), title));
            }
            last = now;
        }
    }
}

async fn primary(conn: &Connection, nm: &NetworkManagerProxy<'_>) -> Option<Primary> {
    let path = nm.primary_connection().await.ok()?;
    if path.as_str() == "/" {
        return None;
    }
    let active = ActiveConnectionProxy::builder(conn)
        .path(path)
        .ok()?
        .build()
        .await
        .ok()?;
    Some(Primary {
        name: active.id().await.ok()?,
        kind: active.connection_type().await.ok()?,
    })
}

fn icon(primary: Option<&Primary>) -> &'static str {
    match primary {
        Some(p) if p.kind == "802-11-wireless" => "wifi",
        Some(_) => "globe",
        None => "wifi-off",
    }
}

fn message(old: Option<&Primary>, new: Option<&Primary>) -> Option<String> {
    match new {
        Some(p) if p.kind == "802-11-wireless" => Some(format!("Wi-Fi: {}", p.name)),
        Some(p) if p.kind == "802-3-ethernet" => Some("Ethernet connected".into()),
        Some(p) => Some(format!("Connected: {}", p.name)),
        None if old.is_some() => Some("Disconnected".into()),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wifi(name: &str) -> Primary {
        Primary {
            name: name.into(),
            kind: "802-11-wireless".into(),
        }
    }

    #[test]
    fn messages() {
        assert_eq!(
            message(None, Some(&wifi("Home"))).as_deref(),
            Some("Wi-Fi: Home")
        );
        assert_eq!(
            message(Some(&wifi("Home")), None).as_deref(),
            Some("Disconnected")
        );
        assert_eq!(message(None, None), None);
    }
}
