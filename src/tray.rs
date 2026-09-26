//! A real system-tray icon via `ksni`, which implements the
//! freedesktop/KDE StatusNotifierItem D-Bus protocol - the same mechanism
//! any other tray application (a network applet, a chat client) uses. It
//! shows up in Waybar's own `tray` module, KDE Plasma natively, GNOME with
//! an AppIndicator extension, or any other SNI host - independent of the
//! `linuxautoswitch waybar` subcommand, which is a plain text status-bar
//! module for people who'd rather not run a tray host at all.
//!
//! The icon itself is drawn at runtime as a small ARGB bitmap (a solid
//! background colored by state, plus "EN"/"RU" text or a pause symbol in a
//! hand-rolled 5x7 pixel font) rather than shipped as image files - no
//! icon-theme installation step, no extra asset files to keep in sync.

use crate::keymap::Lang;
use ksni::Icon;
use ksni::blocking::TrayMethods;
use ksni::menu::StandardItem;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// An action requested by clicking a tray menu item. Generic `From`/`Into`
/// (mirroring `hypr::listen` and `control::serve`) so this module doesn't
/// need to know the concrete event enum the caller uses.
#[derive(Debug, Clone, Copy)]
pub enum TrayAction {
    TogglePause,
    Quit,
}

pub struct TrayIcon<T: From<TrayAction> + Send + 'static> {
    pub paused: bool,
    pub lang: Option<Lang>,
    tx: Sender<T>,
}

impl<T: From<TrayAction> + Send + 'static> ksni::Tray for TrayIcon<T> {
    fn id(&self) -> String {
        "linuxautoswitch".into()
    }

    fn title(&self) -> String {
        match (self.paused, self.lang) {
            (true, _) => "linuxautoswitch (paused)".into(),
            (false, Some(Lang::En)) => "linuxautoswitch - EN".into(),
            (false, Some(Lang::Ru)) => "linuxautoswitch - RU".into(),
            (false, None) => "linuxautoswitch".into(),
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![render_icon(self.paused, self.lang)]
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let pause_label = if self.paused { "Resume" } else { "Pause" }.to_string();
        vec![
            StandardItem {
                label: pause_label,
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayAction::TogglePause.into());
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayAction::Quit.into());
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// How long to wait, at startup, for a StatusNotifierWatcher (a tray host)
/// to appear on the session bus before giving up and spawning anyway.
const WATCHER_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const WATCHER_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// Spawns the tray icon on a background thread and returns a handle to it
/// once ready, delivered through a shared slot rather than directly, since
/// getting it right can take a moment (see below) and the caller shouldn't
/// block its own startup waiting for it.
///
/// `ksni`'s own fallback for "no tray host yet" (`assume_sni_available`)
/// tries an immediate registration, and only falls back to waiting for the
/// watcher's `NameOwnerChanged` signal if that first attempt fails - but it
/// only starts listening for that signal *after* the failed attempt, so a
/// watcher that appears in the narrow window between the two is missed
/// entirely, silently, with no icon ever appearing for the rest of the
/// session. This matters in practice: Quickshell (Omarchy's shell) is a
/// single process that instantiates its StatusNotifierWatcher lazily, when
/// its own tray widget loads as part of the wider shell/bar startup, which
/// can plausibly take longer than this daemon's own near-instant systemd
/// unit start - i.e. this isn't a rare edge case here, it can lose every
/// time. Waiting here for the watcher to actually exist before ever calling
/// `spawn()` sidesteps that race instead of depending on it.
pub fn spawn<T: From<TrayAction> + Send + 'static>(
    tx: Sender<T>,
) -> Arc<Mutex<Option<ksni::blocking::Handle<TrayIcon<T>>>>> {
    let slot = Arc::new(Mutex::new(None));
    let result_slot = slot.clone();
    thread::spawn(move || {
        if !wait_for_watcher(WATCHER_WAIT_TIMEOUT) {
            tracing::warn!(
                "no StatusNotifierWatcher (tray host) appeared within {}s; \
                 spawning anyway, but the tray icon may never show up this session",
                WATCHER_WAIT_TIMEOUT.as_secs()
            );
        }
        let tray = TrayIcon {
            paused: false,
            lang: None,
            tx,
        };
        match tray.assume_sni_available(true).spawn() {
            Ok(handle) => *result_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle),
            Err(e) => tracing::warn!("tray icon not available: {e}"),
        }
    });
    slot
}

/// Polls for `org.kde.StatusNotifierWatcher` having an owner on the session
/// bus, up to `timeout`. Returns `false` on timeout or if a session bus
/// can't be reached at all (in which case `spawn()`'s own attempt will fail
/// too, with its own warning).
fn wait_for_watcher(timeout: Duration) -> bool {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    loop {
        let has_owner = conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "NameHasOwner",
                &("org.kde.StatusNotifierWatcher",),
            )
            .ok()
            .and_then(|reply| reply.body().deserialize::<bool>().ok())
            .unwrap_or(false);
        if has_owner {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(WATCHER_POLL_INTERVAL);
    }
}

const ICON_SIZE: i32 = 48;
const SCALE: i32 = 4;
const FONT_WIDTH: usize = 5;
const FONT_HEIGHT: usize = 7;

