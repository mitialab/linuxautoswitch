//! Core decision logic: buffers keystrokes into words, and on each word
//! boundary decides whether the word was typed in the wrong layout.

use crate::config::Config;
use crate::keymap::{self, Lang};
use crate::{control, dictionary, hypr, steam, typer};
use evdev::KeyCode;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
struct TypedKey {
    code: KeyCode,
    shift: bool,
    capslock: bool,
}

/// The most recently completed word, kept around so the manual-flip hotkey
/// can still act on it after the fact - e.g. after a word the automatic
/// dictionary check didn't recognize (a name, jargon) has already had Space
/// pressed after it.
struct LastWord {
    keys: Vec<TypedKey>,
    /// Which language's text is actually on screen right now for these keys.
    lang_on_screen: Lang,
    /// Spaces typed after the word, between it and the cursor. A flip has to
    /// delete and retype them too.
    trailing_spaces: usize,
}

/// One bit per physical modifier key, so releasing one side of a pair
/// (e.g. right Ctrl) doesn't clear the other side while it's still held.
const LEFT_SHIFT: u8 = 1 << 0;
const RIGHT_SHIFT: u8 = 1 << 1;
const LEFT_CTRL: u8 = 1 << 2;
const RIGHT_CTRL: u8 = 1 << 3;
const LEFT_ALT: u8 = 1 << 4;
const RIGHT_ALT: u8 = 1 << 5;
const LEFT_META: u8 = 1 << 6;
const RIGHT_META: u8 = 1 << 7;

const SHIFT: u8 = LEFT_SHIFT | RIGHT_SHIFT;
/// Modifiers that turn a keypress into a shortcut rather than text.
const SHORTCUT: u8 = LEFT_CTRL | RIGHT_CTRL | LEFT_ALT | RIGHT_ALT | LEFT_META | RIGHT_META;

fn modifier_bit(code: KeyCode) -> Option<u8> {
    Some(match code {
        KeyCode::KEY_LEFTSHIFT => LEFT_SHIFT,
        KeyCode::KEY_RIGHTSHIFT => RIGHT_SHIFT,
        KeyCode::KEY_LEFTCTRL => LEFT_CTRL,
        KeyCode::KEY_RIGHTCTRL => RIGHT_CTRL,
        KeyCode::KEY_LEFTALT => LEFT_ALT,
        KeyCode::KEY_RIGHTALT => RIGHT_ALT,
        KeyCode::KEY_LEFTMETA => LEFT_META,
        KeyCode::KEY_RIGHTMETA => RIGHT_META,
        _ => return None,
    })
}

/// Longest run of word keys still treated as a word. Anything longer is
/// not a real word (or is text we can't safely retype), so the buffer
/// stops growing and no correction is attempted until the next boundary.
const MAX_WORD_KEYS: usize = 64;

pub struct Engine {
    cfg: Config,
    buffer: Vec<TypedKey>,
    /// Set once the current word runs past `MAX_WORD_KEYS`.
    overflowed: bool,
    /// The current word was flipped by hand; don't let automatic correction
    /// flip it back before it's finished.
    manually_flipped: bool,
    /// Currently held modifier keys, as `LEFT_SHIFT | ...` bits.
    modifiers: u8,
    capslock: bool,
    /// Hyprland address of the focused window.
    current_window: String,
    /// Cached exclusion check (window class + Steam detection) for the
    /// focused window, refreshed only when focus changes rather than on
    /// every keystroke.
    excluded: bool,
    /// Toggled by the pause hotkey or a `pause`/`resume`/`toggle` control
    /// command; independent of `cfg.general.enabled`, which only sets the
    /// starting state.
    paused: bool,
    last_word: Option<LastWord>,
    /// The key to double-tap for a manual flip, resolved from
    /// `cfg.hotkeys.manual_correct_key`; `None` if hotkeys are disabled or
    /// the configured name didn't parse.
    manual_correct_key: Option<KeyCode>,
    manual_correct_window: Duration,
    /// When the last clean tap of `manual_correct_key` was released.
    manual_correct_last_tap: Option<Instant>,
    /// `manual_correct_key` is down and nothing else has been pressed since,
    /// so releasing it counts as a tap. Shift pressed to type a capital
    /// letter is not a tap.
    manual_tap_pending: bool,
    /// Modifier bits that must all be held to toggle pause, resolved from
    /// `cfg.hotkeys.toggle_pause_keys`; 0 disables the hotkey.
    toggle_pause_mask: u8,
    /// Every toggle-pause key is held and nothing else was pressed since
    /// the first of them went down; the toggle fires on release.
    toggle_pause_armed: bool,
    /// Some other key was pressed while a toggle-pause key was held, e.g.
    /// both Shifts overlapping while typing capitals. Cleared once all of
    /// them are released.
    toggle_pause_dirty: bool,
    /// Hyprland keyboard device name -> last known active language.
    device_lang: HashMap<String, Lang>,
    /// The most recently observed active language on any keyboard, used
    /// when the typing keyboard's own layout isn't known.
    last_lang: Option<Lang>,
    keyboards: Vec<hypr::KeyboardDevice>,
    /// evdev device name -> matched Hyprland device name. The fallback to
    /// the main keyboard is never cached, and the cache is dropped whenever
    /// the keyboard list is refreshed.
    resolved_devices: HashMap<String, String>,
}

