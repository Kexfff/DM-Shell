//! Battery (UPower display device): charger plugged/unplugged, fully charged, and low or
//! critical battery warnings.

use anyhow::{Result, bail};
use dmsh_core::{ActivitySpec, BatteryLevel, BatteryView, Placement, Priority};
use futures_util::StreamExt;
use zbus::Connection;

use crate::{Emitter, Event};

const CHARGER_ID: &str = "power";
const LOW_ID: &str = "battery-low";
const LOW_PERCENT: f64 = 20.0;
const CRITICAL_PERCENT: f64 = 5.0;

#[zbus::proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait Device {
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn is_present(&self) -> zbus::Result<bool>;
}

/// UPower device states we care about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Charging,
    Discharging,
    Full,
    Other,
}

impl From<u32> for State {
    fn from(v: u32) -> Self {
        match v {
            1 | 5 => State::Charging,    // charging, pending charge
            2 | 6 => State::Discharging, // discharging, pending discharge
            4 => State::Full,
            _ => State::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Ok,
    Low,
    Critical,
}

pub async fn run(emitter: Emitter) -> Result<()> {
    let conn = Connection::system().await?;
    let device = DeviceProxy::new(&conn).await?;
    if !device.is_present().await? {
        bail!("no battery");
    }
    let mut state_changes = device.receive_state_changed().await;
    let mut percent_changes = device.receive_percentage_changed().await;

    let mut state = State::from(device.state().await?);
    let mut level = Level::Ok;
    let mut shown: Option<BatteryView> = None;
    loop {
        let percent = device.percentage().await?;
        let new_state = State::from(device.state().await?);
        let view = battery_view(new_state, percent);
        if shown != Some(view) {
            emitter.emit(Event::Battery(view));
            shown = Some(view);
        }
        if new_state != state {
            if let Some(title) = charger_message(state, new_state) {
                let icon = if new_state == State::Discharging {
                    "battery"
                } else {
                    "battery-charging"
                };
                emitter.push(ActivitySpec {
                    progress: Some((percent / 100.0) as f32),
                    ..emitter.status(CHARGER_ID, icon, title)
                });
            }
            state = new_state;
        }

        let new_level = battery_level(state, percent);
        if new_level != level {
            level = new_level;
            match low_battery(level, percent) {
                Some(spec) => emitter.push(spec),
                None => emitter.dismiss(LOW_ID),
            }
        }

        tokio::select! {
            Some(_) = state_changes.next() => {}
            Some(_) = percent_changes.next() => {}
            else => bail!("UPower stopped"),
        }
    }
}

/// What the right island's battery indicator shows.
fn battery_view(state: State, percent: f64) -> BatteryView {
    BatteryView {
        percent: percent.round().clamp(0.0, 100.0) as u8,
        charging: matches!(state, State::Charging | State::Full),
        level: match battery_level(state, percent) {
            Level::Ok => BatteryLevel::Normal,
            Level::Low => BatteryLevel::Low,
            Level::Critical => BatteryLevel::Critical,
        },
    }
}

fn charger_message(old: State, new: State) -> Option<&'static str> {
    match (old, new) {
        (_, State::Charging) => Some("Charging"),
        (State::Charging | State::Full, State::Discharging) => Some("On battery"),
        (State::Charging, State::Full) => Some("Fully charged"),
        _ => None,
    }
}

fn battery_level(state: State, percent: f64) -> Level {
    match state {
        State::Discharging if percent <= CRITICAL_PERCENT => Level::Critical,
        State::Discharging if percent <= LOW_PERCENT => Level::Low,
        _ => Level::Ok,
    }
}

/// Low: passes through the center like a notification. Critical: stays in the center.
fn low_battery(level: Level, percent: f64) -> Option<ActivitySpec> {
    let (title, placement, priority) = match level {
        Level::Ok => return None,
        Level::Low => ("Battery low", Placement::Flow, Priority::High),
        Level::Critical => ("Battery critical", Placement::Center, Priority::Critical),
    };
    Some(ActivitySpec {
        progress: Some((percent / 100.0) as f32),
        icon: Some("battery-low".into()),
        placement,
        priority,
        ..ActivitySpec::new(LOW_ID, title)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charger_transitions() {
        use State::*;
        assert_eq!(charger_message(Discharging, Charging), Some("Charging"));
        assert_eq!(charger_message(Charging, Discharging), Some("On battery"));
        assert_eq!(charger_message(Charging, Full), Some("Fully charged"));
        assert_eq!(charger_message(Other, Discharging), None);
    }

    #[test]
    fn indicator_view() {
        let v = battery_view(State::Discharging, 4.4);
        assert_eq!(
            (v.percent, v.charging, v.level),
            (4, false, BatteryLevel::Critical)
        );
        let v = battery_view(State::Full, 100.0);
        assert_eq!(
            (v.percent, v.charging, v.level),
            (100, true, BatteryLevel::Normal)
        );
    }

    #[test]
    fn levels_only_while_discharging() {
        assert_eq!(battery_level(State::Discharging, 20.0), Level::Low);
        assert_eq!(battery_level(State::Discharging, 4.0), Level::Critical);
        assert_eq!(battery_level(State::Charging, 4.0), Level::Ok);
        assert_eq!(battery_level(State::Discharging, 50.0), Level::Ok);
    }
}
