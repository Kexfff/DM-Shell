//! `dmsh mcp`: a Model Context Protocol server over stdio (newline-delimited JSON-RPC
//! 2.0), giving agents tools to show status, notify and ask the user in the islands.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use dmsh_core::ipc::{Request, Response};
use dmsh_core::{Action, ActivitySpec, Placement, Priority};
use serde_json::{Value, json};

use crate::agent::{Answer, ask};
use crate::ipc;

/// Used when the client doesn't say which protocol version it speaks.
const PROTOCOL_VERSION: &str = "2025-06-18";

const INSTRUCTIONS: &str = "dmsh shows things on the user's screen in two 'dynamic \
islands' at the top. Use show_status for long-running work (progress), notify when \
something finishes or needs attention, and ask_user to get a decision from the user \
when they may not be watching the terminal. Clear statuses you no longer need.";

pub fn serve() -> Result<()> {
    let mut server = Server::default();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => server.handle(&message),
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[derive(Default)]
struct Server {
    /// Shown as the activities' `app`, e.g. "claude-code".
    client: Option<String>,
    counter: AtomicU64,
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn text(content: impl Into<String>, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": content.into() }], "isError": is_error })
}

impl Server {
    /// Handles one message; `None` for notifications (no reply).
    fn handle(&mut self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned()?;
        let method = message["method"].as_str().unwrap_or("");
        let params = &message["params"];
        Some(match method {
            "initialize" => {
                self.client = params["clientInfo"]["name"].as_str().map(str::to_owned);
                let version = params["protocolVersion"]
                    .as_str()
                    .unwrap_or(PROTOCOL_VERSION);
                result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "dmsh", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => result(id, json!({})),
            "tools/list" => result(id, json!({ "tools": tools() })),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                let args = &params["arguments"];
                let outcome = self.call(name, args);
                result(
                    id,
                    match outcome {
                        Ok(message) => text(message, false),
                        Err(e) => text(format!("{e:#}"), true),
                    },
                )
            }
            _ => error(id, -32601, &format!("unknown method {method}")),
        })
    }

    fn app(&self) -> String {
        self.client.clone().unwrap_or_else(|| "Agent".into())
    }

    fn call(&self, name: &str, args: &Value) -> Result<String> {
        let str_arg = |key: &str| args[key].as_str().map(str::to_owned);
        let required = |key: &str| {
            str_arg(key)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("`{key}` is required"))
        };
        match name {
            "show_status" => {
                let id = format!("mcp:{}", required("id")?);
                let progress = args["progress"].as_f64().map(|p| p as f32);
                push(ActivitySpec {
                    body: str_arg("details"),
                    progress,
                    icon: Some(str_arg("icon").unwrap_or_else(|| "terminal".into())),
                    app: Some(self.app()),
                    placement: Placement::Center,
                    ..ActivitySpec::new(id, required("title")?)
                })?;
                Ok(
                    "Shown in the center island. Call show_status again with the same id to \
                    update it, and clear_status when done."
                        .into(),
                )
            }
            "clear_status" => {
                let id = format!("mcp:{}", required("id")?);
                ipc::request(&Request::Dismiss { id })?;
                Ok("Cleared.".into())
            }
            "notify" => {
                let n = self.counter.fetch_add(1, Ordering::Relaxed);
                let urgent = args["urgent"].as_bool().unwrap_or(false);
                push(ActivitySpec {
                    body: str_arg("details"),
                    icon: Some(str_arg("icon").unwrap_or_else(|| "bell".into())),
                    app: Some(self.app()),
                    placement: Placement::Flow,
                    priority: if urgent {
                        Priority::High
                    } else {
                        Priority::Normal
                    },
                    ..ActivitySpec::new(
                        format!("mcp:notify:{}:{n}", std::process::id()),
                        required("title")?,
                    )
                })?;
                Ok("Notified.".into())
            }
            "ask_user" => {
                let question = required("question")?;
                let options: Vec<String> = args["options"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|o| o.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                if !(2..=4).contains(&options.len()) {
                    anyhow::bail!("`options` needs 2 to 4 choices");
                }
                let timeout = args["timeout_seconds"]
                    .as_u64()
                    .unwrap_or(300)
                    .clamp(10, 3600);
                let n = self.counter.fetch_add(1, Ordering::Relaxed);
                let spec = ActivitySpec {
                    body: str_arg("details"),
                    icon: Some("alert".into()),
                    app: Some(self.app()),
                    placement: Placement::Center,
                    priority: Priority::High,
                    ttl_ms: Some(timeout * 1000),
                    actions: options
                        .iter()
                        .enumerate()
                        .map(|(i, label)| Action::new(i.to_string(), label.clone()))
                        .collect(),
                    ..ActivitySpec::new(format!("mcp:ask:{}:{n}", std::process::id()), question)
                };
                Ok(match ask(spec)? {
                    Answer::Chose(key) => {
                        let choice = key
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| options.get(i))
                            .cloned()
                            .unwrap_or(key);
                        format!("The user chose: {choice}")
                    }
                    Answer::Dismissed => {
                        "The user dismissed the question without answering.".into()
                    }
                    Answer::TimedOut => format!("No answer within {timeout} seconds."),
                })
            }
            _ => anyhow::bail!("unknown tool {name}"),
        }
    }
}

fn push(spec: ActivitySpec) -> Result<()> {
    match ipc::request(&Request::Push(spec))? {
        Response::Ok(_) => Ok(()),
        Response::Error(e) => anyhow::bail!("{e}"),
    }
}

fn tools() -> Value {
    json!([
        {
            "name": "show_status",
            "description": "Show (or update) a live status in the user's center island, e.g. progress of a long task. Same id = update. Remove it with clear_status when done.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Stable id for this status, e.g. 'build'." },
                    "title": { "type": "string", "description": "Short text, e.g. 'Running tests'." },
                    "details": { "type": "string", "description": "Longer text, shown when the user hovers." },
                    "progress": { "type": "number", "minimum": 0, "maximum": 1, "description": "0.0 to 1.0, shown as a bar." },
                    "icon": { "type": "string", "description": "Built-in icon: terminal, download, check, alert, bell, message, music, globe." }
                },
                "required": ["id", "title"]
            }
        },
        {
            "name": "clear_status",
            "description": "Remove a status shown with show_status.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        },
        {
            "name": "notify",
            "description": "Send the user a notification: it pops up briefly, then waits in the right island until read.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "details": { "type": "string" },
                    "urgent": { "type": "boolean", "description": "Higher priority (still not critical)." },
                    "icon": { "type": "string" }
                },
                "required": ["title"]
            }
        },
        {
            "name": "ask_user",
            "description": "Ask the user a question in the island with 2-4 buttons and wait for the answer. Useful when the user may not be watching the terminal. Blocks until they click, dismiss, or the timeout passes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "details": { "type": "string" },
                    "options": { "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 4 },
                    "timeout_seconds": { "type": "integer", "minimum": 10, "maximum": 3600, "description": "Default 300." }
                },
                "required": ["question", "options"]
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_and_list_tools() {
        let mut server = Server::default();
        let init = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "clientInfo": { "name": "claude-code" } }
        });
        let reply = server.handle(&init).unwrap();
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(reply["result"]["serverInfo"]["name"], "dmsh");
        assert_eq!(server.app(), "claude-code");

        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(server.handle(&note).is_none());

        let list = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
            .unwrap();
        let names: Vec<_> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["show_status", "clear_status", "notify", "ask_user"]);
    }

    #[test]
    fn bad_calls_are_tool_errors_not_crashes() {
        let server = Server::default();
        let err = server.call(
            "ask_user",
            &json!({ "question": "?", "options": ["only one"] }),
        );
        assert!(err.unwrap_err().to_string().contains("2 to 4"));
        assert!(server.call("nope", &json!({})).is_err());
        assert!(
            server
                .call("show_status", &json!({ "title": "no id" }))
                .is_err()
        );
    }

    #[test]
    fn unknown_methods_get_errors() {
        let mut server = Server::default();
        let reply = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 7, "method": "resources/list" }))
            .unwrap();
        assert_eq!(reply["error"]["code"], -32601);
    }
}
