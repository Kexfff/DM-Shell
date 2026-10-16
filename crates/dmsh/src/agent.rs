//! Agent integration that isn't the socket protocol itself: Claude Code hooks
//! (`dmsh hook <event>`), their setup (`dmsh claude-code`), and asking the user a
//! question through the island and waiting for the answer.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use dmsh_core::ipc::{Event, Removal, Reply, Request, Response};
use dmsh_core::{Action, ActivitySpec, Placement, Priority};
use serde_json::{Value, json};

use crate::ipc;

/// What happened to a question.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    /// The key of the clicked action.
    Chose(String),
    Dismissed,
    TimedOut,
}

/// Pushes `spec` (which should have actions and a `ttl_ms`) and waits for the answer.
pub fn ask(spec: ActivitySpec) -> Result<Answer> {
    // Subscribe first so even an instant click isn't missed.
    let events = ipc::subscribe()?;
    let id = spec.id.clone();
    match ipc::request(&Request::Push(spec))? {
        Response::Ok(_) => {}
        Response::Error(e) => bail!("{e}"),
    }
    for event in events {
        match event? {
            Event::ActionInvoked { id: e, key } if e == id => return Ok(Answer::Chose(key)),
            Event::Removed { id: e, reason } if e == id => {
                return Ok(match reason {
                    Removal::Expired => Answer::TimedOut,
                    Removal::Dismissed => Answer::Dismissed,
                });
            }
            _ => {}
        }
    }
    bail!("the shell went away")
}

fn push(spec: ActivitySpec) -> Result<()> {
    match ipc::request(&Request::Push(spec))? {
        Response::Ok(_) => Ok(()),
        Response::Error(e) => bail!("{e}"),
    }
}

fn dismiss(id: &str) -> Result<()> {
    match ipc::request(&Request::Dismiss { id: id.into() })? {
        Response::Ok(Reply::Dismissed { .. } | Reply::Done) => Ok(()),
        Response::Ok(_) => Ok(()),
        Response::Error(e) => bail!("{e}"),
    }
}

// --- Claude Code hooks ----------------------------------------------------------------

/// The hook events dmsh handles, as `dmsh hook <event>` arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hook {
    /// UserPromptSubmit, PostToolUse: Claude is working.
    Working,
    /// Notification: Claude needs input (permission, a question).
    Notification,
    /// PermissionRequest: ask Allow / Deny in the island.
    Permission,
    /// Stop: Claude finished its turn.
    Stop,
    /// SessionEnd: remove the session's activities.
    End,
}

/// How long the island waits for a permission answer before falling back to Claude
/// Code's own prompt.
const PERMISSION_WAIT: Duration = Duration::from_secs(60);

/// Runs a hook. Never fails the hook: without dmsh running, or on any error, it does
/// nothing (and for permissions, leaves Claude Code's own prompt to ask).
pub fn hook(event: Hook) -> Result<()> {
    let mut input = String::new();
    std::io::stdin()
        .take(1 << 20)
        .read_to_string(&mut input)
        .ok();
    let input: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    match run_hook(event, &input) {
        Ok(Some(output)) => println!("{output}"),
        Ok(None) => {}
        Err(e) => log::debug!("hook {event:?}: {e:#}"),
    }
    Ok(())
}

