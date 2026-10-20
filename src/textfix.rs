//! Fixes that rewrite a word you have finished typing, in the layout it is
//! already in (unlike a layout correction, nothing is switched). Each is an
//! opt-in `[experimental]` feature, run when Space ends the word.

use crate::config::Experimental;
use crate::dictionary;
use crate::keymap::Lang;
use crate::userdata::UserData;

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
    (word != core).then(|| word + tail)
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

    #[test]
    fn disabled_by_default() {
        let off = Experimental::default();
        assert_eq!(
            fix_word("HEllo", Lang::En, &off, &UserData::default()),
            None
        );
    }
}
