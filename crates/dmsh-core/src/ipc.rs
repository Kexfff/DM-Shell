//! IPC protocol: newline-delimited JSON over a Unix socket.
//!
//! Each request is one JSON object on one line; the server answers each with one line.
//! Example: `{"cmd":"push","id":"build","title":"cargo build","progress":0.4}`
//! → `{"ok":{"type":"done"}}`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::activity::{ActivitySpec, ActivityState, Island, View};

/// Environment variable that overrides the socket path.
pub const SOCKET_ENV: &str = "DMSH_SOCKET";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Create or update an activity.
    Push(ActivitySpec),
    /// Remove an activity by id.
    Dismiss {
        id: String,
    },
    /// Remove all activities, or only the right island's.
    Clear {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        island: Option<Island>,
    },
    /// Invoke an activity's action, as if its button was clicked. Dismisses the activity
    /// unless it has `keep_on_action`.
    Invoke {
        id: String,
        key: String,
    },
    /// What the islands show now, plus every live activity.
    State,
    Version,
    /// Re-read the config file; replies with an error if it is invalid.
    Reload,
    /// Keep this connection open and stream [`Event`]s, one JSON object per line.
    Subscribe,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Ok(Reply),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Done,
    /// Returned by `subscribe`; events follow on the same connection.
    Subscribed,
    /// Returned by `dismiss`: whether the id existed.
    Dismissed {
        existed: bool,
    },
    State(Box<StateDump>),
    Version {
        version: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StateDump {
    /// The view on the focused output.
    pub view: View,
    /// Connector name of the focused output (from niri), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_output: Option<String>,
    pub activities: Vec<ActivityState>,
}

/// Why an activity went away.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Removal {
    Expired,
    Dismissed,
}

/// Streamed to subscribers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    ActionInvoked { id: String, key: String },
    Removed { id: String, reason: Removal },
}

impl Event {
    pub fn id(&self) -> &str {
        match self {
            Event::ActionInvoked { id, .. } | Event::Removed { id, .. } => id,
        }
    }
}

/// All message types, for publishing one JSON Schema document.
#[derive(JsonSchema)]
#[allow(dead_code)]
struct Protocol {
    request: Request,
    response: Response,
    event: Event,
}

/// JSON Schema covering every request and response.
pub fn schema() -> schemars::Schema {
    schemars::schema_for!(Protocol)
}

/// `$DMSH_SOCKET`, else `$XDG_RUNTIME_DIR/dmsh-$WAYLAND_DISPLAY.sock`, so each Wayland
/// session gets its own shell.
pub fn socket_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(SOCKET_ENV) {
        return Some(path.into());
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    // WAYLAND_DISPLAY may be an absolute path; keep only its file name.
    let display = display.rsplit('/').next().unwrap_or(&display).to_owned();
    Some(PathBuf::from(runtime).join(format!("dmsh-{display}.sock")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_format() {
        let r: Request =
            serde_json::from_str(r#"{"cmd":"push","id":"b","title":"t","progress":0.5}"#).unwrap();
        let Request::Push(spec) = r else {
            panic!("expected push")
        };
        assert_eq!(spec.progress, Some(0.5));

        let r: Request = serde_json::from_str(r#"{"cmd":"dismiss","id":"b"}"#).unwrap();
        assert_eq!(r, Request::Dismiss { id: "b".into() });

        let r: Request = serde_json::from_str(r#"{"cmd":"clear"}"#).unwrap();
        assert_eq!(r, Request::Clear { island: None });
        let r: Request = serde_json::from_str(r#"{"cmd":"clear","island":"right"}"#).unwrap();
        assert_eq!(
            r,
            Request::Clear {
                island: Some(Island::Right)
            }
        );
    }

    #[test]
    fn response_wire_format() {
        let ok = serde_json::to_string(&Response::Ok(Reply::Done)).unwrap();
        assert_eq!(ok, r#"{"ok":{"type":"done"}}"#);
        let err = serde_json::to_string(&Response::Error("nope".into())).unwrap();
        assert_eq!(err, r#"{"error":"nope"}"#);
        let event = Event::ActionInvoked {
            id: "perm".into(),
            key: "allow".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"event":"action_invoked","id":"perm","key":"allow"}"#
        );
    }
}
