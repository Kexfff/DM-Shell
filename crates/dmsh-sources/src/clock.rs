//! Idle clock: one update at every minute boundary (and when the config changes),
//! nothing in between.

use std::time::Duration;

use chrono::{Local, Timelike};

use crate::{Emitter, Event};

pub async fn run(emitter: Emitter) -> anyhow::Result<()> {
    loop {
        let now = Local::now();
        let (time, date) = {
            let config = emitter.config();
            // Formats were validated when the config was parsed.
            (
                now.format(&config.clock.time).to_string(),
                now.format(&config.clock.date).to_string(),
            )
        };
        emitter.emit(Event::Clock { time, date });

        let into_minute = Duration::from_secs(now.second().into())
            + Duration::from_nanos((now.nanosecond() % 1_000_000_000).into());
        // A little past the boundary so we never format the previous minute again.
        let wait = Duration::from_secs(60).saturating_sub(into_minute) + Duration::from_millis(20);
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = emitter.config_changed() => {}
        }
    }
}
