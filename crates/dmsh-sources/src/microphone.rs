//! Microphone indicator. Listens to `pactl subscribe` (PipeWire via pipewire-pulse) and,
//! when recording streams or sources change, lists who records from a real source — not
//! a speaker's monitor (screen recorders capturing desktop audio) and not a volume meter.

use std::process::Stdio;

use anyhow::{Context, Result};
use dmsh_core::Sensor;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::{Emitter, SETTLE};

pub async fn run(emitter: Emitter) -> Result<()> {
    let mut apps = recording().await?;
    if apps.is_some() {
        emitter.capture(Sensor::Mic, true, apps.as_deref().unwrap_or_default());
    }
    let mut child = Command::new("pactl")
        .arg("subscribe")
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting `pactl subscribe`")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let mut lines = BufReader::new(stdout).lines();

    while let Some(line) = lines.next_line().await? {
        // "Event 'new' on source-output #151", or a source coming or going.
        if !(line.contains(" on source-output #") || line.contains(" on source #")) {
            continue;
        }
        while let Ok(Ok(Some(_))) = tokio::time::timeout(SETTLE, lines.next_line()).await {}
        let now = match recording().await {
            Ok(now) => now,
            Err(e) => {
                log::debug!("listing recording streams: {e:#}");
                continue;
            }
        };
        // Announce when recording starts, and again when another app joins in.
        let joined = match (&apps, &now) {
            (Some(old), Some(new)) => new.iter().any(|a| !old.contains(a)),
            (None, Some(_)) => true,
            _ => false,
        };
        if joined {
            emitter.capture(Sensor::Mic, true, now.as_deref().unwrap_or_default());
        } else if apps.is_some() && now.is_none() {
            emitter.capture(Sensor::Mic, false, &[]);
        }
        apps = now;
    }
    anyhow::bail!("`pactl subscribe` exited")
}

/// Apps recording from a microphone, or `None` when nothing is.
async fn recording() -> Result<Option<Vec<String>>> {
    let outputs = pactl_json(&["list", "source-outputs"]).await?;
    let sources = pactl_json(&["list", "short", "sources"]).await?;
    Ok(recorders(&outputs, &sources))
}

async fn pactl_json(args: &[&str]) -> Result<Value> {
    let out = Command::new("pactl")
        .args(["-f", "json"])
        .args(args)
        .output()
        .await
        .context("running pactl")?;
    anyhow::ensure!(out.status.success(), "pactl {} failed", args.join(" "));
    serde_json::from_slice(&out.stdout).context("parsing pactl's JSON")
}

fn recorders(outputs: &Value, sources: &Value) -> Option<Vec<String>> {
    let monitors: Vec<u64> = sources
        .as_array()?
        .iter()
        .filter(|s| s["name"].as_str().is_some_and(|n| n.ends_with(".monitor")))
        .filter_map(|s| s["index"].as_u64())
        .collect();
    let mut apps = Vec::new();
    let mut any = false;
    for output in outputs.as_array()? {
        let props = &output["properties"];
        let monitor = output["source"]
            .as_u64()
            .is_some_and(|i| monitors.contains(&i));
        // pavucontrol's level meters record too.
        let meter = props["media.name"].as_str() == Some("Peak detect");
        if output["corked"].as_bool() == Some(true) || monitor || meter {
            continue;
        }
        any = true;
        let name = props["application.name"]
            .as_str()
            .or(props["application.process.binary"].as_str());
        if let Some(name) = name.filter(|n| !n.is_empty())
            && !apps.iter().any(|a| a == name)
        {
            apps.push(name.to_owned());
        }
    }
    apps.sort();
    any.then_some(apps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counts_microphones_not_monitors_meters_or_paused_streams() {
        let sources = json!([
            { "index": 58, "name": "alsa_output.speaker.monitor" },
            { "index": 60, "name": "alsa_input.mic" },
        ]);
        let stream = |source: u64, app: &str, media: &str, corked: bool| {
            json!({
                "source": source,
                "corked": corked,
                "properties": { "application.name": app, "media.name": media },
            })
        };
        let none = json!([
            stream(58, "obs", "Desktop audio", false),
            stream(60, "pavucontrol", "Peak detect", false),
            stream(60, "vesktop", "RecordStream", true),
        ]);
        assert_eq!(recorders(&none, &sources), None);
        let some = json!([
            stream(60, "vesktop", "RecordStream", false),
            stream(60, "vesktop", "RecordStream", false),
            stream(60, "Firefox", "AudioStream", false),
            stream(58, "obs", "Desktop audio", false),
        ]);
        assert_eq!(
            recorders(&some, &sources),
            Some(vec!["Firefox".into(), "vesktop".into()])
        );
        assert_eq!(recorders(&json!([]), &sources), None);
    }
}
