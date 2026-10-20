//! Words and rules that belong to the person rather than to the bundled
//! dictionaries: their own vocabulary (names, jargon, project terms), loaded
//! from plain text files next to `config.toml`.
//!
//! - `words-en.txt` / `words-ru.txt` - one word per line, `#` starts a
//!   comment. A word listed here counts as a real word of that language, so
//!   it is neither "fixed" when typed in its own layout nor mistaken for
//!   gibberish when it is typed in the other one.
//!
//! - `typos-en.txt` / `typos-ru.txt` - your own typo fixes, one `wrong right`
//!   pair per line (the right side may be several words). Used by the
//!   `typo_correction` experimental feature, on top of the built-in table.
//!
//! The files are read once at startup; restart the daemon after editing.

use crate::keymap::Lang;
use std::collections::{BTreeSet, HashMap};
use std::ops::Bound;
use std::path::Path;

#[derive(Default)]
pub struct UserData {
    en: BTreeSet<String>,
    ru: BTreeSet<String>,
    typos_en: HashMap<String, String>,
    typos_ru: HashMap<String, String>,
}

impl UserData {
    /// Reads the word lists from `dir`. A missing file is just an empty list;
    /// an unreadable one is logged and skipped, never fatal.
    pub fn load(dir: &Path) -> Self {
        let read = |name: &str| match std::fs::read_to_string(dir.join(name)) {
            Ok(text) => {
                let words = parse_word_list(&text);
                tracing::info!(file = name, words = words.len(), "loaded user word list");
                words
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(e) => {
                tracing::warn!(file = name, "could not read user word list: {e}");
                BTreeSet::new()
            }
        };
        let read_typos = |name: &str| match std::fs::read_to_string(dir.join(name)) {
            Ok(text) => {
                let typos = parse_typo_list(&text);
                tracing::info!(file = name, typos = typos.len(), "loaded user typo list");
                typos
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                tracing::warn!(file = name, "could not read user typo list: {e}");
                HashMap::new()
            }
        };
        Self {
            en: read("words-en.txt"),
            ru: read("words-ru.txt"),
            typos_en: read_typos("typos-en.txt"),
            typos_ru: read_typos("typos-ru.txt"),
        }
    }

    /// The person's own fix for the misspelling `word` in `lang`, if any.
    /// Case-insensitive.
    pub fn typo(&self, lang: Lang, word: &str) -> Option<&str> {
        let typos = match lang {
            Lang::En => &self.typos_en,
            Lang::Ru => &self.typos_ru,
        };
        if typos.is_empty() {
            return None;
        }
        typos.get(&word.to_lowercase()).map(String::as_str)
    }

    fn words(&self, lang: Lang) -> &BTreeSet<String> {
        match lang {
            Lang::En => &self.en,
            Lang::Ru => &self.ru,
        }
    }

    /// Whether `word` is in the person's own `lang` list. Case-insensitive.
    pub fn knows(&self, lang: Lang, word: &str) -> bool {
        let words = self.words(lang);
        !words.is_empty() && words.contains(&word.to_lowercase())
    }

    /// Whether a listed word is strictly longer than `prefix` and starts
    /// with it. Case-insensitive.
    pub fn has_longer_word(&self, lang: Lang, prefix: &str) -> bool {
        let words = self.words(lang);
        if words.is_empty() {
            return false;
        }
        let prefix = prefix.to_lowercase();
        words
            .range::<str, _>((Bound::Included(prefix.as_str()), Bound::Unbounded))
            .take_while(|w| w.starts_with(&prefix))
            .any(|w| w.len() > prefix.len())
    }
}

/// `wrong right...` pairs, one per line; `#` starts a comment.
pub fn parse_typo_list(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter_map(|line| {
            let (wrong, right) = line.split_once(char::is_whitespace)?;
            let right = right.trim();
            (!right.is_empty()).then(|| (wrong.to_lowercase(), right.to_string()))
        })
        .collect()
}

fn parse_word_list(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(en: &str, ru: &str) -> UserData {
        UserData {
            en: parse_word_list(en),
            ru: parse_word_list(ru),
            ..UserData::default()
        }
    }

    #[test]
    fn parses_words_comments_and_blank_lines() {
        let list = parse_word_list("Hyprland\n\n# a comment\n  Omarchy  # trailing note\n");
        assert_eq!(
            list.into_iter().collect::<Vec<_>>(),
            ["hyprland", "omarchy"]
        );
    }

    #[test]
    fn lookup_is_case_insensitive_and_per_language() {
        let user = data("Hyprland\n", "Омархи\n");
        assert!(user.knows(Lang::En, "HYPRLAND"));
        assert!(user.knows(Lang::Ru, "омархи"));
        assert!(!user.knows(Lang::Ru, "hyprland"));
        assert!(!user.knows(Lang::En, "waybar"));
    }

    #[test]
    fn prefix_lookup_needs_a_strictly_longer_word() {
        let user = data("hyprland\nhypr\n", "");
        assert!(user.has_longer_word(Lang::En, "hypr")); // hyprland
        assert!(user.has_longer_word(Lang::En, "HYP"));
        assert!(!user.has_longer_word(Lang::En, "hyprland"));
        assert!(!user.has_longer_word(Lang::En, "wayb"));
        assert!(!user.has_longer_word(Lang::Ru, "hypr"));
    }

    #[test]
    fn typo_lists_map_a_wrong_word_to_the_right_text() {
        let typos = parse_typo_list("# mine\nTeh   the\nvobshem  в общем  # two words\njunk\n\n");
        assert_eq!(typos.len(), 2);
        assert_eq!(typos["teh"], "the");
        assert_eq!(typos["vobshem"], "в общем");
    }

    #[test]
    fn missing_files_are_an_empty_list() {
        let dir = std::env::temp_dir().join("linuxautoswitch-no-such-dir-for-tests");
        let user = UserData::load(&dir);
        assert!(!user.knows(Lang::En, "anything"));
    }

    #[test]
    fn loads_files_from_a_directory() {
        let dir = std::env::temp_dir().join(format!("las-userdata-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("words-en.txt"), "Quickshell\n").unwrap();
        std::fs::write(dir.join("words-ru.txt"), "# nothing yet\nомарчи\n").unwrap();
        let user = UserData::load(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(user.knows(Lang::En, "quickshell"));
        assert!(user.knows(Lang::Ru, "Омарчи"));
    }
}
