//! Fixes that rewrite a word you have finished typing, in the layout it is
//! already in (unlike a layout correction, nothing is switched). Each is an
//! opt-in `[experimental]` feature, run when Space ends the word.

use crate::config::Experimental;
use crate::dictionary;
use crate::engine::apply_case_pattern;
use crate::keymap::Lang;
use crate::userdata::{self, UserData};
use std::collections::HashMap;
use std::sync::OnceLock;

const TYPOS_EN: &str = include_str!("../assets/dictionaries/typos_en.txt");
const TYPOS_RU: &str = include_str!("../assets/dictionaries/typos_ru.txt");
const YO_RU: &str = include_str!("../assets/dictionaries/yo_ru.txt");

fn builtin_typos(lang: Lang) -> &'static HashMap<String, String> {
    static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
    static RU: OnceLock<HashMap<String, String>> = OnceLock::new();
    match lang {
        Lang::En => EN.get_or_init(|| userdata::parse_typo_list(TYPOS_EN)),
        Lang::Ru => RU.get_or_init(|| userdata::parse_typo_list(TYPOS_RU)),
    }
}

/// Words that take a ё, keyed by their spelling with е.
fn yo_words() -> &'static HashMap<String, &'static str> {
    static MAP: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        YO_RU
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .flat_map(str::split_whitespace)
            .map(|word| (word.replace('ё', "е"), word))
            .collect()
    })
}

/// The fixed text for `text` (a finished word, possibly with trailing
/// punctuation), or `None` if no enabled fix applies.
pub fn fix_word(text: &str, lang: Lang, opts: &Experimental, user: &UserData) -> Option<String> {
    let core = text.trim_end_matches(|c: char| !c.is_alphabetic());
    let tail = &text[core.len()..];
    let mut word = core.to_string();
    if opts.double_caps_fix
        && let Some(fixed) = double_caps(&word, lang, user)
    {
        word = fixed;
    }
    if opts.typo_correction
        && let Some(fixed) = typo(&word, lang, user)
    {
        word = fixed;
    }
    if opts.yofikator
        && lang == Lang::Ru
        && let Some(fixed) = yofy(&word)
    {
        word = fixed;
    }
    (word != core).then(|| word + tail)
}

/// The corrected spelling of a common misspelling, keeping the capitals the
/// word was typed with. The person's own list wins over the built-in one.
fn typo(word: &str, lang: Lang, user: &UserData) -> Option<String> {
    let lower = word.to_lowercase();
    let fixed = user
        .typo(lang, &lower)
        .or_else(|| builtin_typos(lang).get(&lower).map(String::as_str))?;
    Some(apply_case_pattern(word, fixed))
}

/// "еще" -> "ещё" for the words in `yo_ru.txt`.
fn yofy(word: &str) -> Option<String> {
    let fixed = yo_words().get(&word.to_lowercase())?;
    Some(apply_case_pattern(word, fixed))
}

