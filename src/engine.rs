//! Core decision logic: buffers keystrokes into words, and on each word
//! boundary decides whether the word was typed in the wrong layout.

use crate::config::Config;
use crate::keymap::{self, Lang};
use crate::{dictionary, hypr, typer};
use evdev::KeyCode;
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct TypedKey {
    code: KeyCode,
    shift: bool,
    capslock: bool,
}

pub struct Engine {
    cfg: Config,
    buffer: Vec<TypedKey>,
    shift_l: bool,
    shift_r: bool,
    ctrl: bool,
    alt: bool,
    meta: bool,
    capslock: bool,
    current_class: String,
    /// Cached `excluded_classes` check for `current_class`, refreshed only
    /// when focus changes rather than on every keystroke.
    excluded: bool,
    /// Hyprland keyboard device name -> last known active language.
    device_lang: HashMap<String, Lang>,
    keyboards: Vec<hypr::KeyboardDevice>,
    /// evdev device name -> resolved Hyprland device name. The keyboard list
    /// is fixed after startup, so each name only has to be resolved once.
    resolved_devices: HashMap<String, Option<String>>,
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

        let keyboards = hypr::get_keyboards().unwrap_or_else(|e| {
            tracing::warn!("could not query hyprctl devices at startup: {e:#}");
            Vec::new()
        });
        let mut device_lang = HashMap::new();
        for kb in &keyboards {
            if let Some(lang) = detect_lang(&kb.active_keymap, &cfg) {
                device_lang.insert(kb.name.clone(), lang);
            }
        }
        let current_class = hypr::get_active_window_class()
            .ok()
            .flatten()
            .unwrap_or_default();
        let excluded = is_excluded(&current_class, &cfg);

