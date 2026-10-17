//! A tiny Unix-socket control protocol so short-lived commands
//! (`linuxautoswitch status` / `pause` / `resume` / `toggle` / `waybar`) can
//! talk to the long-running daemon process, without needing the daemon
//! itself to be scriptable in-process.
//!
//! Wire format is deliberately trivial: the client writes one line (the
//! command word) and the server writes back one line of JSON.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

pub fn socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(dir).join("linuxautoswitch.sock")
}

#[derive(Debug, Clone, Copy)]
pub enum ControlRequest {
    Status,
    Pause,
    Resume,
    Toggle,
}

impl ControlRequest {
    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "status" => Some(Self::Status),
            "pause" => Some(Self::Pause),
            "resume" => Some(Self::Resume),
            "toggle" => Some(Self::Toggle),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub paused: bool,
    /// The most recently observed active layout, "en" or "ru" - `None` if
    /// the daemon hasn't seen a layout yet.
    pub lang: Option<String>,
    /// Whether the currently focused window is in the exclusion list (or a
    /// detected Steam game), so correction is inactive there regardless of
    /// `paused`.
    pub excluded: bool,
}

/// A request that arrived over the control socket, paired with a channel to
/// send the answer back on. Held open by the connection-handling thread
/// until the daemon's event loop replies.
pub struct ControlEvent {
    pub request: ControlRequest,
    pub reply: Sender<ControlResponse>,
}

/// Binds the control socket and spawns a thread accepting connections. Each
/// connection's request is forwarded into the caller's event channel (as a
/// `T` produced `From<ControlEvent>`, mirroring `hypr::listen`'s pattern so
/// this module doesn't need to know the concrete event enum), and whatever
/// comes back on the per-request reply channel is written back to the
/// client as JSON.
pub fn serve<T: From<ControlEvent> + Send + 'static>(tx: Sender<T>) -> Result<()> {
    let path = socket_path();
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            bail!(
                "another linuxautoswitch instance appears to already be running \
                 (control socket {} is live)",
                path.display()
            );
        }
        // Nothing is listening: a stale socket left over from an unclean
        // shutdown. Safe to remove and rebind.
        let _ = std::fs::remove_file(&path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let listener = bind_private(&path)?;
    tracing::info!(path = %path.display(), "control socket listening");

    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
            let tx = tx.clone();
            thread::spawn(move || {
                if let Err(e) = handle_connection(stream, &tx) {
                    tracing::debug!("control connection error: {e:#}");
                }
            });
        }
    });
    Ok(())
}

/// Binds `path` so it is never reachable by other users, not even briefly.
/// Without XDG_RUNTIME_DIR the socket lands in the shared /tmp, where any
/// local user could otherwise pause the daemon in the gap between `bind`
/// and `chmod`. The socket is therefore created in a private (0700)
/// directory, restricted to 0600, and only then renamed into place.
fn bind_private(path: &std::path::Path) -> Result<UnixListener> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let private = parent.join(format!(".linuxautoswitch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&private);
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&private)
            .with_context(|| format!("creating {}", private.display()))?;
    }
    let result = (|| {
        let tmp = private.join("sock");
        let listener = UnixListener::bind(&tmp)
            .with_context(|| format!("binding control socket at {}", tmp.display()))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting permissions on {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("moving control socket to {}", path.display()))?;
        Ok(listener)
    })();
    let _ = std::fs::remove_dir_all(&private);
    result
}

fn handle_connection<T: From<ControlEvent>>(mut stream: UnixStream, tx: &Sender<T>) -> Result<()> {
    // A client that connects and never sends a line shouldn't hold this
    // thread open forever.
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let Some(request) = ControlRequest::parse(&line) else {
        bail!("unknown command: {line:?}");
    };

    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if tx
        .send(
            ControlEvent {
                request,
                reply: reply_tx,
            }
            .into(),
        )
        .is_err()
    {
        bail!("daemon event loop is gone");
    }
    let response = reply_rx
        .recv_timeout(Duration::from_secs(2))
        .context("daemon did not respond in time")?;
    let json = serde_json::to_string(&response)?;
    stream.write_all(json.as_bytes())?;
    stream.write_all(b"\n")?;
    Ok(())
}

/// Client side: send a command word to a running daemon and return its
/// response.
pub fn send_command(command: &str) -> Result<ControlResponse> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "connecting to {} - is linuxautoswitch running?",
            path.display()
        )
    })?;
    stream.write_all(command.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    serde_json::from_str(&line).context("parsing daemon response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_private_creates_an_owner_only_socket_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("las-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sock");
        let _listener = bind_private(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(UnixStream::connect(&path).is_ok());
        let leftovers = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(leftovers, 1, "only the socket should remain");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
