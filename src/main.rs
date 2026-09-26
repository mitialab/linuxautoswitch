mod config;
mod control;
mod dictionary;
mod engine;
mod hypr;
mod keymap;
mod steam;
mod tray;
mod typer;

use clap::{Parser, Subcommand};
use evdev::KeyCode;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;
use tracing_subscriber::prelude::*;

/// Automatic EN/RU keyboard layout correction for Hyprland/Omarchy.
///
/// Watches raw keystrokes, and when a word doesn't form a real word in the
/// currently active layout but does in the other one, retypes it correctly
/// and switches the layout - the same trick Punto Switcher / Caramba
/// Switcher use on Windows and macOS.
#[derive(Parser)]
#[command(name = "linuxautoswitch", version, about)]
struct Args {
    /// Path to config.toml (default: $XDG_CONFIG_HOME/linuxautoswitch/config.toml)
    #[arg(long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the running daemon's status.
    Status {
        /// Print raw JSON instead of a human-readable line.
        #[arg(long)]
        json: bool,
    },
    /// Pause automatic correction (the daemon keeps running).
    Pause,
    /// Resume automatic correction.
    Resume,
    /// Toggle paused/running - the same effect as the pause hotkey.
    Toggle,
    /// Print status as JSON for a Waybar (or similar) custom module.
    Waybar,
}

enum Event {
    Key {
        device: Arc<str>,
        code: KeyCode,
        value: i32,
    },
    Hypr(hypr::HyprEvent),
    /// A new keyboard device was plugged in.
    KeyboardsChanged,
    Control(control::ControlEvent),
    Tray(tray::TrayAction),
}

impl From<hypr::HyprEvent> for Event {
    fn from(ev: hypr::HyprEvent) -> Self {
        Event::Hypr(ev)
    }
}

impl From<control::ControlEvent> for Event {
    fn from(ev: control::ControlEvent) -> Self {
        Event::Control(ev)
    }
}

impl From<tray::TrayAction> for Event {
    fn from(action: tray::TrayAction) -> Self {
        Event::Tray(action)
    }
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    // Prefer journald when we're running as a systemd service; fall back to
    // plain stdout logging (e.g. when run by hand in a terminal).
    match tracing_journald::layer() {
        Ok(layer) => {
            registry.with(layer).init();
        }
        Err(_) => {
            registry.with(tracing_subscriber::fmt::layer()).init();
        }
    }
}

fn is_keyboard(device: &evdev::Device) -> bool {
    device
        .supported_keys()
        .map(|keys| keys.contains(KeyCode::KEY_A) && keys.contains(KeyCode::KEY_SPACE))
        .unwrap_or(false)
}

/// How often `/dev/input` is re-scanned for newly plugged-in keyboards.
const DEVICE_SCAN_INTERVAL: Duration = Duration::from_secs(2);

/// `/dev/input/event*` nodes that have already been examined (keyboards and
/// other devices alike), so each new node is only opened once.
type Seen = Arc<Mutex<HashSet<PathBuf>>>;

/// Opens every input device that appeared since the last scan and starts a
/// reader thread for each new keyboard. Returns how many were started.
fn scan_devices(seen: &Seen, tx: &Sender<Event>) -> usize {
    let present: HashSet<PathBuf> = match std::fs::read_dir("/dev/input") {
        Ok(dir) => dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("event"))
            })
            .collect(),
        Err(e) => {
            tracing::warn!("could not list /dev/input: {e}");
            return 0;
        }
    };
    let new: Vec<PathBuf> = {
        let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
        seen.retain(|path| present.contains(path));
        present
            .into_iter()
            .filter(|path| seen.insert(path.clone()))
            .collect()
    };

    let mut started = 0;
    for path in new {
        let device = match evdev::Device::open(&path) {
            Ok(device) => device,
            Err(e) => {
                // udev may not have granted access to a brand-new node yet;
                // try again on the next scan.
                tracing::debug!(path = %path.display(), "could not open input device: {e}");
                forget(seen, &path);
                continue;
            }
        };
        if is_keyboard(&device) {
            watch_device(path, device, seen.clone(), tx.clone());
            started += 1;
        }
    }
    started
}

fn forget(seen: &Seen, path: &Path) {
    seen.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(path);
}