fn run_hook(event: Hook, input: &Value) -> Result<Option<Value>> {
    let session = input["session_id"].as_str().unwrap_or("session");
    let id = format!("claude:{}", &session[..session.len().min(8)]);
    let project = input["cwd"]
        .as_str()
        .and_then(|cwd| Path::new(cwd).file_name())
        .map(|n| n.to_string_lossy().into_owned());
    let status = |title: &str, icon: &str| ActivitySpec {
        body: project.clone(),
        icon: Some(icon.into()),
        app: Some("Claude Code".into()),
        ..ActivitySpec::new(id.clone(), title)
    };

    let done = format!("{id}:done");
    match event {
        Hook::Working => {
            // A new turn: the previous "Done" is no longer news.
            dismiss(&done)?;
            push(ActivitySpec {
                placement: Placement::Center,
                priority: Priority::Low,
                ..status("Working", "terminal")
            })?
        }
        Hook::Notification => {
            let kind = input["notification_type"].as_str().unwrap_or("");
            let message = input["message"]
                .as_str()
                .unwrap_or("Claude needs your input");
            match kind {
                // The Stop hook already said it's done.
                "idle_prompt" => {}
                "permission_prompt" | "agent_needs_input" | "elicitation_dialog" | "" => {
                    push(ActivitySpec {
                        placement: Placement::Center,
                        priority: Priority::High,
                        ..status(message, "alert")
                    })?
                }
                _ => push(ActivitySpec {
                    placement: Placement::Flow,
                    ttl_ms: Some(10 * 60_000),
                    ..status(message, "bell")
                })?,
            }
        }
        Hook::Stop => {
            // A separate activity, so it pops up in the center (an update to the working
            // status wouldn't), then waits on the right island.
            dismiss(&id)?;
            push(ActivitySpec {
                id: done,
                placement: Placement::Flow,
                ttl_ms: Some(30 * 60_000),
                ..status("Done", "check")
            })?
        }
        Hook::End => {
            for suffix in ["", ":permission", ":done"] {
                dismiss(&format!("{id}{suffix}"))?;
            }
        }
        Hook::Permission => return permission(input, &id, project),
    }
    Ok(None)
}

/// Asks Allow / Deny / In terminal in the island; returns Claude Code's decision object,
/// or `None` to let its own prompt ask.
fn permission(input: &Value, id: &str, project: Option<String>) -> Result<Option<Value>> {
    let tool = input["tool_name"].as_str().unwrap_or("a tool");
    let tool_input = &input["tool_input"];
    let detail = ["command", "file_path", "url", "pattern", "path"]
        .iter()
        .find_map(|k| tool_input[*k].as_str().map(str::to_owned))
        .unwrap_or_else(|| tool_input.to_string());
    let app = match &project {
        Some(p) => format!("Claude Code · {p}"),
        None => "Claude Code".into(),
    };
    let spec = ActivitySpec {
        body: Some(truncate(&detail, 300)),
        icon: Some("alert".into()),
        app: Some(app),
        placement: Placement::Center,
        priority: Priority::Critical,
        ttl_ms: Some(PERMISSION_WAIT.as_millis() as u64),
        actions: vec![
            Action::new("allow", "Allow"),
            Action::new("deny", "Deny"),
            Action::new("terminal", "In terminal"),
        ],
        ..ActivitySpec::new(format!("{id}:permission"), format!("Allow {tool}?"))
    };
    let decision = match ask(spec)? {
        Answer::Chose(key) if key == "allow" => json!({ "behavior": "allow" }),
        Answer::Chose(key) if key == "deny" => json!({
            "behavior": "deny",
            "message": "The user denied this from the dmsh island.",
        }),
        _ => return Ok(None),
    };
    Ok(Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": decision,
        }
    })))
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

// --- Setup ----------------------------------------------------------------------------

/// `(Claude Code hook event, dmsh hook argument)`.
const HOOKS: &[(&str, &str)] = &[
    ("UserPromptSubmit", "working"),
    ("PostToolUse", "working"),
    ("Notification", "notification"),
    ("PermissionRequest", "permission"),
    ("Stop", "stop"),
    ("SessionEnd", "end"),
];

/// The `hooks` object for Claude Code's settings, calling this binary.
pub fn hooks_config(exe: &Path) -> Value {
    let mut hooks = serde_json::Map::new();
    for (event, arg) in HOOKS {
        let command = format!("'{}' hook {arg}", exe.display());
        let mut hook = json!({ "type": "command", "command": command });
        if *event == "PermissionRequest" {
            // Waits for an answer in the island (falls back to the terminal after a minute).
            hook["timeout"] = json!(PERMISSION_WAIT.as_secs() + 30);
        }
        hooks.insert(event.to_string(), json!([{ "hooks": [hook] }]));
    }
    Value::Object(hooks)
}