impl Engine {
    pub fn new(mut cfg: Config) -> Self {
        // Lowercase every case-insensitive matcher once up front so the
        // comparisons below don't allocate.
        for list in [
            &mut cfg.general.excluded_classes,
            &mut cfg.layouts.english_match,
            &mut cfg.layouts.russian_match,
        ] {
            for s in list.iter_mut() {
                *s = s.to_lowercase();
            }
        }

        let (current_window, excluded) = match hypr::get_active_window().ok().flatten() {
            Some(win) => (win.address, compute_excluded(&win.class, win.pid, &cfg)),
            None => (String::new(), false),
        };

        let manual_correct_key = if cfg.hotkeys.enabled {
            resolve_hotkey_key("manual_correct_key", &cfg.hotkeys.manual_correct_key)
        } else {
            None
        };
        let toggle_pause_mask = if cfg.hotkeys.enabled {
            resolve_hotkey_mask("toggle_pause_keys", &cfg.hotkeys.toggle_pause_keys)
        } else {
            0
        };
        let manual_correct_window = Duration::from_millis(cfg.hotkeys.manual_correct_window_ms);
        let paused = !cfg.general.enabled;

        let mut engine = Self {
            cfg,
            buffer: Vec::new(),
            overflowed: false,
            manually_flipped: false,
            modifiers: 0,
            capslock: false,
            current_window,
            excluded,
            paused,
            last_word: None,
            manual_correct_key,
            manual_correct_window,
            manual_correct_last_tap: None,
            manual_tap_pending: false,
            toggle_pause_mask,
            toggle_pause_armed: false,
            toggle_pause_dirty: false,
            device_lang: HashMap::new(),
            last_lang: None,
            keyboards: Vec::new(),
            resolved_devices: HashMap::new(),
        };
        engine.refresh_keyboards();
        engine
    }

    /// Re-reads Hyprland's keyboard list, e.g. after a keyboard is plugged
    /// in. Layouts already learned from events are kept.
    pub fn refresh_keyboards(&mut self) {
        match hypr::get_keyboards() {
            Ok(keyboards) => {
                for kb in &keyboards {
                    if !self.device_lang.contains_key(&kb.name)
                        && let Some(lang) = detect_lang(&kb.active_keymap, &self.cfg)
                    {
                        self.device_lang.insert(kb.name.clone(), lang);
                        if kb.main || self.last_lang.is_none() {
                            self.last_lang = Some(lang);
                        }
                    }
                }
                self.keyboards = keyboards;
                self.resolved_devices.clear();
            }
            Err(e) => tracing::warn!("could not query hyprctl devices: {e:#}"),
        }
    }

    /// Re-reads the focused window (class + pid) from Hyprland and
    /// refreshes the cached exclusion check. The `ActiveWindow` event
    /// carries no payload of its own - unlike the class, the pid we need for
    /// Steam detection isn't in the event line, so this is how we get it.
    fn refresh_active_window(&mut self) {
        let active = match hypr::get_active_window() {
            Ok(active) => active,
            Err(e) => {
                tracing::debug!("could not query active window: {e:#}");
                // Focus did change, we just can't tell where to; don't
                // carry the word over.
                self.forget_words();
                return;
            }
        };
        let (window, class, pid) = match active {
            Some(win) => (win.address, win.class, win.pid),
            None => (String::new(), String::new(), 0),
        };
        // Compare windows, not classes: two windows of the same app share a
        // class, and a word typed in one must not be retyped into the other.
        if window != self.current_window {
            self.excluded = compute_excluded(&class, pid, &self.cfg);
            self.current_window = window;
            self.forget_words();
        }
    }

    /// Drops both the word being typed and the last completed one, e.g.
    /// when focus moves: neither is safe to retype into another window.
    fn forget_words(&mut self) {
        self.reset_word();
        self.last_word = None;
    }

    fn reset_word(&mut self) {
        self.buffer.clear();
        self.overflowed = false;
        self.manually_flipped = false;
    }

    fn shift(&self) -> bool {
        self.modifiers & SHIFT != 0
    }

    pub fn handle_hypr(&mut self, ev: hypr::HyprEvent) {
        match ev {
            hypr::HyprEvent::ActiveWindow => self.refresh_active_window(),
            hypr::HyprEvent::ActiveLayout { keyboard, layout } => {
                if !self.keyboards.iter().any(|kb| kb.name == keyboard) {
                    // A keyboard we haven't seen yet, e.g. just plugged in.
                    self.refresh_keyboards();
                }
                if let Some(lang) = detect_lang(&layout, &self.cfg) {
                    tracing::debug!(keyboard = %keyboard, layout = %layout, ?lang, "layout changed");
                    self.device_lang.insert(keyboard, lang);
                    self.last_lang = Some(lang);
                }
            }
        }
    }

