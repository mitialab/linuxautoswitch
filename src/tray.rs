//! A real system-tray icon via `ksni`, which implements the
//! freedesktop/KDE StatusNotifierItem D-Bus protocol - the same mechanism
//! any other tray application (a network applet, a chat client) uses. It
//! shows up in Waybar's own `tray` module, KDE Plasma natively, GNOME with
//! an AppIndicator extension, or Quickshell (Omarchy's own shell) - any SNI
//! host - independent of the `linuxautoswitch waybar` subcommand, which is
//! a plain text status-bar module for people who'd rather not run a tray
//! host at all.
//!
//! The icon itself is drawn at runtime as a small ARGB circle (a single
//! letter - "E"/"Р" - in a hand-rolled 5x7 pixel font, on a solid disc)
//! rather than shipped as image files - no icon-theme installation step, no
//! extra asset files to keep in sync. The disc is filled with a fixed
//! neutral grey rather than a color pulled from Omarchy's active theme (see
//! the note on `FILL` below for why).

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

type Rgb = (u8, u8, u8);

/// The disc's fill color. Deliberately a fixed neutral grey rather than
/// anything pulled from Omarchy's active theme (its `colors.toml`'s
/// `accent`/`muted` fields, which an earlier version of this icon used):
/// those are semantic roles, not a promise of being visually neutral - on
/// at least one real theme `muted` is a saturated navy blue, which made
/// this icon (a raw ARGB pixmap, so unlike other tray items it can't be
/// recolored by the tray host to match) stand out against the genuinely
/// monochrome symbolic icons next to it instead of blending in. This value
/// was picked to match that observed neutral tray-icon grey directly.
const FILL: Rgb = (0xa0, 0xa0, 0xa0);

/// Picks black or white text for readability over `bg`, using perceived
/// luminance (ITU-R BT.601) rather than assuming any particular theme
/// accent is light or dark - they vary a lot from theme to theme.
fn contrasting_text(bg: Rgb) -> Rgb {
    let (r, g, b) = bg;
    let luminance = 0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64;
    if luminance > 150.0 {
        (0x1a, 0x1a, 0x1a)
    } else {
        (0xff, 0xff, 0xff)
    }
}

/// 5x7 dot-matrix glyphs, MSB-first per row (bit 4 = leftmost column).
/// 'P' doubles as Cyrillic 'Р' (er), which has the same shape as Latin
/// P - used for the Russian layout indicator - so there's no need for a
/// separate non-ASCII glyph.
fn glyph_rows(c: char) -> [u8; FONT_HEIGHT] {
    match c {
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        _ => [0, 0, 0, 0b11111, 0, 0, 0], // dash, for an unknown layout
    }
}

fn set_px(buf: &mut [u8], x: i32, y: i32, color: Rgb) {
    if x < 0 || y < 0 || x >= ICON_SIZE || y >= ICON_SIZE {
        return;
    }
    let idx = ((y * ICON_SIZE + x) * 4) as usize;
    // ARGB32, network (big-endian) byte order: A, R, G, B per pixel.
    buf[idx] = 255;
    buf[idx + 1] = color.0;
    buf[idx + 2] = color.1;
    buf[idx + 3] = color.2;
}

/// Fills a disc of `radius` around the icon's center with `color`; pixels
/// outside stay fully transparent, so the tray shows a round icon rather
/// than a square one.
fn draw_disc(buf: &mut [u8], radius: f64, color: Rgb) {
    let center = ICON_SIZE as f64 / 2.0;
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = x as f64 + 0.5 - center;
            let dy = y as f64 + 0.5 - center;
            if dx * dx + dy * dy <= radius * radius {
                set_px(buf, x, y, color);
            }
        }
    }
}

