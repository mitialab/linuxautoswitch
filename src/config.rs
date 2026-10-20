use anyhow::Context;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub layouts: Layouts,
    pub hypr: HyprCfg,
    pub hotkeys: Hotkeys,
    pub timing: Timing,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct General {
    pub enabled: bool,
    /// Minimum length for a correction triggered by pressing Space after a
    /// word. Kept lower than `eager_min_word_length` on purpose: by the time
    /// Space is pressed the word is confirmed *complete* (nothing more is
    /// coming), so a short word like "да" or "но" can be trusted the same
    /// as a long one. `eager_min_word_length` below is for mid-word
    /// checking, where a short buffer might still just be the first two
    /// letters of something longer. 1-letter words are never corrected
    /// regardless of this setting - too easy to get a spurious hit, and too
    /// disruptive when they're a real single-letter word, an initial, or an
    /// abbreviation.
    pub min_word_length: usize,
    /// Window classes (as reported by `hyprctl activewindow`) to never
    /// touch: terminals, password managers, anywhere retyping text is
    /// dangerous or where non-language gibberish (commands, passwords) is
    /// expected. A trailing `*` matches as a prefix, e.g. `"steam_app_*"`.
    pub excluded_classes: Vec<String>,
    /// Skip Steam games entirely, detected via the `SteamAppId` environment
    /// variable Steam sets on every game it launches (native or Proton) -
    /// far more reliable than window classes, which vary per game.
    pub exclude_steam_games: bool,
    /// Check for a correction after every keystroke instead of only at
    /// word boundaries (space/enter/tab/...). Needed for text that's acted
    /// on without ever hitting a boundary key, e.g. a browser address bar
    /// where you type a URL and immediately press Enter, or click a
    /// suggestion. Holds back while the typed text is the start of a longer
    /// word in the active layout, so a half-typed word isn't mistaken for a
    /// whole word in the other language - see README.
    pub eager_correction: bool,
    /// Minimum length for an eager (mid-word) correction. Kept higher than
    /// `min_word_length` by default since short coincidental matches are
    /// more likely, and a premature short correction is more disruptive
    /// than waiting a couple more keystrokes.
    pub eager_min_word_length: usize,
    /// Also watch mice and touchpads, so that a click (which moves the
    /// cursor somewhere the typed word no longer is) drops the word being
    /// tracked instead of letting a later correction delete the wrong text.
    /// Only button presses are read, never movement.
    pub reset_on_click: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            enabled: true,
            min_word_length: 2,
            excluded_classes: [
                "kitty",
                "foot",
                "Alacritty",
                "wezterm",
                "org.keepassxc.KeePassXC",
                "1Password",
                "Bitwarden",
                "steam",
                "steam_app_*",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            exclude_steam_games: true,
            eager_correction: true,
            eager_min_word_length: 4,
            reset_on_click: true,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Layouts {
    /// Substrings (case-insensitive) matched against Hyprland's
    /// `active_keymap` description to recognize the English layout.
    pub english_match: Vec<String>,
    /// Same, for Russian.
    pub russian_match: Vec<String>,
    /// Index of each layout in your Hyprland `kb_layout` list (0-based).
    /// E.g. `kb_layout = us,ru` -> `english_index = 0`, `russian_index = 1`.
    pub english_index: u32,
    pub russian_index: u32,
}

impl Default for Layouts {
    fn default() -> Self {
        Self {
            english_match: vec!["English".to_string()],
            russian_match: vec!["Russian".to_string()],
            english_index: 0,
            russian_index: 1,
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct HyprCfg {
    /// Force a specific Hyprland keyboard device name instead of
    /// auto-detecting it from the evdev device name. Only needed with
    /// multiple keyboards or if auto-detection picks the wrong one; find the
    /// right value with `hyprctl devices | grep -A2 Keyboard`.
    pub device_name_override: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Hotkeys {
    pub enabled: bool,
    /// Key to double-tap to manually flip the last word to the other
    /// language, bypassing the dictionary check entirely - for names,
    /// jargon, or anything the automatic detection didn't recognize.
    /// One of: leftshift, rightshift, leftctrl, rightctrl, leftalt,
    /// rightalt, leftmeta/leftsuper, rightmeta/rightsuper.
    pub manual_correct_key: String,
    /// Maximum gap between the two taps, in milliseconds.
    pub manual_correct_window_ms: u64,
    /// Keys that, held down together, toggle the whole daemon
    /// paused/running - same key names as `manual_correct_key` above.
    pub toggle_pause_keys: Vec<String>,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            enabled: true,
            manual_correct_key: "leftshift".to_string(),
            manual_correct_window_ms: 400,
            toggle_pause_keys: vec!["leftshift".to_string(), "rightshift".to_string()],
        }
    }
}

/// How a correction is carried out. The defaults wait for nothing: the daemon
/// only reads keys, it cannot hold them back, so every millisecond spent
/// waiting is a millisecond in which the next thing you type reaches the
/// application first. Raise a delay only for an app that visibly loses the
/// start of a retyped word.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Timing {
    /// Pause between switching the layout and typing the correction.
    pub delay_after_layout_switch_ms: u64,
    /// Pause between the Backspaces and the retyped word (inside one
    /// `wtype` call). Helps slow apps that process deletions lazily.
    pub delay_before_retype_ms: u64,
    /// After retyping, the active layout is read back from Hyprland; if it is
    /// not the one asked for, the switch is repeated up to this many times.
    pub layout_switch_retries: u32,
    /// Wait before each repeated switch.
    pub layout_switch_retry_delay_ms: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            delay_after_layout_switch_ms: 0,
            delay_before_retype_ms: 0,
            layout_switch_retries: 2,
            layout_switch_retry_delay_ms: 50,
        }
    }
}

pub fn default_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string())).join(".config")
        });
    base.join("linuxautoswitch").join("config.toml")
}

/// The directory holding `config.toml` (and the user's word lists).
pub fn dir(path: Option<&std::path::Path>) -> PathBuf {
    let path = path.map(PathBuf::from).unwrap_or_else(default_path);
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

pub fn load(path: Option<PathBuf>) -> anyhow::Result<Config> {
    let path = path.unwrap_or_else(default_path);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let cfg = toml::from_str(&text)
                .map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))?;
            tracing::info!("loaded config from {}", path.display());
            Ok(cfg)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(
                "no config file at {}, using built-in defaults",
                path.display()
            );
            Ok(Config::default())
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example file is what `install.sh` copies for new users, so its
    /// values must be the built-in defaults - otherwise the documented
    /// behavior and the behavior without a config file quietly diverge.
    #[test]
    fn example_config_matches_the_built_in_defaults() {
        let example: Config = toml::from_str(include_str!("../assets/config.example.toml"))
            .expect("assets/config.example.toml parses");
        let default = Config::default();
        assert_eq!(
            example.general.reset_on_click,
            default.general.reset_on_click
        );
        assert_eq!(
            example.general.eager_min_word_length,
            default.general.eager_min_word_length
        );
        let (a, b) = (&example.timing, &default.timing);
        assert_eq!(
            a.delay_after_layout_switch_ms,
            b.delay_after_layout_switch_ms
        );
        assert_eq!(a.delay_before_retype_ms, b.delay_before_retype_ms);
        assert_eq!(a.layout_switch_retries, b.layout_switch_retries);
        assert_eq!(
            a.layout_switch_retry_delay_ms,
            b.layout_switch_retry_delay_ms
        );
    }
}
