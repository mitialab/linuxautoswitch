//! Compact, embedded EN/RU dictionaries used to decide whether a
//! reconstructed word is "real" in a given language.
//!
//! The word lists (`assets/dictionaries/*_raw.txt`) are compiled at build
//! time into `fst::Set`s (see `build.rs`) and the resulting bytes are
//! embedded straight into the binary. An `fst::Set` stores ~2M words in a
//! few MB and does lookups without ever materializing a `HashSet`, which
//! matters here: this is a daemon meant to sit in the background
//! indefinitely, not a short-lived CLI tool.

use crate::keymap::Lang;
use fst::automaton::{Automaton, Str};
use fst::{IntoStreamer, Set, Streamer};
use std::borrow::Cow;
use std::sync::OnceLock;

static EN_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/en.fst"));
static RU_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ru.fst"));

static EN_SET: OnceLock<Set<&'static [u8]>> = OnceLock::new();
static RU_SET: OnceLock<Set<&'static [u8]>> = OnceLock::new();

fn en() -> &'static Set<&'static [u8]> {
    EN_SET.get_or_init(|| Set::new(EN_BYTES).expect("embedded en.fst is a valid fst::Set"))
}

fn ru() -> &'static Set<&'static [u8]> {
    RU_SET.get_or_init(|| Set::new(RU_BYTES).expect("embedded ru.fst is a valid fst::Set"))
}

fn lowered(word: &str) -> Cow<'_, str> {
    // Borrow instead of allocating when there's nothing to lowercase, which
    // is the common case for typed words - these run on every keystroke
    // under eager correction.
    if word.chars().any(char::is_uppercase) {
        Cow::Owned(word.to_lowercase())
    } else {
        Cow::Borrowed(word)
    }
}

fn set(lang: Lang) -> &'static Set<&'static [u8]> {
    match lang {
        Lang::En => en(),
        Lang::Ru => ru(),
    }
}

/// Whether `word` is a known word in `lang`. Case-insensitive.
pub fn contains(lang: Lang, word: &str) -> bool {
    set(lang).contains(lowered(word).as_bytes())
}

/// Whether some known word in `lang` is strictly longer than `prefix` and
/// starts with it - i.e. whether someone typing `prefix` may simply not be
/// finished yet. Case-insensitive.
pub fn has_longer_word(lang: Lang, prefix: &str) -> bool {
    let prefix = lowered(prefix);
    let mut stream = set(lang)
        .search(Str::new(&prefix).starts_with())
        .into_stream();
    // Keys come out in order, so the exact word (if it is one) is first and
    // anything after it is longer: this loop runs at most twice.
    while let Some(key) = stream.next() {
        if key.len() > prefix.len() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_words() {
        assert!(contains(Lang::En, "hello"));
        assert!(contains(Lang::En, "HELLO"));
        assert!(contains(Lang::Ru, "привет"));
        assert!(contains(Lang::Ru, "ПРИВЕТ"));
    }

    #[test]
    fn yo_can_be_typed_as_ye() {
        assert!(contains(Lang::Ru, "ёлка"));
        assert!(contains(Lang::Ru, "елка"));
    }

    #[test]
    fn brand_and_site_names_are_recognized() {
        assert!(contains(Lang::En, "google"));
        assert!(contains(Lang::En, "youtube"));
        assert!(contains(Lang::En, "github"));
        assert!(contains(Lang::Ru, "яндекс"));
        assert!(contains(Lang::Ru, "вконтакте"));
    }

    #[test]
    fn unknown_words() {
        assert!(!contains(Lang::En, "ghbdtn"));
        assert!(!contains(Lang::Ru, "руддщ"));
    }

    #[test]
    fn longer_words_are_found_by_prefix() {
        assert!(has_longer_word(Lang::En, "blen")); // blend
        assert!(has_longer_word(Lang::En, "BLEN"));
        assert!(has_longer_word(Lang::Ru, "прив")); // привет
        // A complete word with no extension has nothing longer.
        assert!(!has_longer_word(Lang::En, "zyzzyvas"));
        assert!(!has_longer_word(Lang::En, "ghbdtn"));
        assert!(!has_longer_word(Lang::Ru, "руддщ"));
    }
}
