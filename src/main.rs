mod config;
mod dictionary;
mod engine;
mod hypr;
mod keymap;
mod typer;

use clap::Parser;
use evdev::KeyCode;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
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
}

enum Event {
    Key {
        device: Arc<str>,
        code: KeyCode,
        value: i32,
    },
    Hypr(hypr::HyprEvent),
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

fn main() -> anyhow::Result<()> {
    init_logging();
    let args = Args::parse();
    let cfg = config::load(args.config)?;

    tracing::info!("linuxautoswitch starting");

    let (tx, rx) = mpsc::channel::<Event>();

    // Hyprland event socket -> Event channel.
    {
        let (hypr_tx, hypr_rx) = mpsc::channel();
        hypr::listen(hypr_tx);
        let tx = tx.clone();
        thread::spawn(move || {
            for ev in hypr_rx {
                if tx.send(Event::Hypr(ev)).is_err() {
                    break;
                }
            }
        });
    }

    // One reader thread per physical keyboard device.
    let mut watched = 0usize;
    for (path, device) in evdev::enumerate() {
        if !is_keyboard(&device) {
            continue;
        }
        let name: Arc<str> = Arc::from(device.name().unwrap_or("unknown-keyboard"));
        tracing::info!(path = %path.display(), name = %name, "watching keyboard device");
        let tx = tx.clone();
        let mut device = device;
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
                        return;
                    }
                }
            }
        });
        watched += 1;
    }
    drop(tx);

    if watched == 0 {
        tracing::warn!(
            "no keyboard devices found under /dev/input - is this user in the `input` group?"
        );
    }

    let mut engine = engine::Engine::new(cfg);
    for event in rx {
        match event {
            Event::Key {
                device,
                code,
                value,
            } => engine.handle_key(&device, code, value),
            Event::Hypr(ev) => engine.handle_hypr(ev),
        }
    }

    Ok(())
}
