use anyhow::Context;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub layouts: Layouts,
    pub hypr: HyprCfg,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct General {
    pub enabled: bool,
    /// Words shorter than this are never auto-corrected: too easy to get a
    /// spurious dictionary hit on 1-2 letter "words".
    pub min_word_length: usize,
    /// Window classes (as reported by `hyprctl activewindow`) to never
    /// touch: terminals, password managers, anywhere retyping text is
    /// dangerous or where non-language gibberish (commands, passwords) is
    /// expected.
    pub excluded_classes: Vec<String>,
}

impl Default for General {
    fn default() -> Self {
        Self {
            enabled: true,
            min_word_length: 3,
            excluded_classes: [
                "kitty",
                "foot",
                "Alacritty",
                "wezterm",
                "org.keepassxc.KeePassXC",
                "1Password",
                "Bitwarden",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
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

pub fn default_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string())).join(".config")
        });
    base.join("linuxautoswitch").join("config.toml")
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
