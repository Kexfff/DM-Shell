//! Bluetooth glance (BlueZ on the system bus): "Ear (2) connected · 80%" on the right
//! island when a device connects (the battery is filled in if it arrives a moment
//! later), "… disconnected" when it goes, and a warning once its battery runs low.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use dmsh_core::{ActivitySpec, Placement, Priority};
use futures_util::StreamExt;
use zbus::{
    Connection, MatchRule, MessageStream, fdo::ObjectManagerProxy, message::Type,
    zvariant::OwnedValue,
};

use crate::Emitter;

/// At or below this, warn (once per connection, or until it's charged again).
const LOW: u8 = 15;
const RECOVERED: u8 = 25;
/// Headphones often report their battery a few seconds after connecting; until then a
/// battery reading updates the "connected" message instead of being news on its own.
const BATTERY_GRACE: Duration = Duration::from_secs(15);

#[derive(Debug, Default)]
struct Device {
    alias: Option<String>,
    /// BlueZ's icon name, e.g. `audio-headset`, `input-mouse`.
    kind: Option<String>,
    connected: bool,
    battery: Option<u8>,
    connected_at: Option<Instant>,
    warned_low: bool,
}

impl Device {
    fn apply(&mut self, interface: &str, props: &HashMap<String, OwnedValue>) {
        let get = |key: &str| props.get(key).and_then(|v| v.try_clone().ok());
        match interface {
            "org.bluez.Device1" => {
                if let Some(alias) = get("Alias").and_then(|v| String::try_from(v).ok()) {
                    self.alias = Some(alias);
                }
                if let Some(kind) = get("Icon").and_then(|v| String::try_from(v).ok()) {
                    self.kind = Some(kind);
                }
                if let Some(connected) = get("Connected").and_then(|v| bool::try_from(v).ok()) {
                    self.connected = connected;
                }
            }
            "org.bluez.Battery1" => {
                if let Some(p) = get("Percentage").and_then(|v| u8::try_from(v).ok()) {
                    self.battery = Some(p);
                }
            }
            _ => {}
        }
    }

    fn icon(&self) -> &'static str {
        match self.kind.as_deref() {
            Some(k) if k.starts_with("audio-") => "headphones",
            _ => "bluetooth",
        }
    }
}

pub async fn run(emitter: Emitter) -> Result<()> {
    let conn = Connection::system().await?;
    let manager = ObjectManagerProxy::builder(&conn)
        .destination("org.bluez")?
        .path("/")?
        .build()
        .await?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .path_namespace("/org/bluez")?
        .build();
    let mut changes = MessageStream::for_match_rule(rule, &conn, None).await?;
    let mut added = manager.receive_interfaces_added().await?;
    let mut removed = manager.receive_interfaces_removed().await?;

    // What's connected already isn't news.
    let mut devices: HashMap<String, Device> = HashMap::new();
    for (path, interfaces) in manager.get_managed_objects().await? {
        let device = devices.entry(path.to_string()).or_default();
        for (interface, props) in &interfaces {
            device.apply(interface.as_str(), props);
        }
    }

    loop {
        tokio::select! {
            Some(message) = changes.next() => {
                let Ok(message) = message else { continue };
                let Some(path) = message.header().path().map(|p| p.to_string()) else {
                    continue;
                };
                type Changed = (String, HashMap<String, OwnedValue>, Vec<String>);
                let Ok((interface, props, _)) = message.body().deserialize::<Changed>() else {
                    continue;
                };
                changed(&emitter, &mut devices, path, &interface, &props);
            }
            Some(signal) = added.next() => {
                let Ok(args) = signal.args() else { continue };
                let path = args.object_path().to_string();
                for (interface, props) in args.interfaces_and_properties() {
                    let props: HashMap<String, OwnedValue> = props
                        .iter()
                        .filter_map(|(k, v)| Some((k.to_string(), v.try_to_owned().ok()?)))
                        .collect();
                    changed(&emitter, &mut devices, path.clone(), interface.as_str(), &props);
                }
            }
            Some(signal) = removed.next() => {
                let Ok(args) = signal.args() else { continue };
                if args.interfaces().iter().any(|i| i.as_str() == "org.bluez.Device1") {
                    devices.remove(args.object_path().as_str());
                }
            }
            else => break,
        }
    }
    anyhow::bail!("lost BlueZ")
}