fn is_dmsh_group(group: &Value) -> bool {
    group["hooks"].as_array().is_some_and(|hooks| {
        hooks.iter().any(|h| {
            h["command"]
                .as_str()
                .is_some_and(|c| c.contains("dmsh") && c.contains(" hook "))
        })
    })
}

/// Adds (or, with `remove`, removes) dmsh's hooks in a Claude Code settings file,
/// keeping everything else. Writes a backup next to it first.
pub fn apply(settings: &Path, exe: &Path, remove: bool) -> Result<()> {
    let mut root: Value = match std::fs::read_to_string(settings) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} isn't valid JSON", settings.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e).context(format!("reading {}", settings.display())),
    };
    if settings.exists() {
        let backup = settings.with_extension("json.dmsh-backup");
        std::fs::copy(settings, &backup)
            .with_context(|| format!("backing up to {}", backup.display()))?;
    }
    let object = root
        .as_object_mut()
        .context("settings must be a JSON object")?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("\"hooks\" must be an object")?;
    for (event, groups) in hooks_config(exe).as_object().expect("object") {
        let list = hooks
            .entry(event.clone())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("hooks.{event} must be an array"))?;
        list.retain(|g| !is_dmsh_group(g));
        if !remove {
            list.extend(groups.as_array().expect("array").iter().cloned());
        }
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    if let Some(parent) = settings.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(settings, serde_json::to_string_pretty(&root)? + "\n")
        .with_context(|| format!("writing {}", settings.display()))?;
    Ok(())
}

pub fn claude_settings_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("settings.json"))
}

/// Printed by `dmsh claude-code`.
pub fn setup_text(exe: &Path) -> Result<String> {
    let hooks = serde_json::to_string_pretty(&json!({ "hooks": hooks_config(exe) }))?;
    Ok(format!(
        r#"# dmsh × Claude Code

## Hooks: session status and permission prompts in the island

Merge this into ~/.claude/settings.json (or run `dmsh claude-code --apply`):

{hooks}

- UserPromptSubmit / PostToolUse: "Working" in the center (low priority)
- Notification: "needs your input" in the center
- PermissionRequest: Allow / Deny / In terminal in the island; no answer within a
  minute (or dmsh not running) falls back to Claude Code's own prompt
- Stop: "Done", then it moves to the right island
- SessionEnd: removes the session's activities

## MCP server: tools for showing status, notifying and asking you

claude mcp add --transport stdio dmsh --scope user -- '{exe}' mcp
"#,
        exe = exe.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from("/usr/bin/dmsh")
    }

    #[test]
    fn hooks_config_covers_every_event() {
        let config = hooks_config(&exe());
        for (event, arg) in HOOKS {
            let command = config[event][0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(command, format!("'/usr/bin/dmsh' hook {arg}"));
        }
        assert!(config["PermissionRequest"][0]["hooks"][0]["timeout"].is_u64());
    }

    #[test]
    fn apply_merges_keeps_other_settings_and_removes_cleanly() {
        let dir = std::env::temp_dir().join(format!("dmsh-apply-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let original = json!({
            "model": "opus",
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "notify-me" }] }]
            }
        });
        std::fs::write(&path, original.to_string()).unwrap();

        apply(&path, &exe(), false).unwrap();
        apply(&path, &exe(), false).unwrap(); // idempotent
        let merged: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(merged["model"], "opus");
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "the user's hook plus one dmsh hook");
        assert!(merged["hooks"]["PermissionRequest"].is_array());

        apply(&path, &exe(), true).unwrap();
        let removed: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(removed, original);
        assert!(dir.join("settings.json.dmsh-backup").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn truncates_by_characters() {
        assert_eq!(truncate("héllo", 3), "hél…");
        assert_eq!(truncate("hi", 3), "hi");
    }
}