        Self {
            cfg,
            buffer: Vec::new(),
            shift_l: false,
            shift_r: false,
            ctrl: false,
            alt: false,
            meta: false,
            capslock: false,
            current_class,
            excluded,
            device_lang,
            keyboards,
            resolved_devices: HashMap::new(),
        }
    }

    fn shift(&self) -> bool {
        self.shift_l || self.shift_r
    }

    pub fn handle_hypr(&mut self, ev: hypr::HyprEvent) {
        match ev {
            hypr::HyprEvent::ActiveWindow { class } => {
                if class != self.current_class {
                    self.excluded = is_excluded(&class, &self.cfg);
                    self.current_class = class;
                    self.buffer.clear();
                }
            }
            hypr::HyprEvent::ActiveLayout { keyboard, layout } => {
                if let Some(lang) = detect_lang(&layout, &self.cfg) {
                    tracing::debug!(keyboard = %keyboard, layout = %layout, ?lang, "layout changed");
                    self.device_lang.insert(keyboard, lang);
                }
            }
        }
    }

    /// `value`: 0 = key up, 1 = key down, 2 = autorepeat.
    pub fn handle_key(&mut self, source_device: &str, code: KeyCode, value: i32) {
        if value == 2 {
            return; // autorepeat doesn't add new characters we care about
        }

        match code {
            KeyCode::KEY_LEFTSHIFT => {
                self.shift_l = value == 1;
                return;
            }
            KeyCode::KEY_RIGHTSHIFT => {
                self.shift_r = value == 1;
                return;
            }
            KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL => {
                self.ctrl = value == 1;
                return;
            }
            KeyCode::KEY_LEFTALT | KeyCode::KEY_RIGHTALT => {
                self.alt = value == 1;
                return;
            }
            KeyCode::KEY_LEFTMETA | KeyCode::KEY_RIGHTMETA => {
                self.meta = value == 1;
                return;
            }
            KeyCode::KEY_CAPSLOCK => {
                if value == 1 {
                    self.capslock = !self.capslock;
                }
                return;
            }
            _ => {}
        }

        if value != 1 {
            return; // everything below only reacts to key-down
        }

        if !self.cfg.general.enabled || self.excluded {
            self.buffer.clear();
            return;
        }

        if code == KeyCode::KEY_BACKSPACE {
            self.buffer.pop();
            return;
        }

        if self.ctrl || self.alt || self.meta {
            // A shortcut (Ctrl+C, Alt+Tab, Super+...), not text. Whatever
            // was buffered before it is no longer contiguous on screen.
            self.buffer.clear();
            return;
        }

        if keymap::is_word_key(code) {
            self.buffer.push(TypedKey {
                code,
                shift: self.shift(),
                capslock: self.capslock,
            });
            return;
        }

        // Space, enter, tab, digits, punctuation, arrows, etc. all end the
        // current word, but only a space is checked for a correction. By the
        // time we see the key the application has already acted on it, so
        // fixing the word means deleting and retyping that key too. That is
        // safe for a space; Enter may already have sent a message, Tab moved
        // focus and arrows moved the cursor away from the word.
        if code == KeyCode::KEY_SPACE {
            self.finalize(source_device, " ");
        } else {
            self.buffer.clear();
        }
    }

    /// `boundary` is the text the word-ending key has already put on screen
    /// after the word; it is deleted and retyped along with the correction.
    fn finalize(&mut self, source_device: &str, boundary: &str) {
        let keys = std::mem::take(&mut self.buffer);
        if keys.len() < self.cfg.general.min_word_length {
            return;
        }

        let device_name = self.resolve_hypr_device_name(source_device);
        let Some(current_lang) = self.lang_for_device(device_name.as_deref()) else {
            tracing::debug!("no known active layout yet, skipping correction check");
            return;
        };

        let en_word: String = keys
            .iter()
            .filter_map(|k| keymap::char_for(k.code, k.shift, k.capslock, Lang::En))
            .collect();
        let ru_word: String = keys
            .iter()
            .filter_map(|k| keymap::char_for(k.code, k.shift, k.capslock, Lang::Ru))
            .collect();

        let (typed_word, other_word, other_lang) = match current_lang {
            Lang::En => (&en_word, &ru_word, Lang::Ru),
            Lang::Ru => (&ru_word, &en_word, Lang::En),
        };

        // Already a real word in the layout that's actually active: nothing
        // to do.
        if dictionary::contains(current_lang, typed_word) {
            return;
        }
        // Only correct when the *other* interpretation is a real word - an
        // unrecognized word in both dictionaries is far more likely to be a
        // name, a command, or a typo than a layout mistake.
        if !dictionary::contains(other_lang, other_word) {
            return;
        }

        let corrected = apply_case_pattern(typed_word, other_word);
        tracing::info!(from = %typed_word, to = %corrected, "autocorrecting keyboard layout");

        let backspaces = keys.len() + boundary.chars().count();
        if let Err(e) = typer::correct_word(backspaces, &format!("{corrected}{boundary}")) {
            tracing::warn!("failed to retype corrected word: {e:#}");
            return;
        }

        match device_name {
            Some(device_name) => {
                let index = match other_lang {
                    Lang::En => self.cfg.layouts.english_index,
                    Lang::Ru => self.cfg.layouts.russian_index,
                };
                match hypr::switch_layout(&device_name, index) {
                    Ok(()) => {
                        self.device_lang.insert(device_name, other_lang);
                    }
                    Err(e) => tracing::warn!("failed to switch layout: {e:#}"),
                }
            }
            None => tracing::warn!(
                "could not resolve a hyprland keyboard device for '{source_device}'; \
                 set `hypr.device_name_override` in the config"
            ),
        }
    }

    fn lang_for_device(&self, device_name: Option<&str>) -> Option<Lang> {
        if let Some(name) = device_name
            && let Some(lang) = self.device_lang.get(name)
        {
            return Some(*lang);
        }
        // Fall back to whatever layout we've most recently observed on any
        // keyboard - better than nothing on unusual multi-keyboard setups.
        self.device_lang.values().next().copied()
    }

    /// Hyprland slugifies libinput device names (lowercase, spaces -> `-`)
    /// for its own device identifiers; evdev gives us the human-readable
    /// name. Reproduce that transform to match them up.
    fn resolve_hypr_device_name(&mut self, evdev_name: &str) -> Option<String> {
        if let Some(o) = &self.cfg.hypr.device_name_override {
            return Some(o.clone());
        }
        if let Some(cached) = self.resolved_devices.get(evdev_name) {
            return cached.clone();
        }
        let resolved = self.match_hypr_device_name(evdev_name);
        self.resolved_devices
            .insert(evdev_name.to_string(), resolved.clone());
        resolved
    }

    fn match_hypr_device_name(&self, evdev_name: &str) -> Option<String> {
        let slug = evdev_name.to_lowercase().replace(' ', "-");
        if let Some(kb) = self
            .keyboards
            .iter()
            .find(|kb| kb.name == slug || kb.name.contains(&slug) || slug.contains(&kb.name))
        {
            return Some(kb.name.clone());
        }
        self.keyboards
            .iter()
            .find(|k| k.main)
            .or_else(|| self.keyboards.first())
            .map(|k| k.name.clone())
    }
}

/// `cfg.general.excluded_classes` must already be lowercased (see
/// `Engine::new`).
fn is_excluded(class: &str, cfg: &Config) -> bool {
    if class.is_empty() {
        return false;
    }
    let class = class.to_lowercase();
    cfg.general.excluded_classes.contains(&class)
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
