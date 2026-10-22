//! The current temperature, from wttr.in through `curl`, every `weather.interval`. While
//! it can't be fetched (offline, wttr.in down, an unknown place) the last reading stays
//! for a while, then nothing is shown.

use std::time::Duration;

use anyhow::{Context, Result};
use dmsh_core::config::Units;
use tokio::process::Command;
use tokio::time::Instant;

use crate::{Emitter, Event};

/// How soon to try again after a failure (e.g. no network yet right after login).
const RETRY: Duration = Duration::from_secs(2 * 60);
/// A reading this old is no longer the current temperature.
const STALE: Duration = Duration::from_secs(60 * 60);

pub async fn run(emitter: Emitter) -> Result<()> {
    let mut shown: Option<String> = None;
    let mut fetched_at: Option<Instant> = None;
    loop {
        let (url, interval) = {
            let config = emitter.config();
            let w = &config.weather;
            (url(w.location.as_deref(), w.units), w.interval)
        };
        let wait = match fetch(&url).await {
            Ok(temperature) => {
                fetched_at = Some(Instant::now());
                if shown.as_ref() != Some(&temperature) {
                    emitter.emit(Event::Weather(Some(temperature.clone())));
                    shown = Some(temperature);
                }
                interval
            }
            Err(e) => {
                // Without curl there's nothing to retry.
                if e.chain().any(|e| {
                    e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                }) {
                    return Err(e);
                }
                log::debug!("weather: {e:#}");
                let stale = fetched_at.is_none_or(|t| t.elapsed() > STALE);
                if stale && shown.take().is_some() {
                    emitter.emit(Event::Weather(None));
                }
                RETRY.min(interval)
            }
        };
        // A changed location or units applies right away.
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = emitter.config_changed() => {}
        }
    }
}

fn url(location: Option<&str>, units: Option<Units>) -> String {
    let place = location.map(encode).unwrap_or_default();
    let units = match units {
        Some(Units::Celsius) => "&m",
        Some(Units::Fahrenheit) => "&u",
        None => "",
    };
    format!("https://wttr.in/{place}?format=%t{units}")
}

/// Percent-encodes a place name for the URL path.
fn encode(place: &str) -> String {
    let mut out = String::new();
    for byte in place.trim().bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b',' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

async fn fetch(url: &str) -> Result<String> {
    let out = Command::new("curl")
        .args(["-fsS", "--max-time", "15", url])
        .output()
        .await
        .context("running curl")?;
    anyhow::ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let text = String::from_utf8_lossy(&out.stdout);
    parse(&text).with_context(|| format!("unexpected answer from wttr.in: {text:?}"))
}

/// `+11°C` → `11°`, `-3°F` → `-3°`; anything else (an error page) → `None`.
fn parse(text: &str) -> Option<String> {
    let number = text
        .trim()
        .strip_suffix("°C")
        .or_else(|| text.trim().strip_suffix("°F"))?;
    let degrees: i32 = number.strip_prefix('+').unwrap_or(number).parse().ok()?;
    Some(format!("{degrees}°"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wttr_temperatures() {
        assert_eq!(parse("+11°C").as_deref(), Some("11°"));
        assert_eq!(parse("-3°F\n").as_deref(), Some("-3°"));
        assert_eq!(parse("-0°C").as_deref(), Some("0°"));
        assert_eq!(parse("Unknown location; please try ~55.7,37.6"), None);
        assert_eq!(parse("<html>"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn builds_urls() {
        assert_eq!(url(None, None), "https://wttr.in/?format=%t");
        assert_eq!(
            url(Some("Saint Petersburg"), Some(Units::Celsius)),
            "https://wttr.in/Saint%20Petersburg?format=%t&m"
        );
        assert_eq!(
            url(Some("~Eiffel Tower"), Some(Units::Fahrenheit)),
            "https://wttr.in/~Eiffel%20Tower?format=%t&u"
        );
        assert_eq!(
            url(Some("Москва"), None),
            "https://wttr.in/%D0%9C%D0%BE%D1%81%D0%BA%D0%B2%D0%B0?format=%t"
        );
    }
}