    /// `value`: 0 = key up, 1 = key down, 2 = autorepeat.
    ///
    /// Autorepeat is handled like a key-down: a held letter or Backspace
    /// keeps typing or deleting on screen, so the buffer has to follow it.
    pub fn handle_key(&mut self, source_device: &str, code: KeyCode, value: i32) {
        let held = value != 0;

        if value == 1 {
            self.track_hotkey_press(code);
        }

        if let Some(bit) = modifier_bit(code) {
            if held {
                self.modifiers |= bit;
            } else {
                self.modifiers &= !bit;
            }
            if value == 0 {
                self.track_hotkey_release(code, bit, source_device);
            }
            return;
        }
        if code == KeyCode::KEY_CAPSLOCK {
            if value == 1 {
                self.capslock = !self.capslock;
            }
            return;
        }

        if !held {
            return; // everything below only reacts to key-down and autorepeat
        }

        if self.paused || self.excluded {
            self.forget_words();
            return;
        }

        if code == KeyCode::KEY_BACKSPACE {
            if self.buffer.pop().is_none() {
                // Deleting past the current word eats into what follows the
                // last word: a trailing space, or the word itself.
                match &mut self.last_word {
                    Some(word) if word.trailing_spaces > 0 => word.trailing_spaces -= 1,
                    _ => self.last_word = None,
                }
            }
            return;
        }

        if self.modifiers & SHORTCUT != 0 {
            // A shortcut (Ctrl+C, Alt+Tab, Super+...), not text. Whatever
            // was buffered before it is no longer contiguous on screen, and
            // the shortcut may have changed the text (paste, select all).
            self.forget_words();
            return;
        }

        if keymap::is_word_key(code) {
            if self.overflowed {
                return;
            }
            if self.buffer.len() >= MAX_WORD_KEYS {
                self.buffer.clear();
                self.overflowed = true;
                self.last_word = None;
                return;
            }
            self.buffer.push(TypedKey {
                code,
                shift: self.shift(),
                capslock: self.capslock,
            });
            // Check after every keystroke, not just at a boundary: text
            // that's acted on without ever hitting Space - a browser
            // address bar you press Enter on, a chat message sent with
            // Enter, a search box - would otherwise never get corrected in
            // time, since by the time we see Enter the app has already
            // acted on it. This way the word is usually already fixed
            // before that happens.
            if self.cfg.general.eager_correction && !self.manually_flipped {
                self.try_correct(source_device, "", self.cfg.general.eager_min_word_length);
            }
            return;
        }

        // Space, enter, tab, digits, punctuation, arrows, etc. all end the
        // current word, but only a space is checked for a correction here.
        // Eager correction above already handles anything that completed a
        // real word before this point; by the time we see Enter, Tab, or an
        // arrow key the application has already acted on it (sent a
        // message, moved focus, moved the cursor), so retyping past those
        // would be wrong or too late. A space is still safe to redo.
        let is_space = code == KeyCode::KEY_SPACE;
        if self.buffer.is_empty() {
            // Nothing typed since the last word (e.g. eager correction
            // already fixed it): only a space keeps it flippable.
            match &mut self.last_word {
                Some(word) if is_space && !self.overflowed => word.trailing_spaces += 1,
                _ => self.last_word = None,
            }
        } else if !is_space || self.overflowed {
            self.last_word = None;
        } else if self.manually_flipped
            || !self.try_correct(source_device, " ", self.cfg.general.min_word_length)
        {
            // Not corrected - the word was fine, too short to check, or not
            // recognized in either language. Keep it anyway: this is exactly
            // the case the manual flip hotkey exists for.
            let device_name = self.resolve_hypr_device_name(source_device);
            self.last_word = self
                .lang_for_device(device_name.as_deref())
                .map(|lang| LastWord {
                    keys: self.buffer.clone(),
                    lang_on_screen: lang,
                    trailing_spaces: 1,
                });
        }
        self.reset_word();
    }

    /// Hotkeys are clean taps and chords: they only count when no other key
    /// is pressed in between, so Shift held to type a capital letter never
    /// triggers anything. Called on every fresh key-down.
    fn track_hotkey_press(&mut self, code: KeyCode) {
        if Some(code) == self.manual_correct_key {
            self.manual_tap_pending = true;
        } else {
            self.manual_tap_pending = false;
            self.manual_correct_last_tap = None;
        }

        let mask = self.toggle_pause_mask;
        if mask == 0 {
            return;
        }
        let bit = modifier_bit(code).unwrap_or(0);
        if bit & mask == 0 {
            if self.modifiers & mask != 0 {
                self.toggle_pause_dirty = true;
                self.toggle_pause_armed = false;
            }
            return;
        }
        let pressed = self.modifiers | bit;
        if !self.toggle_pause_dirty && pressed & mask == mask {
            self.toggle_pause_armed = true;
        }
    }

    /// Fires tap and chord hotkeys when their key is released. `bit` has
    /// already been cleared from `self.modifiers`.
    fn track_hotkey_release(&mut self, code: KeyCode, bit: u8, source_device: &str) {
        if Some(code) == self.manual_correct_key && self.manual_tap_pending {
            self.manual_tap_pending = false;
            let now = Instant::now();
            let is_double_tap = self
                .manual_correct_last_tap
                .is_some_and(|t| now.duration_since(t) <= self.manual_correct_window);
            if is_double_tap {
                // Consumed: a third tap starts a fresh pair rather than
                // firing again immediately.
                self.manual_correct_last_tap = None;
                self.manual_flip(source_device);
            } else {
                self.manual_correct_last_tap = Some(now);
            }
        }

        let mask = self.toggle_pause_mask;
        if bit & mask != 0 {
            // Toggle on the first release of the full chord - once, not
            // again as the remaining keys come up. Works regardless of
            // `paused`/`excluded`, since it's the way out of `paused`.
            if self.toggle_pause_armed && !self.toggle_pause_dirty {
                self.paused = !self.paused;
                tracing::info!(paused = self.paused, "toggled pause via hotkey");
                self.forget_words();
            }
            self.toggle_pause_armed = false;
            if self.modifiers & mask == 0 {
                self.toggle_pause_dirty = false;
            }
        }
    }