fn draw_glyph_centered(buf: &mut [u8], c: char, color: Rgb) {
    let glyph_w = FONT_WIDTH as i32 * SCALE;
    let glyph_h = FONT_HEIGHT as i32 * SCALE;
    let origin_x = (ICON_SIZE - glyph_w) / 2;
    let origin_y = (ICON_SIZE - glyph_h) / 2;
    for (row, bits) in glyph_rows(c).iter().enumerate() {
        for col in 0..FONT_WIDTH {
            if bits & (1 << (FONT_WIDTH - 1 - col)) == 0 {
                continue;
            }
            let px = origin_x + col as i32 * SCALE;
            let py = origin_y + row as i32 * SCALE;
            for dy in 0..SCALE {
                for dx in 0..SCALE {
                    set_px(buf, px + dx, py + dy, color);
                }
            }
        }
    }
}

fn draw_pause_bars_centered(buf: &mut [u8], color: Rgb) {
    let bar_w = 6;
    let bar_h = FONT_HEIGHT as i32 * SCALE;
    let gap = 6;
    let start_x = (ICON_SIZE - (bar_w * 2 + gap)) / 2;
    let start_y = (ICON_SIZE - bar_h) / 2;
    for bar in 0..2 {
        let x0 = start_x + bar * (bar_w + gap);
        for x in x0..x0 + bar_w {
            for y in start_y..start_y + bar_h {
                set_px(buf, x, y, color);
            }
        }
    }
}

/// Renders the tray icon: a grey disc (see `FILL`) with either a single
/// "E"/"P" letter or a pause symbol in a contrasting color on top. Paused
/// vs. running is conveyed by that symbol, not by color.
fn render_icon(paused: bool, lang: Option<Lang>) -> Icon {
    let text_color = contrasting_text(FILL);

    let mut data = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
    draw_disc(&mut data, ICON_SIZE as f64 / 2.0 - 1.0, FILL);

    if paused {
        draw_pause_bars_centered(&mut data, text_color);
    } else {
        let letter = match lang {
            Some(Lang::En) => 'E',
            Some(Lang::Ru) => 'P', // Cyrillic Р
            None => '-',
        };
        draw_glyph_centered(&mut data, letter, text_color);
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
    fn corner_pixels_are_transparent_outside_the_circle() {
        let icon = render_icon(false, Some(Lang::En));
        assert_eq!(pixel(&icon, 0, 0)[0], 0, "corner alpha should be 0");
    }

    #[test]
    fn fill_is_the_fixed_grey_regardless_of_state_or_language() {
        let center = ICON_SIZE / 2;
        for (paused, lang) in [
            (false, Some(Lang::En)),
            (false, Some(Lang::Ru)),
            (false, None),
            (true, Some(Lang::En)),
            (true, None),
        ] {
            let icon = render_icon(paused, lang);
            // Off to the side of the centered glyph, still plain fill color.
            assert_eq!(pixel(&icon, 4, center)[0], 255, "should be inside the disc");
            assert_eq!(pixel(&icon, 4, center), [255, FILL.0, FILL.1, FILL.2]);
        }
    }

    #[test]
    fn running_draws_a_contrasting_letter() {
        let icon = render_icon(false, Some(Lang::Ru));
        let text = contrasting_text(FILL);
        assert!(
            icon.data
                .chunks_exact(4)
                .any(|p| p == [255, text.0, text.1, text.2])
        );
    }

    #[test]
    fn paused_draws_contrasting_bars() {
        let icon = render_icon(true, None);
        let text = contrasting_text(FILL);
        assert!(
            icon.data
                .chunks_exact(4)
                .any(|p| p == [255, text.0, text.1, text.2])
        );
    }

    #[test]
    fn contrasting_text_picks_white_on_dark_black_on_light() {
        assert_eq!(contrasting_text((0x10, 0x10, 0x10)), (0xff, 0xff, 0xff));
        assert_eq!(contrasting_text((0xf0, 0xf0, 0xf0)), (0x1a, 0x1a, 0x1a));
    }
}
