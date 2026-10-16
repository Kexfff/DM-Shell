//! Idle clock: one update at every minute boundary (and when the config changes),
//! nothing in between.

use std::time::Duration;

use chrono::{Local, Timelike};
use futures_util::StreamExt;

use crate::{Emitter, Event};

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

pub async fn run(emitter: Emitter) -> anyhow::Result<()> {
    // The minute timer runs on a monotonic clock, which stops during suspend; logind
    // tells us when the system wakes so the clock is right immediately.
    let mut wakes = match wake_signals().await {
        Ok(stream) => Some(stream),
        Err(e) => {
            log::debug!("no logind sleep signals: {e}");
            None
        }
    };
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
        let woke = async {
            match &mut wakes {
                Some(stream) => loop {
                    match stream.next().await {
                        Some(s) if s.args().is_ok_and(|a| !a.start) => return,
                        Some(_) => {}
                        None => std::future::pending::<()>().await,
                    }
                },
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = emitter.config_changed() => {}
            _ = woke => {}
        }
    }
}

async fn wake_signals() -> zbus::Result<PrepareForSleepStream> {
    let conn = zbus::Connection::system().await?;
    LoginProxy::new(&conn)
        .await?
        .receive_prepare_for_sleep()
        .await
}