fn changed(
    emitter: &Emitter,
    devices: &mut HashMap<String, Device>,
    path: String,
    interface: &str,
    props: &HashMap<String, OwnedValue>,
) {
    let id = format!("bluetooth:{}", path.rsplit('/').next().unwrap_or(&path));
    let device = devices.entry(path).or_default();
    let (was_connected, old_battery) = (device.connected, device.battery);
    device.apply(interface, props);
    let now = Instant::now();
    for spec in news(device, &id, was_connected, old_battery, now) {
        emitter.push(ActivitySpec {
            ttl_ms: spec
                .ttl_ms
                .or(Some(emitter.config().timing.status.as_millis() as u64)),
            ..spec
        });
    }
}

/// What to show after a device changed; updates the device's bookkeeping.
fn news(
    device: &mut Device,
    id: &str,
    was_connected: bool,
    old_battery: Option<u8>,
    now: Instant,
) -> Vec<ActivitySpec> {
    let name = device
        .alias
        .clone()
        .unwrap_or_else(|| "Bluetooth device".into());
    let connected_message = |device: &Device| {
        let title = match device.battery {
            Some(p) => format!("{name} connected · {p}%"),
            None => format!("{name} connected"),
        };
        ActivitySpec {
            icon: Some(device.icon().into()),
            placement: Placement::Right,
            priority: Priority::Low,
            ..ActivitySpec::new(id, title)
        }
    };
    let mut out = Vec::new();
    if device.connected && !was_connected {
        device.connected_at = Some(now);
        device.warned_low = false;
        out.push(connected_message(device));
    } else if !device.connected && was_connected {
        device.connected_at = None;
        out.push(ActivitySpec {
            icon: Some(device.icon().into()),
            placement: Placement::Right,
            priority: Priority::Low,
            ..ActivitySpec::new(id, format!("{name} disconnected"))
        });
        return out;
    } else if device.connected
        && device.battery != old_battery
        && device
            .connected_at
            .is_some_and(|t| now.saturating_duration_since(t) < BATTERY_GRACE)
    {
        out.push(connected_message(device));
    }
    if let Some(p) = device.battery.filter(|_| device.connected) {
        if p <= LOW && !device.warned_low {
            device.warned_low = true;
            out.push(ActivitySpec {
                icon: Some("battery-low".into()),
                placement: Placement::Flow,
                priority: Priority::High,
                ttl_ms: Some(10 * 60_000),
                ..ActivitySpec::new(format!("{id}:low"), format!("{name} battery low · {p}%"))
            });
        } else if p >= RECOVERED {
            device.warned_low = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn earbuds() -> Device {
        Device {
            alias: Some("Ear (2)".into()),
            kind: Some("audio-headset".into()),
            ..Device::default()
        }
    }

    #[test]
    fn connect_battery_and_disconnect() {
        let t0 = Instant::now();
        let mut d = earbuds();
        d.connected = true;
        let n = news(&mut d, "bluetooth:x", false, None, t0);
        assert_eq!(n[0].title, "Ear (2) connected");
        assert_eq!(n[0].icon.as_deref(), Some("headphones"));

        // The battery shows up a moment later: the same message, now with it.
        d.battery = Some(80);
        let n = news(
            &mut d,
            "bluetooth:x",
            true,
            None,
            t0 + Duration::from_secs(3),
        );
        assert_eq!(
            (n[0].id.as_str(), n[0].title.as_str()),
            ("bluetooth:x", "Ear (2) connected · 80%")
        );

        // Later battery changes aren't news.
        d.battery = Some(70);
        assert!(
            news(
                &mut d,
                "bluetooth:x",
                true,
                Some(80),
                t0 + Duration::from_secs(600)
            )
            .is_empty()
        );

        d.connected = false;
        let n = news(&mut d, "bluetooth:x", true, Some(70), t0);
        assert_eq!(n[0].title, "Ear (2) disconnected");
    }

    #[test]
    fn warns_once_when_low() {
        let t0 = Instant::now() + Duration::from_secs(3600);
        let mut d = earbuds();
        d.connected = true;
        d.connected_at = Some(t0 - Duration::from_secs(3000));
        d.battery = Some(14);
        let n = news(&mut d, "bluetooth:x", true, Some(16), t0);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].title, "Ear (2) battery low · 14%");
        assert_eq!(n[0].priority, Priority::High);
        d.battery = Some(12);
        assert!(
            news(&mut d, "bluetooth:x", true, Some(14), t0).is_empty(),
            "once"
        );
        d.battery = Some(30);
        news(&mut d, "bluetooth:x", true, Some(12), t0);
        d.battery = Some(10);
        assert_eq!(
            news(&mut d, "bluetooth:x", true, Some(30), t0).len(),
            1,
            "again after charging"
        );
    }

    #[test]
    fn mice_and_keyboards_get_the_bluetooth_icon() {
        let d = Device {
            kind: Some("input-mouse".into()),
            ..Device::default()
        };
        assert_eq!(d.icon(), "bluetooth");
    }
}