/// "ПРивет" -> "Привет": the Shift key was held a moment too long. Only when
/// the word is exactly two capitals followed by at least two lowercase
/// letters (four letters or more in all) *and* its lowercase form is a real
/// word, so initialisms and mixed-case terms - "PRs", "TVs", "MHz", "GPUs",
/// "iPhone", "McDonald" - are never touched.
fn double_caps(word: &str, lang: Lang, user: &UserData) -> Option<String> {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 4
        || !chars.iter().all(|c| c.is_alphabetic())
        || !chars[0].is_uppercase()
        || !chars[1].is_uppercase()
        || !chars[2..].iter().all(|c| c.is_lowercase())
    {
        return None;
    }
    let lower = word.to_lowercase();
    if !(dictionary::contains(lang, &lower) || user.knows(lang, &lower)) {
        return None;
    }
    let mut fixed = String::with_capacity(word.len());
    fixed.extend(chars[0].to_uppercase());
    fixed.push_str(&lower[chars[0].to_lowercase().map(char::len_utf8).sum::<usize>()..]);
    Some(fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Experimental {
        Experimental {
            double_caps_fix: true,
            ..Experimental::default()
        }
    }

    fn fix(text: &str, lang: Lang) -> Option<String> {
        fix_word(text, lang, &on(), &UserData::default())
    }

    #[test]
    fn second_capital_is_lowered_in_both_languages() {
        assert_eq!(fix("HEllo", Lang::En).as_deref(), Some("Hello"));
        assert_eq!(fix("ПРивет", Lang::Ru).as_deref(), Some("Привет"));
    }

    #[test]
    fn trailing_punctuation_is_kept() {
        assert_eq!(fix("HEllo,", Lang::En).as_deref(), Some("Hello,"));
        assert_eq!(fix("ПРивет.", Lang::Ru).as_deref(), Some("Привет."));
    }

    #[test]
    fn initialisms_and_mixed_case_terms_are_left_alone() {
        for word in [
            "PRs", "TVs", "MHz", "IDs", "GPUs", "HELLO", "Hello", "hello", "iPhone", "McDonald",
            "USB", "ID",
        ] {
            assert_eq!(fix(word, Lang::En), None, "{word}");
        }
    }

    #[test]
    fn a_word_that_is_not_a_word_is_left_alone() {
        // "Zxcvbn" is not a word, so we can't know the capitals were a slip.
        assert_eq!(fix("ZXcvbn", Lang::En), None);
    }

    fn with(configure: impl FnOnce(&mut Experimental)) -> Experimental {
        let mut opts = Experimental::default();
        configure(&mut opts);
        opts
    }

    #[test]
    fn typos_are_fixed_keeping_capitals_and_punctuation() {
        let opts = with(|o| o.typo_correction = true);
        let user = UserData::default();
        assert_eq!(
            fix_word("teh", Lang::En, &opts, &user).as_deref(),
            Some("the")
        );
        assert_eq!(
            fix_word("Teh", Lang::En, &opts, &user).as_deref(),
            Some("The")
        );
        assert_eq!(
            fix_word("TEH", Lang::En, &opts, &user).as_deref(),
            Some("THE")
        );
        assert_eq!(
            fix_word("teh,", Lang::En, &opts, &user).as_deref(),
            Some("the,")
        );
        assert_eq!(
            fix_word("Сдесь", Lang::Ru, &opts, &user).as_deref(),
            Some("Здесь")
        );
        assert_eq!(
            fix_word("вобщем", Lang::Ru, &opts, &user).as_deref(),
            Some("в общем")
        );
    }

    #[test]
    fn typos_are_per_language_and_leave_real_words_alone() {
        let opts = with(|o| o.typo_correction = true);
        let user = UserData::default();
        assert_eq!(fix_word("teh", Lang::Ru, &opts, &user), None);
        assert_eq!(fix_word("the", Lang::En, &opts, &user), None);
        assert_eq!(fix_word("здесь", Lang::Ru, &opts, &user), None);
    }

    #[test]
    fn own_typo_list_wins_over_the_built_in_one() {
        let opts = with(|o| o.typo_correction = true);
        let dir = std::env::temp_dir().join(format!("las-typos-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("typos-en.txt"), "teh the-one\nhte the\n").unwrap();
        let user = UserData::load(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            fix_word("teh", Lang::En, &opts, &user).as_deref(),
            Some("the-one")
        );
        assert_eq!(
            fix_word("hte", Lang::En, &opts, &user).as_deref(),
            Some("the")
        );
    }

    /// Misspellings the bundled word lists wrongly accept (they are scraped
    /// and noisy). The typo table never consults the dictionary, so these are
    /// fixed anyway - but a *new* entry that the dictionary accepts has to be
    /// added here on purpose, after checking it really is never correct.
    const KNOWN_NOISY_DICTIONARY_ENTRIES: [&str; 17] = [
        "wich",
        "untill",
        "accomodate",
        "calender",
        "enviroment",
        "relevent",
        "succesful",
        "wierd",
        "cemetary",
        "immediatly",
        "millenium",
        "noticable",
        "prefered",
        "unforseen",
        "пожалуста",
        "чтото",
        "кудато",
    ];

    #[test]
    fn typos_the_dictionary_accepts_are_listed_on_purpose() {
        let mut accepted: Vec<&str> = Vec::new();
        for lang in [Lang::En, Lang::Ru] {
            for wrong in builtin_typos(lang).keys() {
                if dictionary::contains(lang, wrong) {
                    accepted.push(wrong);
                }
            }
        }
        accepted.sort_unstable();
        let mut expected = KNOWN_NOISY_DICTIONARY_ENTRIES.to_vec();
        expected.sort_unstable();
        assert_eq!(accepted, expected);
    }

    #[test]
    fn built_in_typos_never_chain_into_each_other() {
        for lang in [Lang::En, Lang::Ru] {
            for right in builtin_typos(lang).values() {
                assert!(!builtin_typos(lang).contains_key(right), "{right}");
            }
        }
    }

    #[test]
    fn yo_is_restored_for_listed_words_only() {
        let opts = with(|o| o.yofikator = true);
        let user = UserData::default();
        assert_eq!(
            fix_word("еще", Lang::Ru, &opts, &user).as_deref(),
            Some("ещё")
        );
        assert_eq!(
            fix_word("Идет", Lang::Ru, &opts, &user).as_deref(),
            Some("Идёт")
        );
        assert_eq!(
            fix_word("ЕЩЕ", Lang::Ru, &opts, &user).as_deref(),
            Some("ЕЩЁ")
        );
        assert_eq!(
            fix_word("ребенок.", Lang::Ru, &opts, &user).as_deref(),
            Some("ребёнок.")
        );
        // Deliberately not listed: a different word without the ё.
        for word in ["все", "чем", "всем", "моем", "мед", "черт"] {
            assert_eq!(fix_word(word, Lang::Ru, &opts, &user), None, "{word}");
        }
        // Already right, or not Russian.
        assert_eq!(fix_word("ещё", Lang::Ru, &opts, &user), None);
        assert_eq!(fix_word("еще", Lang::En, &opts, &user), None);
    }

    #[test]
    fn yo_list_is_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for line in YO_RU.lines().filter(|l| !l.trim_start().starts_with('#')) {
            for word in line.split_whitespace() {
                assert!(word.contains('ё'), "{word} has no ё");
                assert!(seen.insert(word.replace('ё', "е")), "{word} listed twice");
            }
        }
        assert!(seen.len() > 100);
    }

    #[test]
    fn disabled_by_default() {
        let off = Experimental::default();
        assert_eq!(
            fix_word("HEllo", Lang::En, &off, &UserData::default()),
            None
        );
    }
}
