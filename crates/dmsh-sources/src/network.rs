//! Network (NetworkManager): reports when the primary connection changes, e.g. Wi-Fi
//! connected to a new network or disconnected.

use std::time::Duration;

use anyhow::{Result, bail};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::{Connection, zvariant::OwnedObjectPath};

use crate::{Emitter, Event};

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
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_wireless_enabled(&self, value: bool) -> zbus::Result<()>;
    fn get_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Device {
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;
}

/// `NM_DEVICE_TYPE_WIFI`.
const DEVICE_TYPE_WIFI: u32 = 2;

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

/// `wifi` carries on/off requests from the quick control.
pub async fn run(emitter: Emitter, mut wifi: mpsc::UnboundedReceiver<bool>) -> Result<()> {
    let conn = Connection::system().await?;
    let nm = NetworkManagerProxy::new(&conn).await?;
    let mut changes = nm.receive_primary_connection_changed().await;
    let has_wifi = has_wifi_device(&conn, &nm).await;
    // Yields the current value first, then every change.
    let mut radio = nm.receive_wireless_enabled_changed().await;

    let mut last = primary(&conn, &nm).await;
    loop {
        tokio::select! {
            change = changes.next() => {
                if change.is_none() {
                    bail!("NetworkManager stopped");
                }
                while let Ok(Some(_)) = tokio::time::timeout(SETTLE, changes.next()).await {}
                let now = primary(&conn, &nm).await;
                if now != last {
                    if let Some(title) = message(last.as_ref(), now.as_ref()) {
                        emitter.push(emitter.status(ID, icon(now.as_ref()), title));
                    }
                    last = now;
                }
            }
            Some(change) = radio.next(), if has_wifi => {
                if let Ok(on) = change.get().await {
                    emitter.emit(Event::Wifi(on));
                }
            }
            Some(on) = wifi.recv() => {
                if let Err(e) = nm.set_wireless_enabled(on).await {
                    log::warn!("turning Wi-Fi {}: {e}", if on { "on" } else { "off" });
                }
            }
        }
    }
}

async fn has_wifi_device(conn: &Connection, nm: &NetworkManagerProxy<'_>) -> bool {
    for path in nm.get_devices().await.unwrap_or_default() {
        let Ok(builder) = DeviceProxy::builder(conn).path(path) else {
            continue;
        };
        if let Ok(device) = builder.build().await
            && device.device_type().await == Ok(DEVICE_TYPE_WIFI)
        {
            return true;
        }
    }
    false
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
