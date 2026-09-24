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
use fst::Set;
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

/// Whether `word` is a known word in `lang`. Case-insensitive.
pub fn contains(lang: Lang, word: &str) -> bool {
    let lower = word.to_lowercase();
    match lang {
        Lang::En => en().contains(lower.as_bytes()),
        Lang::Ru => ru().contains(lower.as_bytes()),
    }
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
    fn unknown_words() {
        assert!(!contains(Lang::En, "ghbdtn"));
        assert!(!contains(Lang::Ru, "руддщ"));
    }
}
