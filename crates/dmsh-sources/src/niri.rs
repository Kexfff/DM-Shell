//! niri event stream: focused output, keyboard layout OSD and screencast indicator.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use dmsh_core::{ActivitySpec, Placement, Priority};
use niri_ipc::{Event as NiriEvent, Request, socket::SOCKET_PATH_ENV};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::{Emitter, Event, osd};

const LAYOUT_ID: &str = "keyboard-layout";
const CAST_ID: &str = "screencast";

pub async fn run(emitter: Emitter) -> Result<()> {
    let path = std::env::var_os(SOCKET_PATH_ENV).context("NIRI_SOCKET is not set")?;
    let stream = UnixStream::connect(&path)
        .await
        .context("connecting to niri")?;
    let (read, mut write) = stream.into_split();
    let mut request = serde_json::to_string(&Request::EventStream)?;
    request.push('\n');
    write.write_all(request.as_bytes()).await?;

    let mut lines = BufReader::new(read).lines();
    let reply = lines.next_line().await?.context("niri closed the socket")?;
    serde_json::from_str::<niri_ipc::Reply>(&reply)?.map_err(anyhow::Error::msg)?;

    let mut state = State::default();
    while let Some(line) = lines.next_line().await? {
        match serde_json::from_str::<NiriEvent>(&line) {
            Ok(event) => state.handle(event, &emitter),
            // Newer niri versions may send events this build doesn't know.
            Err(e) => log::debug!("skipping niri event: {e}"),
        }
    }
    anyhow::bail!("niri closed the event stream")
}

#[derive(Default)]
struct State {
    /// Workspace id → output name.
    workspaces: HashMap<u64, Option<String>>,
    focused_output: Option<String>,
    layouts: Vec<String>,
    active_casts: HashSet<u64>,
}

impl State {
    fn handle(&mut self, event: NiriEvent, emitter: &Emitter) {
        match event {
            NiriEvent::WorkspacesChanged { workspaces } => {
                self.workspaces = workspaces
                    .iter()
                    .map(|w| (w.id, w.output.clone()))
                    .collect();
                let focused = workspaces
                    .iter()
                    .find(|w| w.is_focused)
                    .and_then(|w| w.output.clone());
                self.set_focus(focused, emitter);
            }
            NiriEvent::WorkspaceActivated { id, focused: true } => {
                let output = self.workspaces.get(&id).cloned().flatten();
                self.set_focus(output, emitter);
            }
            NiriEvent::KeyboardLayoutsChanged { keyboard_layouts } => {
                self.layouts = keyboard_layouts.names;
            }
            NiriEvent::KeyboardLayoutSwitched { idx } => {
                if let Some(name) = self.layouts.get(usize::from(idx)) {
                    emitter.push(osd(LAYOUT_ID, "keyboard", name.clone(), None));
                }
            }
            NiriEvent::CastsChanged { casts } => {
                self.active_casts = casts
                    .iter()
                    .filter(|c| c.is_active)
                    .map(|c| c.stream_id)
                    .collect();
                self.update_casts(emitter);
            }
            NiriEvent::CastStartedOrChanged { cast } => {
                if cast.is_active {
                    self.active_casts.insert(cast.stream_id);
                } else {
                    self.active_casts.remove(&cast.stream_id);
                }
                self.update_casts(emitter);
            }
            NiriEvent::CastStopped { stream_id } => {
                self.active_casts.remove(&stream_id);
                self.update_casts(emitter);
            }
            _ => {}
        }
    }

    fn set_focus(&mut self, output: Option<String>, emitter: &Emitter) {
        if output.is_some() && output != self.focused_output {
            self.focused_output = output.clone();
            emitter.emit(Event::FocusedOutput(output));
        }
    }

    fn update_casts(&self, emitter: &Emitter) {
        if self.active_casts.is_empty() {
            emitter.dismiss(CAST_ID);
        } else {
            emitter.push(ActivitySpec {
                icon: Some("screen-share".into()),
                placement: Placement::Right,
                priority: Priority::High,
                ..ActivitySpec::new(CAST_ID, "Screen is being shared")
            });
        }
    }
}
