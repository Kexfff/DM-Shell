//! IPC server (inside the shell's event loop) and client (used by the CLI).
//! The wire protocol is defined in `dmsh_core::ipc`.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use dmsh_core::ipc::{Request, Response, socket_path};
use smithay_client_toolkit::reexports::calloop::{
    Interest, LoopHandle, Mode, PostAction,
    generic::{Generic, NoIoDrop},
};

use crate::shell::Shell;

/// Requests longer than this are rejected and the connection closed.
const MAX_LINE: usize = 64 * 1024;

/// Removes the socket file when the shell exits normally. A file left behind by a crash
/// is detected as stale and replaced on the next start.
pub struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn listen(handle: &LoopHandle<'static, Shell>) -> Result<SocketGuard> {
    let path = socket_path().context("XDG_RUNTIME_DIR is not set")?;
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            bail!("another dmsh is already running on {}", path.display());
        }
        std::fs::remove_file(&path).context("removing stale socket")?;
    }
    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    listener.set_nonblocking(true)?;
    log::info!("listening on {}", path.display());

    let conn_handle = handle.clone();
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            move |_, listener, _| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(e) = add_connection(&conn_handle, stream) {
                                log::warn!("accepting IPC connection: {e}");
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            log::warn!("accepting IPC connection: {e}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("registering IPC socket: {e}"))?;
    Ok(SocketGuard(path))
}

fn add_connection(handle: &LoopHandle<'static, Shell>, stream: UnixStream) -> Result<()> {
    stream.set_nonblocking(true)?;
    let mut buf = Vec::new();
    handle
        .insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            move |_, stream, shell| {
                let mut chunk = [0u8; 4096];
                let mut eof = false;
                loop {
                    match (&**stream).read(&mut chunk) {
                        Ok(0) => {
                            eof = true;
                            break;
                        }
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => return Ok(PostAction::Remove),
                    }
                }

                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    if !respond(stream, shell, &line) {
                        return Ok(PostAction::Remove);
                    }
                }
                if buf.len() > MAX_LINE {
                    let error = Response::Error(format!("request longer than {MAX_LINE} bytes"));
                    send(stream, &error);
                    return Ok(PostAction::Remove);
                }
                if eof {
                    // Accept a final request without a trailing newline (e.g. `printf ... | socat`).
                    respond(stream, shell, &std::mem::take(&mut buf));
                    return Ok(PostAction::Remove);
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("registering IPC connection: {e}"))?;
    Ok(())
}

/// Handles one request line; returns `false` if the connection should be closed.
fn respond(stream: &NoIoDrop<UnixStream>, shell: &mut Shell, line: &[u8]) -> bool {
    let line = line.trim_ascii();
    if line.is_empty() {
        return true;
    }
    let response = match serde_json::from_slice::<Request>(line) {
        Ok(request) => shell.handle(request),
        Err(e) => Response::Error(format!("invalid request: {e}")),
    };
    send(stream, &response)
}

fn send(stream: &NoIoDrop<UnixStream>, response: &Response) -> bool {
    let mut out = serde_json::to_vec(response).expect("responses always serialize");
    out.push(b'\n');
    // Large state dumps may not fit the socket buffer; block briefly instead of dropping.
    let stream: &UnixStream = stream;
    let sent = stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(1))))
        .and_then(|()| (&*stream).write_all(&out))
        .and_then(|()| stream.set_nonblocking(true));
    sent.is_ok()
}

/// Sends one request to the running shell and waits for the response.
pub fn request(request: &Request) -> Result<Response> {
    let reply = request_raw(&serde_json::to_string(request)?)?;
    serde_json::from_str(&reply).context("parsing response")
}

/// Sends one raw JSON line and returns the response line as-is.
pub fn request_raw(line: &str) -> Result<String> {
    let path = socket_path().context("XDG_RUNTIME_DIR is not set")?;
    let mut stream = UnixStream::connect(&path)
        .with_context(|| format!("connecting to {} (is dmsh running?)", path.display()))?;
    stream.write_all(line.trim().as_bytes())?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    if reply.trim().is_empty() {
        bail!("shell closed the connection without replying");
    }
    Ok(reply.trim_end().to_owned())
}