/// Spawns a thread forwarding `device`'s key events until it's unplugged.
fn watch_device(path: PathBuf, mut device: evdev::Device, seen: Seen, tx: Sender<Event>) {
    let name: Arc<str> = Arc::from(device.name().unwrap_or("unknown-keyboard"));
    tracing::info!(path = %path.display(), name = %name, "watching keyboard device");
    thread::spawn(move || {
        loop {
            match device.fetch_events() {
                Ok(events) => {
                    for ev in events {
                        if let evdev::EventSummary::Key(_, code, value) = ev.destructure()
                            && tx
                                .send(Event::Key {
                                    device: name.clone(),
                                    code,
                                    value,
                                })
                                .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(name = %name, "device read error: {e}, no longer watching this device");
                    // If the same node comes back (e.g. replugged), the next
                    // scan picks it up again.
                    forget(&seen, &path);
                    return;
                }
            }
        }
    });
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.command {
        Some(cmd) => run_client_command(cmd),
        None => run_daemon(args.config),
    }
}

/// A short-lived invocation (`status`/`pause`/`resume`/`toggle`/`waybar`)
/// that talks to an already-running daemon over the control socket, prints
/// a result, and exits - as opposed to the no-subcommand form, which *is*
/// the daemon.
fn run_client_command(cmd: Command) -> anyhow::Result<()> {
    match cmd {
        Command::Status { json } => print_status(&control::send_command("status")?, json),
        Command::Pause => print_status(&control::send_command("pause")?, false),
        Command::Resume => print_status(&control::send_command("resume")?, false),
        Command::Toggle => print_status(&control::send_command("toggle")?, false),
        Command::Waybar => {
            let resp = control::send_command("status");
            println!("{}", waybar_json(resp.as_ref().ok()));
        }
    }
    Ok(())
}

fn print_status(resp: &control::ControlResponse, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(resp).unwrap_or_else(|_| "{}".to_string())
        );
        return;
    }
    if resp.paused {
        println!("linuxautoswitch: paused");
    } else {
        let lang = resp.lang.as_deref().unwrap_or("unknown").to_uppercase();
        let note = if resp.excluded {
            " (current window is excluded)"
        } else {
            ""
        };
        println!("linuxautoswitch: running - layout {lang}{note}");
    }
}

/// Waybar custom-module JSON: `{"text", "class", "tooltip"}`. `resp` is
/// `None` when the daemon isn't reachable at all.
fn waybar_json(resp: Option<&control::ControlResponse>) -> String {
    let Some(resp) = resp else {
        return r#"{"text":"?","class":"offline","tooltip":"linuxautoswitch is not running"}"#
            .to_string();
    };
    let lang = resp.lang.as_deref().unwrap_or("--").to_uppercase();
    if resp.paused {
        return format!(
            r#"{{"text":"⏸ {lang}","class":"paused","tooltip":"linuxautoswitch: paused"}}"#
        );
    }
    let class = lang.to_lowercase();
    format!(
        r#"{{"text":"{lang}","class":"{class}","tooltip":"linuxautoswitch: running ({lang})"}}"#
    )
}

fn run_daemon(config_path: Option<PathBuf>) -> anyhow::Result<()> {
    init_logging();
    let cfg = config::load(config_path)?;

    tracing::info!("linuxautoswitch starting");

    let (tx, rx) = mpsc::channel::<Event>();

    // Hyprland event socket -> Event channel.
    hypr::listen(tx.clone());

    // Control socket (status/pause/resume/toggle) -> Event channel. Not
    // fatal if it fails (e.g. another instance is already running) - the
    // core correction loop below works fine without it.
    if let Err(e) = control::serve(tx.clone()) {
        tracing::warn!("control socket not available: {e:#}");
    }

    // Real system-tray icon (StatusNotifierItem, via ksni), separate from
    // the `waybar` subcommand's text-only module. Also not fatal if no SNI
    // host is around at all (e.g. no D-Bus session).
    let tray_handle = tray::spawn(tx.clone());

    // One reader thread per physical keyboard device, including ones
    // plugged in later.
    let seen: Seen = Arc::default();
    if scan_devices(&seen, &tx) == 0 {
        tracing::warn!(
            "no keyboard devices found under /dev/input - is this user in the `input` group?"
        );
    }
    thread::spawn(move || {
        loop {
            thread::sleep(DEVICE_SCAN_INTERVAL);
            if scan_devices(&seen, &tx) > 0 && tx.send(Event::KeyboardsChanged).is_err() {
                return;
            }
        }
    });

    let mut engine = engine::Engine::new(cfg);
    let mut last_tray_status: Option<control::ControlResponse> = None;
    for event in rx {
        match event {
            Event::Key {
                device,
                code,
                value,
            } => engine.handle_key(&device, code, value),
            Event::Hypr(ev) => engine.handle_hypr(ev),
            Event::KeyboardsChanged => engine.refresh_keyboards(),
            Event::Control(ev) => {
                let response = engine.handle_control(ev.request);
                let _ = ev.reply.send(response);
            }
            Event::Tray(tray::TrayAction::TogglePause) => {
                engine.handle_control(control::ControlRequest::Toggle);
            }
            Event::Tray(tray::TrayAction::Quit) => {
                tracing::info!("quit requested from tray menu");
                break;
            }
        }

        // Cheap to compute (no I/O); only push to the tray - which crosses
        // threads and emits D-Bus signals - when something visible actually
        // changed, rather than on every keystroke. The tray spawns in the
        // background (see tray::spawn) and may not be in the slot yet.
        if let Some(handle) = tray_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let status = engine.handle_control(control::ControlRequest::Status);
            if last_tray_status.as_ref() != Some(&status) {
                let lang = status.lang.as_deref().and_then(|s| match s {
                    "en" => Some(keymap::Lang::En),
                    "ru" => Some(keymap::Lang::Ru),
                    _ => None,
                });
                let paused = status.paused;
                handle.update(|tray| {
                    tray.paused = paused;
                    tray.lang = lang;
                });
                last_tray_status = Some(status);
            }
        }
    }

    Ok(())
}