    /// Checks the current buffer for a wrong-layout word and, if found,
    /// retypes it - plus `boundary`, text a word-ending key may have
    /// already put on screen - and switches the layout. Returns whether a
    /// correction was made; only then is the buffer cleared, since a caller
    /// still mid-word (the eager path) needs to keep accumulating
    /// otherwise.
    fn try_correct(&mut self, source_device: &str, boundary: &str, min_length: usize) -> bool {
        // Each key yields at most one letter, so a buffer shorter than the
        // minimum can't qualify. Checked first because eager correction runs
        // this on every keystroke.
        if self.overflowed || self.buffer.len() < min_length.max(MIN_CORRECTION_LENGTH) {
            return false;
        }

        let device_name = self.resolve_hypr_device_name(source_device);
        let Some(current_lang) = self.lang_for_device(device_name.as_deref()) else {
            tracing::debug!("no known active layout yet, skipping correction check");
            return false;
        };

        let other_lang = current_lang.other();
        let Some(corrected) = correction(&self.buffer, current_lang, min_length) else {
            return false;
        };

        let keys_snapshot = self.buffer.clone();
        let trailing_spaces = boundary.chars().count();
        let backspaces = self.buffer.len() + trailing_spaces;
        if let Err(e) = typer::correct_word(backspaces, &format!("{corrected}{boundary}")) {
            tracing::warn!("failed to retype corrected word: {e:#}");
            return false;
        }
        // Whatever was typed is gone now regardless of whether the layout
        // switch below succeeds - don't let a stale buffer trigger another
        // correction on top of this one.
        self.reset_word();
        // The word now on screen is in `other_lang` - if it's still wrong
        // (a name that happened to also look wrong in the dictionary's
        // eyes), the manual flip hotkey can undo this.
        self.last_word = Some(LastWord {
            keys: keys_snapshot,
            lang_on_screen: other_lang,
            trailing_spaces,
        });

        match device_name {
            Some(device_name) => {
                let index = match other_lang {
                    Lang::En => self.cfg.layouts.english_index,
                    Lang::Ru => self.cfg.layouts.russian_index,
                };
                match hypr::switch_layout(&device_name, index) {
                    Ok(()) => {
                        self.device_lang.insert(device_name, other_lang);
                        self.last_lang = Some(other_lang);
                    }
                    Err(e) => tracing::warn!("failed to switch layout: {e:#}"),
                }
            }
            None => tracing::warn!(
                "could not resolve a hyprland keyboard device for '{source_device}'; \
                 set `hypr.device_name_override` in the config"
            ),
        }
        true
    }

    fn lang_for_device(&self, device_name: Option<&str>) -> Option<Lang> {
        if let Some(name) = device_name
            && let Some(lang) = self.device_lang.get(name)
        {
            return Some(*lang);
        }
        // Fall back to whatever layout we've most recently observed on any
        // keyboard - better than nothing on unusual multi-keyboard setups.
        self.last_lang
    }

    /// Hyprland slugifies libinput device names (lowercase, spaces -> `-`)
    /// for its own device identifiers; evdev gives us the human-readable
    /// name. Reproduce that transform to match them up.
    fn resolve_hypr_device_name(&mut self, evdev_name: &str) -> Option<String> {
        if let Some(o) = &self.cfg.hypr.device_name_override {
            return Some(o.clone());
        }
        if let Some(cached) = self.resolved_devices.get(evdev_name) {
            return Some(cached.clone());
        }
        let slug = evdev_name.to_lowercase().replace(' ', "-");
        if let Some(kb) = self
            .keyboards
            .iter()
            .find(|kb| kb.name == slug || kb.name.contains(&slug) || slug.contains(&kb.name))
        {
            self.resolved_devices
                .insert(evdev_name.to_string(), kb.name.clone());
            return Some(kb.name.clone());
        }
        // Not cached: the keyboard may just not be in Hyprland's list yet, and
        // a later refresh should get a chance to match it properly.
        self.keyboards
            .iter()
            .find(|k| k.main)
            .or_else(|| self.keyboards.first())
            .map(|k| k.name.clone())
    }

    /// Flips the word under the cursor to the other language, bypassing the
    /// dictionary check entirely - for words the automatic detection didn't
    /// recognize (names, jargon, anything not in either dictionary).
    /// Prefers the word still being typed, if there is one; otherwise falls
    /// back to the last completed word.
    fn manual_flip(&mut self, source_device: &str) {
        if self.paused || self.excluded {
            return;
        }

        if !self.buffer.is_empty() && !self.overflowed {
            let device_name = self.resolve_hypr_device_name(source_device);
            let Some(current_lang) = self.lang_for_device(device_name.as_deref()) else {
                return;
            };
            let keys = self.buffer.clone();
            if self
                .flip_and_retype(&keys, current_lang, 0, source_device)
                .is_some()
            {
                self.manually_flipped = true;
            }
            return;
        }

        if let Some(LastWord {
            keys,
            lang_on_screen,
            trailing_spaces,
        }) = self.last_word.take()
            && let Some(new_lang) =
                self.flip_and_retype(&keys, lang_on_screen, trailing_spaces, source_device)
        {
            // Toggle back and forth on repeated presses, same as Caramba.
            self.last_word = Some(LastWord {
                keys,
                lang_on_screen: new_lang,
                trailing_spaces,
            });
        }
    }