/// 5x7 dot-matrix glyphs, MSB-first per row (bit 4 = leftmost column). Only
/// the letters actually needed ("EN"/"RU") plus a fallback dash for an
/// unknown layout.
fn glyph_rows(c: char) -> [u8; FONT_HEIGHT] {
    match c {
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10101, 0b10011, 0b10001, 0b10001,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        _ => [0, 0, 0, 0b11111, 0, 0, 0], // dash, for an unknown/"--" layout
    }
}

fn set_px(buf: &mut [u8], x: i32, y: i32) {
    if x < 0 || y < 0 || x >= ICON_SIZE || y >= ICON_SIZE {
        return;
    }
    let idx = ((y * ICON_SIZE + x) * 4) as usize;
    buf[idx] = 255;
    buf[idx + 1] = 255;
    buf[idx + 2] = 255;
    buf[idx + 3] = 255;
}

fn draw_glyph(buf: &mut [u8], origin_x: i32, origin_y: i32, c: char) {
    for (row, bits) in glyph_rows(c).iter().enumerate() {
        for col in 0..FONT_WIDTH {
            if bits & (1 << (FONT_WIDTH - 1 - col)) == 0 {
                continue;
            }
            let px = origin_x + col as i32 * SCALE;
            let py = origin_y + row as i32 * SCALE;
            for dy in 0..SCALE {
                for dx in 0..SCALE {
                    set_px(buf, px + dx, py + dy);
                }
            }
        }
    }
}

fn draw_text(buf: &mut [u8], text: &str) {
    let glyph_w = FONT_WIDTH as i32 * SCALE;
    let glyph_h = FONT_HEIGHT as i32 * SCALE;
    let count = text.chars().count() as i32;
    let total_w = glyph_w * count + SCALE * (count - 1).max(0);
    let start_x = (ICON_SIZE - total_w) / 2;
    let start_y = (ICON_SIZE - glyph_h) / 2;
    for (i, c) in text.chars().enumerate() {
        draw_glyph(buf, start_x + i as i32 * (glyph_w + SCALE), start_y, c);
    }
}

fn draw_pause_bars(buf: &mut [u8]) {
    let bar_w = 8;
    let bar_h = FONT_HEIGHT as i32 * SCALE;
    let gap = 8;
    let start_x = (ICON_SIZE - (bar_w * 2 + gap)) / 2;
    let start_y = (ICON_SIZE - bar_h) / 2;
    for bar in 0..2 {
        let x0 = start_x + bar * (bar_w + gap);
        for x in x0..x0 + bar_w {
            for y in start_y..start_y + bar_h {
                set_px(buf, x, y);
            }
        }
    }
}

/// Renders the tray icon: a background colored by state (gray when paused,
/// otherwise blue for English / green for Russian / dark gray if the
/// layout isn't known yet) with either "EN"/"RU" text or a pause symbol
/// drawn in white on top.
fn render_icon(paused: bool, lang: Option<Lang>) -> Icon {
    let (r, g, b) = if paused {
        (0x88, 0x88, 0x88)
    } else {
        match lang {
            Some(Lang::En) => (0x2b, 0x6c, 0xb0),
            Some(Lang::Ru) => (0x2f, 0x85, 0x5a),
            None => (0x55, 0x55, 0x55),
        }
    };

    let mut data = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
    for px in data.chunks_exact_mut(4) {
        // ARGB32, network (big-endian) byte order: A, R, G, B per pixel.
        px[0] = 255;
        px[1] = r;
        px[2] = g;
        px[3] = b;
    }

    if paused {
        draw_pause_bars(&mut data);
    } else {
        let text = match lang {
            Some(Lang::En) => "EN",
            Some(Lang::Ru) => "RU",
            None => "--",
        };
        draw_text(&mut data, text);
    }

    Icon {
        width: ICON_SIZE,
        height: ICON_SIZE,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(icon: &Icon, x: i32, y: i32) -> [u8; 4] {
        let idx = ((y * icon.width + x) * 4) as usize;
        [
            icon.data[idx],
            icon.data[idx + 1],
            icon.data[idx + 2],
            icon.data[idx + 3],
        ]
    }

    #[test]
    fn icon_has_expected_dimensions() {
        let icon = render_icon(false, Some(Lang::En));
        assert_eq!(icon.width, ICON_SIZE);
        assert_eq!(icon.height, ICON_SIZE);
        assert_eq!(icon.data.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
    }

    #[test]
    fn running_uses_language_background_color() {
        // Corner pixel is background, never touched by the centered text.
        assert_eq!(
            pixel(&render_icon(false, Some(Lang::En)), 0, 0),
            [255, 0x2b, 0x6c, 0xb0]
        );
        assert_eq!(
            pixel(&render_icon(false, Some(Lang::Ru)), 0, 0),
            [255, 0x2f, 0x85, 0x5a]
        );
    }

    #[test]
    fn paused_uses_gray_background_regardless_of_language() {
        assert_eq!(
            pixel(&render_icon(true, Some(Lang::En)), 0, 0),
            [255, 0x88, 0x88, 0x88]
        );
        assert_eq!(
            pixel(&render_icon(true, None), 0, 0),
            [255, 0x88, 0x88, 0x88]
        );
    }

    #[test]
    fn running_draws_white_text_pixels() {
        let icon = render_icon(false, Some(Lang::Ru));
        assert!(icon.data.chunks_exact(4).any(|p| p == [255, 255, 255, 255]));
    }

    #[test]
    fn paused_draws_white_bar_pixels() {
        let icon = render_icon(true, None);
        assert!(icon.data.chunks_exact(4).any(|p| p == [255, 255, 255, 255]));
    }
}
