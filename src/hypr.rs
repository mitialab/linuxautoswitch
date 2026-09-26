//! Talks to Hyprland over its IPC unix sockets.
//!
//! Hyprland exposes two sockets under
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`:
//! - `.socket.sock`  - request/response, one command per connection.
//! - `.socket2.sock` - a stream of `EVENT>>payload` lines for everything
//!   that happens in the compositor (window focus, layout changes, ...).

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

fn instance_signature() -> Result<String> {
    std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .context("HYPRLAND_INSTANCE_SIGNATURE is not set - is Hyprland running?")
}

fn runtime_dir() -> Result<PathBuf> {
    let dir = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(dir).join("hypr").join(instance_signature()?))
}

fn cmd_socket_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join(".socket.sock"))
}

fn event_socket_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join(".socket2.sock"))
}

/// Sends a single command (exactly what you'd pass to `hyprctl`) and
/// returns its response. Prefix with `j/` for JSON output.
pub fn send_command(cmd: &str) -> Result<String> {
    let path = cmd_socket_path()?;
    let mut stream =
        UnixStream::connect(&path).with_context(|| format!("connecting to {}", path.display()))?;
    stream.write_all(cmd.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

#[derive(Debug, Deserialize, Clone)]
pub struct KeyboardDevice {
    pub name: String,
    #[serde(default)]
    pub main: bool,
    #[serde(default)]
    pub active_keymap: String,
}

#[derive(Debug, Deserialize)]
struct DevicesResponse {
    #[serde(default)]
    keyboards: Vec<KeyboardDevice>,
}

pub fn get_keyboards() -> Result<Vec<KeyboardDevice>> {
    let resp = send_command("j/devices")?;
    let parsed: DevicesResponse =
        serde_json::from_str(&resp).context("parsing `hyprctl j/devices` output")?;
    Ok(parsed.keyboards)
}

#[derive(Debug, Clone)]
pub struct ActiveWindow {
    /// Hyprland's unique window address; tells apart two windows of the
    /// same app, which share a class (and often a pid).
    pub address: String,
    pub class: String,
    /// PID of the process owning the focused window, used to walk up the
    /// process tree and recognize Steam games (see `crate::steam`).
    pub pid: i32,
}

pub fn get_active_window() -> Result<Option<ActiveWindow>> {
    let resp = send_command("j/activewindow")?;
    if resp.trim().is_empty() || resp.trim() == "{}" {
        return Ok(None);
    }
    let v: serde_json::Value =
        serde_json::from_str(&resp).context("parsing `hyprctl j/activewindow` output")?;
    let class = v.get("class").and_then(|c| c.as_str()).map(String::from);
    let pid = v.get("pid").and_then(|p| p.as_i64());
    let address = v.get("address").and_then(|a| a.as_str()).unwrap_or("");
    Ok(match (class, pid) {
        (Some(class), Some(pid)) => Some(ActiveWindow {
            address: address.to_string(),
            class,
            pid: pid as i32,
        }),
        _ => None,
    })
}

/// Switches `device` to the layout at `index` in its configured
/// `kb_layout` list (0-based, same order as in the user's Hyprland config).
pub fn switch_layout(device: &str, index: u32) -> Result<()> {
    let resp = send_command(&format!("switchxkblayout {device} {index}"))?;
    if resp.trim().eq_ignore_ascii_case("ok") || resp.trim().is_empty() {
        Ok(())
    } else {
        bail!("hyprctl switchxkblayout returned: {resp}");
    }
}

#[derive(Debug, Clone)]
pub enum HyprEvent {
    /// Focus changed. Carries no payload: the event line itself doesn't
    /// include the pid we need for Steam detection, so on receipt we just
    /// re-query `j/activewindow` for the full picture.
    ActiveWindow,
    ActiveLayout {
        keyboard: String,
        layout: String,
    },
}

/// Spawns a background thread that connects to Hyprland's event socket and
/// forwards the events we care about, converted into the caller's event
/// type so they can share one channel. Reconnects automatically (e.g.
/// across a Hyprland restart).
pub fn listen<T: From<HyprEvent> + Send + 'static>(tx: Sender<T>) {
    thread::spawn(move || {
        loop {
            if let Err(e) = connect_and_listen(&tx) {
                tracing::warn!("hyprland event socket error: {e:#}; retrying in 2s");
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
}

fn connect_and_listen<T: From<HyprEvent>>(tx: &Sender<T>) -> Result<()> {
    let path = event_socket_path()?;
    let stream =
        UnixStream::connect(&path).with_context(|| format!("connecting to {}", path.display()))?;
    tracing::info!("connected to hyprland event socket");
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
        if line.strip_prefix("activewindow>>").is_some() {
            if tx.send(HyprEvent::ActiveWindow.into()).is_err() {
                return Ok(());
            }
        } else if let Some(rest) = line.strip_prefix("activelayout>>") {
            let mut parts = rest.splitn(2, ',');
            let keyboard = parts.next().unwrap_or("").to_string();
            let layout = parts.next().unwrap_or("").to_string();
            if tx
                .send(HyprEvent::ActiveLayout { keyboard, layout }.into())
                .is_err()
            {
                return Ok(());
            }
        }
    }
    bail!("event socket closed")
}