    /// Retypes `keys` (currently shown as `on_screen_lang`, followed by
    /// `trailing_spaces` spaces) as the other language and switches the
    /// active layout to match. Returns the new on-screen language on
    /// success.
    fn flip_and_retype(
        &mut self,
        keys: &[TypedKey],
        on_screen_lang: Lang,
        trailing_spaces: usize,
        source_device: &str,
    ) -> Option<Lang> {
        let (corrected, target_lang) = flipped(keys, on_screen_lang)?;
        tracing::info!(to = %corrected, ?target_lang, "manually flipping word");

        let text = corrected + &" ".repeat(trailing_spaces);
        if let Err(e) = typer::correct_word(keys.len() + trailing_spaces, &text) {
            tracing::warn!("failed to retype flipped word: {e:#}");
            return None;
        }

        match self.resolve_hypr_device_name(source_device) {
            Some(device_name) => {
                let index = match target_lang {
                    Lang::En => self.cfg.layouts.english_index,
                    Lang::Ru => self.cfg.layouts.russian_index,
                };
                match hypr::switch_layout(&device_name, index) {
                    Ok(()) => {
                        self.device_lang.insert(device_name, target_lang);
                        self.last_lang = Some(target_lang);
                    }
                    Err(e) => tracing::warn!("failed to switch layout: {e:#}"),
                }
            }
            None => tracing::warn!(
                "could not resolve a hyprland keyboard device for '{source_device}'; \
                 set `hypr.device_name_override` in the config"
            ),
        }
        Some(target_lang)
    }

    /// Answers a request from the control socket (`linuxautoswitch status` /
    /// `pause` / `resume` / `toggle`).
    pub fn handle_control(&mut self, request: control::ControlRequest) -> control::ControlResponse {
        match request {
            control::ControlRequest::Status => {}
            control::ControlRequest::Pause => self.paused = true,
            control::ControlRequest::Resume => self.paused = false,
            control::ControlRequest::Toggle => self.paused = !self.paused,
        }
        control::ControlResponse {
            paused: self.paused,
            lang: self.last_lang.map(|lang| match lang {
                Lang::En => "en".to_string(),
                Lang::Ru => "ru".to_string(),
            }),
            excluded: self.excluded,
        }
    }
}

/// The `(text, language)` a word would become if manually flipped from
/// `on_screen_lang`, bypassing the dictionary check entirely.
fn flipped(keys: &[TypedKey], on_screen_lang: Lang) -> Option<(String, Lang)> {
    if keys.is_empty() {
        return None;
    }
    let target_lang = on_screen_lang.other();
    let on_screen_text: String = keys
        .iter()
        .filter_map(|k| keymap::char_for(k.code, k.shift, k.capslock, on_screen_lang))
        .collect();
    let target_text: String = keys
        .iter()
        .filter_map(|k| keymap::char_for(k.code, k.shift, k.capslock, target_lang))
        .collect();
    Some((
        apply_case_pattern(&on_screen_text, &target_text),
        target_lang,
    ))
}

/// Resolves one hotkey key name from config, warning and returning `None`
/// (disabling that hotkey) if it doesn't parse.
fn resolve_hotkey_key(field: &str, name: &str) -> Option<KeyCode> {
    let key = keymap::parse_modifier_key(name);
    if key.is_none() {
        tracing::warn!("invalid hotkeys.{field} '{name}', that hotkey is disabled");
    }
    key
}

/// Resolves a combo of hotkey key names into a modifier bitmask; 0 (and a
/// warning) if any of them don't parse, or the list is empty.
fn resolve_hotkey_mask(field: &str, names: &[String]) -> u8 {
    if names.is_empty() {
        return 0;
    }
    let mut mask = 0u8;
    for name in names {
        match keymap::parse_modifier_key(name).and_then(modifier_bit) {
            Some(bit) => mask |= bit,
            None => {
                tracing::warn!("invalid key '{name}' in hotkeys.{field}, that hotkey is disabled");
                return 0;
            }
        }
    }
    mask
}

/// Corrections never fire below this length, regardless of config: a single
/// letter is too likely to be a real initial, a loop variable, or list
/// marker, and matching it against either dictionary is close to a coin
/// flip.
const MIN_CORRECTION_LENGTH: usize = 2;

/// The text `keys` should be replaced with if they were typed in the wrong
/// layout (`current_lang` being the active one), or `None` to leave them.
fn correction(keys: &[TypedKey], current_lang: Lang, min_word_length: usize) -> Option<String> {
    let min_word_length = min_word_length.max(MIN_CORRECTION_LENGTH);
    let en_word: String = keys
        .iter()
        .filter_map(|k| keymap::char_for(k.code, k.shift, k.capslock, Lang::En))
        .collect();
    let ru_word: String = keys
        .iter()
        .filter_map(|k| keymap::char_for(k.code, k.shift, k.capslock, Lang::Ru))
        .collect();

    let other_lang = current_lang.other();
    let (typed_word, other_word) = match current_lang {
        Lang::En => (&en_word, &ru_word),
        Lang::Ru => (&ru_word, &en_word),
    };

    // Punctuation typed right after a word (e.g. the key that is `.` in
    // Russian but `/` in English) isn't part of the word, so look up each
    // interpretation without it. It is still retyped as part of the
    // corrected text below.
    let typed_core = trim_trailing_punctuation(typed_word);
    let other_core = trim_trailing_punctuation(other_word);
    if other_core.chars().count() < min_word_length {
        return None;
    }

    // Already a real word in the layout that's actually active: nothing
    // to do.
    if dictionary::contains(current_lang, typed_core) {
        return None;
    }
    // Only correct when the *other* interpretation is a real word - an
    // unrecognized word in both dictionaries is far more likely to be a
    // name, a command, or a typo than a layout mistake.
    if !dictionary::contains(other_lang, other_core) {
        return None;
    }

    let corrected = apply_case_pattern(typed_word, other_word);
    tracing::info!(from = %typed_word, to = %corrected, "autocorrecting keyboard layout");
    Some(corrected)
}

/// `word` without any non-letters at its end.
fn trim_trailing_punctuation(word: &str) -> &str {
    word.trim_end_matches(|c: char| !c.is_alphabetic())
}

