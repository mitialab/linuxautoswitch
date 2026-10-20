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
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::ops::Bound;
use std::path::Path;
use std::path::PathBuf;

#[derive(Default)]
pub struct UserData {
    en: BTreeSet<String>,
    ru: BTreeSet<String>,
    typos_en: HashMap<String, String>,
    typos_ru: HashMap<String, String>,
    learned: Option<Learned>,
}

/// Text the person undid an automatic correction of: typed in this layout,
/// it is left alone from now on. Persisted as `en<TAB>word` lines.
struct Learned {
    path: PathBuf,
    rejected: HashSet<(Lang, String)>,
}

/// Stop learning past this many words, so the file and the set stay small.
const MAX_LEARNED: usize = 5000;
/// Shorter than this is too likely to be a coincidence, not a decision.
const MIN_LEARNED_LETTERS: usize = 3;

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
            learned: None,
        }
    }

    /// Turns on learning from undone corrections, kept in `path`. Existing
    /// lines are loaded; a missing file is fine.
    pub fn with_learning(mut self, path: PathBuf) -> Self {
        let rejected = match std::fs::read_to_string(&path) {
            Ok(text) => parse_learned(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(e) => {
                tracing::warn!(path = %path.display(), "could not read learned words: {e}");
                HashSet::new()
            }
        };
        tracing::info!(words = rejected.len(), "learning from undone corrections");
        self.learned = Some(Learned { path, rejected });
        self
    }

    /// Whether the person undid a correction of `word` typed in `lang`.
    /// Always false unless learning is on.
    pub fn is_rejected(&self, lang: Lang, word: &str) -> bool {
        self.learned.as_ref().is_some_and(|l| {
            !l.rejected.is_empty() && l.rejected.contains(&(lang, word.to_lowercase()))
        })
    }

    /// Remembers that `word`, typed in `lang`, should not be corrected.
    /// Returns whether it was newly learned (and written to disk).
    pub fn reject(&mut self, lang: Lang, word: &str) -> bool {
        let Some(learned) = &mut self.learned else {
            return false;
        };
        let word = word.to_lowercase();
        if word.chars().filter(|c| c.is_alphabetic()).count() < MIN_LEARNED_LETTERS
            || word
                .chars()
                .any(|c| c.is_whitespace() || !c.is_alphabetic() && !"'-".contains(c))
            || learned.rejected.len() >= MAX_LEARNED
            || !learned.rejected.insert((lang, word.clone()))
        {
            return false;
        }
        let tag = match lang {
            Lang::En => "en",
            Lang::Ru => "ru",
        };
        let written = learned
            .path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&learned.path)
            })
            .and_then(|mut file| writeln!(file, "{tag}\t{word}"));
        if let Err(e) = written {
            tracing::warn!(path = %learned.path.display(), "could not save learned word: {e}");
        }
        true
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

fn parse_learned(text: &str) -> HashSet<(Lang, String)> {
    text.lines()
        .filter_map(|line| {
            let (tag, word) = line.split_once('\t')?;
            let lang = match tag {
                "en" => Lang::En,
                "ru" => Lang::Ru,
                _ => return None,
            };
            let word = word.trim().to_lowercase();
            (!word.is_empty()).then_some((lang, word))
        })
        .collect()
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

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("las-learned-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("learned.txt")
    }

    #[test]
    fn learned_words_are_saved_and_reloaded() {
        let path = temp_file("roundtrip");
        let mut user = UserData::default().with_learning(path.clone());
        assert!(!user.is_rejected(Lang::En, "ghbdtn"));
        assert!(user.reject(Lang::En, "Ghbdtn"));
        assert!(user.is_rejected(Lang::En, "GHBDTN"));
        assert!(!user.is_rejected(Lang::Ru, "ghbdtn")); // per layout
        assert!(!user.reject(Lang::En, "ghbdtn")); // already known: no duplicate line

        let reloaded = UserData::default().with_learning(path.clone());
        assert!(reloaded.is_rejected(Lang::En, "ghbdtn"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "en\tghbdtn\n");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn nothing_is_learned_or_applied_when_learning_is_off() {
        let mut user = UserData::default();
        assert!(!user.reject(Lang::En, "ghbdtn"));
        assert!(!user.is_rejected(Lang::En, "ghbdtn"));
    }

    #[test]
    fn short_or_odd_text_is_not_learned() {
        let path = temp_file("guards");
        let mut user = UserData::default().with_learning(path.clone());
        for word in ["ab", "x", "", "a b c", "abc123", "ab.", "две слова"] {
            assert!(!user.reject(Lang::En, word), "{word:?}");
        }
        assert!(user.reject(Lang::En, "don't"));
        assert!(user.reject(Lang::Ru, "кто-то"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn learned_file_ignores_junk_lines() {
        let parsed = parse_learned("en\tfoo\nxx\tbar\nru\t\nbroken\nru\tЯблоко\n");
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains(&(Lang::En, "foo".to_string())));
        assert!(parsed.contains(&(Lang::Ru, "яблоко".to_string())));
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
