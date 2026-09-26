//! Core decision logic: buffers keystrokes into words, and on each word
//! boundary decides whether the word was typed in the wrong layout.

use crate::config::Config;
use crate::keymap::{self, Lang};
use crate::{dictionary, hypr, steam, typer};
use evdev::KeyCode;
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct TypedKey {
    code: KeyCode,
    shift: bool,
    capslock: bool,
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
    /// Currently held modifier keys, as `LEFT_SHIFT | ...` bits.
    modifiers: u8,
    capslock: bool,
    current_class: String,
    /// Cached exclusion check (window class + Steam detection) for
    /// `current_class`, refreshed only when focus changes rather than on
    /// every keystroke.
    excluded: bool,
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

        let (current_class, excluded) = match hypr::get_active_window().ok().flatten() {
            Some(win) => {
                let excluded = compute_excluded(&win.class, win.pid, &cfg);
                (win.class, excluded)
            }
            None => (String::new(), false),
        };

        let mut engine = Self {
            cfg,
            buffer: Vec::new(),
            overflowed: false,
            modifiers: 0,
            capslock: false,
            current_class,
            excluded,
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
                return;
            }
        };
        let (class, pid) = match active {
            Some(win) => (win.class, win.pid),
            None => (String::new(), 0),
        };
        if class != self.current_class {
            self.excluded = compute_excluded(&class, pid, &self.cfg);
            self.current_class = class;
            self.reset_word();
        }
    }

    fn reset_word(&mut self) {
        self.buffer.clear();
        self.overflowed = false;
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

        if let Some(bit) = modifier_bit(code) {
            if held {
                self.modifiers |= bit;
            } else {
                self.modifiers &= !bit;
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

        if !self.cfg.general.enabled || self.excluded {
            self.reset_word();
            return;
        }

        if code == KeyCode::KEY_BACKSPACE {
            self.buffer.pop();
            return;
        }

        if self.modifiers & SHORTCUT != 0 {
            // A shortcut (Ctrl+C, Alt+Tab, Super+...), not text. Whatever
            // was buffered before it is no longer contiguous on screen.
            self.reset_word();
            return;
        }

        if keymap::is_word_key(code) {
            if self.overflowed {
                return;
            }
            if self.buffer.len() >= MAX_WORD_KEYS {
                self.buffer.clear();
                self.overflowed = true;
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
            if self.cfg.general.eager_correction {
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
        if code == KeyCode::KEY_SPACE {
            self.try_correct(source_device, " ", self.cfg.general.min_word_length);
        }
        self.reset_word();
    }

    /// Checks the current buffer for a wrong-layout word and, if found,
    /// retypes it - plus `boundary`, text a word-ending key may have
    /// already put on screen - and switches the layout. Returns whether a
    /// correction was made; only then is the buffer cleared, since a caller
    /// still mid-word (the eager path) needs to keep accumulating
    /// otherwise.
    fn try_correct(&mut self, source_device: &str, boundary: &str, min_length: usize) -> bool {
        if self.overflowed {
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

        let backspaces = self.buffer.len() + boundary.chars().count();
        if let Err(e) = typer::correct_word(backspaces, &format!("{corrected}{boundary}")) {
            tracing::warn!("failed to retype corrected word: {e:#}");
            return false;
        }
        // Whatever was typed is gone now regardless of whether the layout
        // switch below succeeds - don't let a stale buffer trigger another
        // correction on top of this one.
        self.reset_word();

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
}

/// The text `keys` should be replaced with if they were typed in the wrong
/// layout (`current_lang` being the active one), or `None` to leave them.
fn correction(keys: &[TypedKey], current_lang: Lang, min_word_length: usize) -> Option<String> {
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
}