/// `cfg.general.excluded_classes` must already be lowercased (see
/// `Engine::new`). A trailing `*` in a pattern matches as a prefix.
fn compute_excluded(class: &str, pid: i32, cfg: &Config) -> bool {
    let class_excluded = !class.is_empty()
        && cfg
            .general
            .excluded_classes
            .iter()
            .any(|pattern| steam::class_matches(class, pattern));
    if class_excluded {
        return true;
    }
    cfg.general.exclude_steam_games && steam::is_steam_process(pid)
}

/// The `cfg.layouts` matchers must already be lowercased (see
/// `Engine::new`).
fn detect_lang(desc: &str, cfg: &Config) -> Option<Lang> {
    let d = desc.to_lowercase();
    if cfg
        .layouts
        .english_match
        .iter()
        .any(|m| d.contains(m.as_str()))
    {
        return Some(Lang::En);
    }
    if cfg
        .layouts
        .russian_match
        .iter()
        .any(|m| d.contains(m.as_str()))
    {
        return Some(Lang::Ru);
    }
    None
}

/// Carries over the capitalization pattern of `source` (what was actually
/// typed) onto `target` (the corrected word): all-caps stays all-caps,
/// capitalized-first-letter stays capitalized, everything else is left
/// lowercase.
fn apply_case_pattern(source: &str, target: &str) -> String {
    let letters: Vec<char> = source.chars().filter(|c| c.is_alphabetic()).collect();
    let all_upper = !letters.is_empty() && letters.iter().all(|c| c.is_uppercase());
    if all_upper {
        return target.to_uppercase();
    }
    if source.chars().next().is_some_and(char::is_uppercase) {
        let mut chars = target.chars();
        return match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        };
    }
    target.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOWN: i32 = 1;
    const REPEAT: i32 = 2;

    fn keys(codes: &[KeyCode]) -> Vec<TypedKey> {
        codes
            .iter()
            .map(|&code| TypedKey {
                code,
                shift: false,
                capslock: false,
            })
            .collect()
    }

    // g h b d t n = "привет" on the Russian layout.
    const PRIVET: [KeyCode; 6] = [
        KeyCode::KEY_G,
        KeyCode::KEY_H,
        KeyCode::KEY_B,
        KeyCode::KEY_D,
        KeyCode::KEY_T,
        KeyCode::KEY_N,
    ];

    #[test]
    fn corrects_word_typed_in_wrong_layout() {
        assert_eq!(
            correction(&keys(&PRIVET), Lang::En, 3).as_deref(),
            Some("привет")
        );
    }

    #[test]
    fn corrects_word_with_trailing_punctuation() {
        // The `/` key is `.` on the Russian layout.
        let mut codes = PRIVET.to_vec();
        codes.push(KeyCode::KEY_SLASH);
        assert_eq!(
            correction(&keys(&codes), Lang::En, 3).as_deref(),
            Some("привет.")
        );
    }

    #[test]
    fn leaves_real_word_with_trailing_punctuation() {
        // "hello," in the English layout is fine as typed.
        let codes = [
            KeyCode::KEY_H,
            KeyCode::KEY_E,
            KeyCode::KEY_L,
            KeyCode::KEY_L,
            KeyCode::KEY_O,
            KeyCode::KEY_COMMA,
        ];
        assert_eq!(correction(&keys(&codes), Lang::En, 3), None);
    }

    #[test]
    fn google_typed_under_wrong_layout_is_corrected() {
        // g o o g l e physically pressed while a Russian layout is active
        // produces gibberish on screen, but should still resolve to the
        // English brand name - this is the case eager, per-keystroke
        // checking exists for: by the time Enter is pressed in a browser's
        // address bar, the word is already complete and this fires.
        let codes = [
            KeyCode::KEY_G,
            KeyCode::KEY_O,
            KeyCode::KEY_O,
            KeyCode::KEY_G,
            KeyCode::KEY_L,
            KeyCode::KEY_E,
        ];
        assert_eq!(
            correction(&keys(&codes), Lang::Ru, 3).as_deref(),
            Some("google")
        );
    }

    #[test]
    fn short_word_is_corrected_when_confirmed_complete() {
        // KEY_J -> "о", KEY_Y -> "н": physically typing "он" ("he") while
        // English is active produces the meaningless "jy" on screen. At a
        // word boundary (Space was just pressed) the word is known to be
        // complete, so even a 2-letter word like this can be trusted.
        let codes = [KeyCode::KEY_J, KeyCode::KEY_Y];
        assert_eq!(
            correction(&keys(&codes), Lang::En, 2).as_deref(),
            Some("он")
        );
    }

    #[test]
    fn short_word_is_not_corrected_eagerly() {
        // The same two letters, but checked with the (higher) eager
        // mid-word threshold: at only two letters in, there's no telling a
        // complete short word from the first two letters of a longer one,
        // so eager checking should hold off.
        let codes = [KeyCode::KEY_J, KeyCode::KEY_Y];
        assert_eq!(correction(&keys(&codes), Lang::En, 4), None);
    }

    #[test]
    fn single_letter_is_never_corrected_even_if_configured_to() {
        let codes = [KeyCode::KEY_J];
        assert_eq!(correction(&keys(&codes), Lang::En, 1), None);
    }

    #[test]
    fn word_over_the_limit_is_not_corrected() {
        let mut engine = Engine::new(Config::default());
        engine.last_lang = Some(Lang::En);
        for _ in 0..MAX_WORD_KEYS + 1 {
            engine.handle_key("kbd", KeyCode::KEY_G, DOWN);
        }
        assert!(engine.overflowed);
        assert!(engine.buffer.len() <= MAX_WORD_KEYS);
        engine.handle_key("kbd", KeyCode::KEY_SPACE, DOWN);
        assert!(!engine.overflowed);
        assert!(engine.buffer.is_empty());
    }

    #[test]
    fn unknown_keyboard_uses_most_recent_layout() {
        let mut engine = Engine::new(Config::default());
        for (keyboard, layout) in [
            ("a", "English (US)"),
            ("b", "Russian"),
            ("a", "English (US)"),
        ] {
            engine.handle_hypr(hypr::HyprEvent::ActiveLayout {
                keyboard: keyboard.to_string(),
                layout: layout.to_string(),
            });
        }
        assert_eq!(engine.lang_for_device(Some("c")), Some(Lang::En));
        assert_eq!(engine.lang_for_device(Some("b")), Some(Lang::Ru));
    }

    #[test]
    fn autorepeat_letter_extends_word() {
        let mut engine = Engine::new(Config::default());
        engine.handle_key("kbd", KeyCode::KEY_G, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_G, REPEAT);
        engine.handle_key("kbd", KeyCode::KEY_G, REPEAT);
        assert_eq!(engine.buffer.len(), 3);
    }

    #[test]
    fn autorepeat_backspace_keeps_deleting() {
        let mut engine = Engine::new(Config::default());
        for code in [
            KeyCode::KEY_G,
            KeyCode::KEY_H,
            KeyCode::KEY_B,
            KeyCode::KEY_D,
        ] {
            engine.handle_key("kbd", code, DOWN);
        }
        engine.handle_key("kbd", KeyCode::KEY_BACKSPACE, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_BACKSPACE, REPEAT);
        engine.handle_key("kbd", KeyCode::KEY_BACKSPACE, REPEAT);
        assert_eq!(engine.buffer.len(), 1);
    }

    #[test]
    fn releasing_one_ctrl_keeps_the_other_held() {
        let mut engine = Engine::new(Config::default());
        engine.handle_key("kbd", KeyCode::KEY_G, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_LEFTCTRL, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_RIGHTCTRL, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_RIGHTCTRL, 0);
        // Left Ctrl is still down, so this is Ctrl+H, not the letter h.
        engine.handle_key("kbd", KeyCode::KEY_H, DOWN);
        assert!(engine.buffer.is_empty());
    }

    #[test]
    fn releasing_one_shift_keeps_the_other_held() {
        let mut engine = Engine::new(Config::default());
        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_RIGHTSHIFT, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_RIGHTSHIFT, 0);
        assert!(engine.shift());
    }

    #[test]
    fn autorepeat_modifier_stays_held() {
        let mut engine = Engine::new(Config::default());
        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, REPEAT);
        assert!(engine.shift());
        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, 0);
        assert!(!engine.shift());
    }

    #[test]
    fn case_pattern_all_caps() {
        assert_eq!(apply_case_pattern("GHBDTN", "привет"), "ПРИВЕТ");
    }

    #[test]
    fn case_pattern_capitalized() {
        assert_eq!(apply_case_pattern("Ghbdtn", "привет"), "Привет");
    }

    #[test]
    fn case_pattern_lowercase() {
        assert_eq!(apply_case_pattern("ghbdtn", "привет"), "привет");
    }

    #[test]
    fn manual_flip_ignores_dictionary_validity() {
        // q x z is gibberish in both languages (like a name would be) -
        // exactly the case the dictionary-based corrector never touches,
        // but the manual flip hotkey should still work on.
        let codes = [KeyCode::KEY_Q, KeyCode::KEY_X, KeyCode::KEY_Z];
        let (text, lang) = flipped(&keys(&codes), Lang::En).unwrap();
        assert_eq!(lang, Lang::Ru);
        assert_eq!(text, "йчя");
        // And flipping back returns the original.
        let (back, lang2) = flipped(&keys(&codes), Lang::Ru).unwrap();
        assert_eq!(lang2, Lang::En);
        assert_eq!(back, "qxz");
    }

    #[test]
    fn manual_flip_preserves_capitalization() {
        let codes = [
            TypedKey {
                code: KeyCode::KEY_Q,
                shift: true,
                capslock: false,
            },
            TypedKey {
                code: KeyCode::KEY_X,
                shift: false,
                capslock: false,
            },
        ];
        let (text, _) = flipped(&codes, Lang::En).unwrap();
        assert_eq!(text, "Йч");
    }

    #[test]
    fn flipped_is_none_for_empty_word() {
        assert_eq!(flipped(&[], Lang::En), None);
    }

    const UP: i32 = 0;

    fn tap(engine: &mut Engine, code: KeyCode) {
        engine.handle_key("kbd", code, DOWN);
        engine.handle_key("kbd", code, UP);
    }

    #[test]
    fn both_shifts_together_toggle_pause() {
        let mut engine = Engine::new(Config::default());
        assert!(!engine.paused);
        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_RIGHTSHIFT, DOWN);
        assert!(!engine.paused, "the chord fires on release");
        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, UP);
        assert!(engine.paused, "both shifts together should toggle pause");
        engine.handle_key("kbd", KeyCode::KEY_RIGHTSHIFT, UP);
        assert!(engine.paused, "releasing the second key must not re-toggle");

        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_RIGHTSHIFT, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_RIGHTSHIFT, UP);
        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, UP);
        assert!(!engine.paused, "pressing both again should toggle back off");
    }

    #[test]
    fn toggle_pause_does_not_refire_on_autorepeat() {
        let mut engine = Engine::new(Config::default());
        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_RIGHTSHIFT, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_RIGHTSHIFT, REPEAT);
        engine.handle_key("kbd", KeyCode::KEY_RIGHTSHIFT, UP);
        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, UP);
        assert!(engine.paused);
    }

    #[test]
    fn overlapping_shifts_while_typing_capitals_do_not_pause() {
        // Rolling from right Shift ("A") to left Shift ("P") while typing
        // fast holds both Shifts at once for a moment, with letters in
        // between. That's typing, not the pause chord.
        let mut engine = Engine::new(Config::default());
        engine.handle_key("kbd", KeyCode::KEY_RIGHTSHIFT, DOWN);
        tap(&mut engine, KeyCode::KEY_A);
        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, DOWN);
        engine.handle_key("kbd", KeyCode::KEY_RIGHTSHIFT, UP);
        tap(&mut engine, KeyCode::KEY_P);
        engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, UP);
        assert!(!engine.paused);
    }

    #[test]
    fn double_tap_manual_correct_key_consumes_the_pair() {
        let mut cfg = Config::default();
        cfg.hotkeys.manual_correct_window_ms = 10_000; // avoid timing flakiness
        let mut engine = Engine::new(cfg);
        engine.last_lang = Some(Lang::En);
        assert!(engine.manual_correct_last_tap.is_none());

        // Tap 1: a single tap, not a trigger yet.
        tap(&mut engine, KeyCode::KEY_LEFTSHIFT);
        assert!(engine.manual_correct_last_tap.is_some());

        // Tap 2, within the window: this is the double-tap. The pair is
        // consumed (reset to None) so a third tap starts fresh rather than
        // firing again immediately.
        tap(&mut engine, KeyCode::KEY_LEFTSHIFT);
        assert!(engine.manual_correct_last_tap.is_none());

        // Tap 3: starts a new pair.
        tap(&mut engine, KeyCode::KEY_LEFTSHIFT);
        assert!(engine.manual_correct_last_tap.is_some());
    }

    #[test]
    fn shift_used_for_a_capital_is_not_a_tap() {
        let mut cfg = Config::default();
        cfg.hotkeys.manual_correct_window_ms = 10_000;
        let mut engine = Engine::new(cfg);
        // Shift+H, then Shift+I: two Shift presses, but both typed letters.
        for letter in [KeyCode::KEY_H, KeyCode::KEY_I] {
            engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, DOWN);
            tap(&mut engine, letter);
            engine.handle_key("kbd", KeyCode::KEY_LEFTSHIFT, UP);
            assert!(engine.manual_correct_last_tap.is_none());
        }
    }

    #[test]
    fn a_key_between_taps_breaks_the_double_tap() {
        let mut cfg = Config::default();
        cfg.hotkeys.manual_correct_window_ms = 10_000;
        let mut engine = Engine::new(cfg);
        tap(&mut engine, KeyCode::KEY_LEFTSHIFT);
        tap(&mut engine, KeyCode::KEY_A);
        assert!(engine.manual_correct_last_tap.is_none());
    }

    #[test]
    fn last_word_tracks_spaces_typed_after_it() {
        let mut engine = Engine::new(Config::default());
        engine.last_lang = Some(Lang::En);
        // "qxz" is not a word in either language, so it stays as typed.
        for code in [KeyCode::KEY_Q, KeyCode::KEY_X, KeyCode::KEY_Z] {
            tap(&mut engine, code);
        }
        tap(&mut engine, KeyCode::KEY_SPACE);
        assert_eq!(engine.last_word.as_ref().unwrap().trailing_spaces, 1);
        tap(&mut engine, KeyCode::KEY_SPACE);
        assert_eq!(engine.last_word.as_ref().unwrap().trailing_spaces, 2);
        tap(&mut engine, KeyCode::KEY_BACKSPACE);
        tap(&mut engine, KeyCode::KEY_BACKSPACE);
        assert_eq!(engine.last_word.as_ref().unwrap().trailing_spaces, 0);
        // One more Backspace deletes into the word itself.
        tap(&mut engine, KeyCode::KEY_BACKSPACE);
        assert!(engine.last_word.is_none());
    }

    #[test]
    fn last_word_is_dropped_after_enter() {
        let mut engine = Engine::new(Config::default());
        engine.last_lang = Some(Lang::En);
        for code in [KeyCode::KEY_Q, KeyCode::KEY_X, KeyCode::KEY_Z] {
            tap(&mut engine, code);
        }
        tap(&mut engine, KeyCode::KEY_SPACE);
        assert!(engine.last_word.is_some());
        tap(&mut engine, KeyCode::KEY_ENTER);
        assert!(engine.last_word.is_none());

        for code in [KeyCode::KEY_Q, KeyCode::KEY_X, KeyCode::KEY_Z] {
            tap(&mut engine, code);
        }
        tap(&mut engine, KeyCode::KEY_ENTER);
        assert!(engine.last_word.is_none());
    }

    #[test]
    fn pause_hotkey_disabled_when_hotkeys_disabled() {
        let mut cfg = Config::default();
        cfg.hotkeys.enabled = false;
        let engine = Engine::new(cfg);
        assert_eq!(engine.toggle_pause_mask, 0);
        assert_eq!(engine.manual_correct_key, None);
    }

    #[test]
    fn control_toggle_flips_paused_and_reports_state() {
        let mut engine = Engine::new(Config::default());
        engine.last_lang = Some(Lang::Ru);
        let resp = engine.handle_control(control::ControlRequest::Toggle);
        assert!(resp.paused);
        assert_eq!(resp.lang.as_deref(), Some("ru"));
        let resp = engine.handle_control(control::ControlRequest::Status);
        assert!(resp.paused);
        let resp = engine.handle_control(control::ControlRequest::Resume);
        assert!(!resp.paused);
    }
}
