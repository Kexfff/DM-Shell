//! Idle clock: one update at every minute boundary, nothing in between.

use std::time::Duration;

use chrono::{Local, Timelike};

use crate::{Emitter, Event};

pub async fn run(emitter: Emitter) -> anyhow::Result<()> {
    loop {
        let now = Local::now();
        emitter.emit(Event::Clock {
            time: now.format("%H:%M").to_string(),
            date: now.format("%A, %-d %B").to_string(),
        });

        let into_minute = Duration::from_secs(now.second().into())
            + Duration::from_nanos((now.nanosecond() % 1_000_000_000).into());
        // A little past the boundary so we never format the previous minute again.
        let wait = Duration::from_secs(60).saturating_sub(into_minute) + Duration::from_millis(20);
        tokio::time::sleep(wait).await;
    }
}
