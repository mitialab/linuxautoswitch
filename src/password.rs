//! Spots text that looks like a password while it is being typed, so it is
//! never "corrected": a password mixes cases, digits and symbols, and the
//! word buffer (which ends at every digit and symbol) never sees it whole.
//!
//! The rule: a run of keys without a Space/Enter/Tab, longer than six keys,
//! containing a lowercase letter, an uppercase letter and a digit or symbol.
//! It is a guess from the shape of the text - Wayland doesn't say which field
//! has focus - so it can't replace excluding a password manager's window.

use crate::keymap;
use evdev::KeyCode;

/// What has been typed since the last Space/Enter/Tab, as a summary.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct TokenStats {
    len: usize,
    lower: bool,
    upper: bool,
    digit: bool,
    symbol: bool,
}

/// Keys longer than this, with the right mix, make a token password-like.
const MIN_PASSWORD_LEN: usize = 7;

impl TokenStats {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Counts one pressed key. Keys that type nothing (arrows, F-keys, ...)
    /// are ignored.
    pub fn push(&mut self, code: KeyCode, shift: bool, capslock: bool) {
        let kind = if keymap::is_az(code) {
            if shift ^ capslock {
                self.upper = true;
            } else {
                self.lower = true;
            }
            true
        } else if is_digit_row(code) {
            if shift {
                self.symbol = true;
            } else {
                self.digit = true;
            }
            true
        } else if is_keypad_digit(code) {
            self.digit = true;
            true
        } else if is_symbol_key(code) {
            self.symbol = true;
            true
        } else {
            false
        };
        if kind {
            self.len += 1;
        }
    }

    /// A Backspace shortens the token; what it contained so far stays
    /// counted (a password with a typo is still a password).
    pub fn pop(&mut self) {
        self.len = self.len.saturating_sub(1);
    }

    pub fn looks_like_password(&self) -> bool {
        self.len >= MIN_PASSWORD_LEN && self.lower && self.upper && (self.digit || self.symbol)
    }
}

fn is_digit_row(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::KEY_1
            | KeyCode::KEY_2
            | KeyCode::KEY_3
            | KeyCode::KEY_4
            | KeyCode::KEY_5
            | KeyCode::KEY_6
            | KeyCode::KEY_7
            | KeyCode::KEY_8
            | KeyCode::KEY_9
            | KeyCode::KEY_0
    )
}

fn is_keypad_digit(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::KEY_KP0
            | KeyCode::KEY_KP1
            | KeyCode::KEY_KP2
            | KeyCode::KEY_KP3
            | KeyCode::KEY_KP4
            | KeyCode::KEY_KP5
            | KeyCode::KEY_KP6
            | KeyCode::KEY_KP7
            | KeyCode::KEY_KP8
            | KeyCode::KEY_KP9
    )
}

/// Punctuation keys. On the Russian layout several of these are letters, but
/// a password is typed on the English one, so for this purpose they are
/// symbols.
fn is_symbol_key(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::KEY_MINUS
            | KeyCode::KEY_EQUAL
            | KeyCode::KEY_BACKSLASH
            | KeyCode::KEY_LEFTBRACE
            | KeyCode::KEY_RIGHTBRACE
            | KeyCode::KEY_SEMICOLON
            | KeyCode::KEY_APOSTROPHE
            | KeyCode::KEY_GRAVE
            | KeyCode::KEY_COMMA
            | KeyCode::KEY_DOT
            | KeyCode::KEY_SLASH
    )
}

/// Keys that end a token: whatever follows starts something new.
pub fn ends_token(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::KEY_SPACE | KeyCode::KEY_ENTER | KeyCode::KEY_KPENTER | KeyCode::KEY_TAB
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(keys: &[(KeyCode, bool)]) -> TokenStats {
        let mut stats = TokenStats::default();
        for &(code, shift) in keys {
            stats.push(code, shift, false);
        }
        stats
    }

    // "Passw0rd!" = P(shift) a s s w 0 r d 1(shift)
    const PASSWORD: [(KeyCode, bool); 9] = [
        (KeyCode::KEY_P, true),
        (KeyCode::KEY_A, false),
        (KeyCode::KEY_S, false),
        (KeyCode::KEY_S, false),
        (KeyCode::KEY_W, false),
        (KeyCode::KEY_0, false),
        (KeyCode::KEY_R, false),
        (KeyCode::KEY_D, false),
        (KeyCode::KEY_1, true),
    ];

    #[test]
    fn mixed_case_digits_and_symbols_make_a_password() {
        assert!(typed(&PASSWORD).looks_like_password());
    }

    #[test]
    fn six_keys_are_too_short_but_seven_are_enough() {
        assert!(!typed(&PASSWORD[..6]).looks_like_password());
        assert!(typed(&PASSWORD[..7]).looks_like_password());
    }

    #[test]
    fn plain_words_and_single_case_text_are_not_passwords() {
        // "hello1234": no uppercase.
        let lower: Vec<_> = [
            KeyCode::KEY_H,
            KeyCode::KEY_E,
            KeyCode::KEY_L,
            KeyCode::KEY_L,
            KeyCode::KEY_O,
            KeyCode::KEY_1,
            KeyCode::KEY_2,
            KeyCode::KEY_3,
        ]
        .into_iter()
        .map(|c| (c, false))
        .collect();
        assert!(!typed(&lower).looks_like_password());
        // "Helloworld": no digit or symbol.
        let mut words = vec![(KeyCode::KEY_H, true)];
        words.extend([KeyCode::KEY_E; 9].map(|c| (c, false)));
        assert!(!typed(&words).looks_like_password());
    }

    #[test]
    fn caps_lock_counts_as_uppercase() {
        let mut stats = TokenStats::default();
        stats.push(KeyCode::KEY_A, false, true); // A
        for _ in 0..4 {
            stats.push(KeyCode::KEY_B, false, false);
        }
        stats.push(KeyCode::KEY_1, false, false);
        stats.push(KeyCode::KEY_2, false, false);
        assert!(stats.looks_like_password());
    }

    #[test]
    fn keys_that_type_nothing_are_not_counted() {
        let mut stats = typed(&PASSWORD[..6]);
        stats.push(KeyCode::KEY_LEFT, false, false);
        stats.push(KeyCode::KEY_F5, false, false);
        assert!(!stats.looks_like_password());
    }

    #[test]
    fn backspace_shortens_but_keeps_what_was_seen() {
        let mut stats = typed(&PASSWORD[..7]);
        assert!(stats.looks_like_password());
        stats.pop();
        assert!(!stats.looks_like_password()); // back to six keys
        stats.push(KeyCode::KEY_X, false, false);
        assert!(stats.looks_like_password());
    }

    #[test]
    fn only_space_enter_and_tab_end_a_token() {
        assert!(ends_token(KeyCode::KEY_SPACE));
        assert!(ends_token(KeyCode::KEY_ENTER));
        assert!(ends_token(KeyCode::KEY_TAB));
        assert!(!ends_token(KeyCode::KEY_1));
        assert!(!ends_token(KeyCode::KEY_DOT));
    }
}
