//! Physical-key -> character mapping for the two layouts we support.
//!
//! Evdev key codes identify a *physical* key position, independent of the
//! currently active XKB layout (that's the whole trick this tool relies on:
//! we can reconstruct "what this key sequence would mean under layout X"
//! without ever asking the compositor what layout is active). The tables
//! below encode the standard Windows/X11 Russian ("ЙЦУКЕН") layout, which is
//! also what Hyprland/Omarchy ships by default, laid out on the same
//! physical positions as a US QWERTY layout.

use evdev::KeyCode;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    En,
    Ru,
}

/// (en_lower, en_upper, ru_lower, ru_upper) for every physical key that can
/// produce a letter in *either* language. Note several keys that are pure
/// punctuation on a US layout (`[`, `]`, `;`, `'`, `,`, `.`, `` ` ``) double
/// as genuine Cyrillic letters (х ъ ж э б ю ё) on the Russian layout, so they
/// have to be treated as word-forming keys too.
///
/// `KeyCode` is a newtype (`struct KeyCode(pub u16)`) with associated
/// consts, not a real Rust enum, so its variants are matched here via their
/// fully qualified `KeyCode::KEY_*` paths rather than a glob import (a glob
/// import doesn't bring associated consts into pattern scope).
fn table(key: KeyCode) -> Option<(char, char, char, char)> {
    Some(match key {
        KeyCode::KEY_Q => ('q', 'Q', 'й', 'Й'),
        KeyCode::KEY_W => ('w', 'W', 'ц', 'Ц'),
        KeyCode::KEY_E => ('e', 'E', 'у', 'У'),
        KeyCode::KEY_R => ('r', 'R', 'к', 'К'),
        KeyCode::KEY_T => ('t', 'T', 'е', 'Е'),
        KeyCode::KEY_Y => ('y', 'Y', 'н', 'Н'),
        KeyCode::KEY_U => ('u', 'U', 'г', 'Г'),
        KeyCode::KEY_I => ('i', 'I', 'ш', 'Ш'),
        KeyCode::KEY_O => ('o', 'O', 'щ', 'Щ'),
        KeyCode::KEY_P => ('p', 'P', 'з', 'З'),
        KeyCode::KEY_LEFTBRACE => ('[', '{', 'х', 'Х'),
        KeyCode::KEY_RIGHTBRACE => (']', '}', 'ъ', 'Ъ'),

        KeyCode::KEY_A => ('a', 'A', 'ф', 'Ф'),
        KeyCode::KEY_S => ('s', 'S', 'ы', 'Ы'),
        KeyCode::KEY_D => ('d', 'D', 'в', 'В'),
        KeyCode::KEY_F => ('f', 'F', 'а', 'А'),
        KeyCode::KEY_G => ('g', 'G', 'п', 'П'),
        KeyCode::KEY_H => ('h', 'H', 'р', 'Р'),
        KeyCode::KEY_J => ('j', 'J', 'о', 'О'),
        KeyCode::KEY_K => ('k', 'K', 'л', 'Л'),
        KeyCode::KEY_L => ('l', 'L', 'д', 'Д'),
        KeyCode::KEY_SEMICOLON => (';', ':', 'ж', 'Ж'),
        KeyCode::KEY_APOSTROPHE => ('\'', '"', 'э', 'Э'),
        KeyCode::KEY_GRAVE => ('`', '~', 'ё', 'Ё'),

        KeyCode::KEY_Z => ('z', 'Z', 'я', 'Я'),
        KeyCode::KEY_X => ('x', 'X', 'ч', 'Ч'),
        KeyCode::KEY_C => ('c', 'C', 'с', 'С'),
        KeyCode::KEY_V => ('v', 'V', 'м', 'М'),
        KeyCode::KEY_B => ('b', 'B', 'и', 'И'),
        KeyCode::KEY_N => ('n', 'N', 'т', 'Т'),
        KeyCode::KEY_M => ('m', 'M', 'ь', 'Ь'),
        KeyCode::KEY_COMMA => (',', '<', 'б', 'Б'),
        KeyCode::KEY_DOT => ('.', '>', 'ю', 'Ю'),
        KeyCode::KEY_SLASH => ('/', '?', '.', ','),

        _ => return None,
    })
}

/// True for the 33 physical keys that form a letter in at least one of the
/// two layouts (and therefore should extend the in-progress word buffer
/// instead of ending it).
pub fn is_word_key(key: KeyCode) -> bool {
    table(key).is_some()
}

/// True for the "plain" a-z row, where Caps Lock behaves the same in both
/// layouts. The extra punctuation-keys-that-are-letters-in-Russian
/// (see `table` above) are handled separately in `char_for`, since real
/// keyboards apply Caps Lock to *all* Cyrillic letter positions but only to
/// a-z on a US layout.
fn is_az(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::KEY_A
            | KeyCode::KEY_B
            | KeyCode::KEY_C
            | KeyCode::KEY_D
            | KeyCode::KEY_E
            | KeyCode::KEY_F
            | KeyCode::KEY_G
            | KeyCode::KEY_H
            | KeyCode::KEY_I
            | KeyCode::KEY_J
            | KeyCode::KEY_K
            | KeyCode::KEY_L
            | KeyCode::KEY_M
            | KeyCode::KEY_N
            | KeyCode::KEY_O
            | KeyCode::KEY_P
            | KeyCode::KEY_Q
            | KeyCode::KEY_R
            | KeyCode::KEY_S
            | KeyCode::KEY_T
            | KeyCode::KEY_U
            | KeyCode::KEY_V
            | KeyCode::KEY_W
            | KeyCode::KEY_X
            | KeyCode::KEY_Y
            | KeyCode::KEY_Z
    )
}

/// The character `key` would produce under `lang`, given the shift and caps
/// lock state at the time it was pressed. Returns `None` for keys outside
/// the word-forming set.
pub fn char_for(key: KeyCode, shift: bool, capslock: bool, lang: Lang) -> Option<char> {
    let (en_lo, en_hi, ru_lo, ru_hi) = table(key)?;
    let effective_shift = match lang {
        // On a US layout, Caps Lock only affects the true a-z letters.
        Lang::En => shift ^ (capslock && is_az(key)),
        // On a Russian layout, all 33 letter positions respond to Caps Lock.
        Lang::Ru => shift ^ capslock,
    };
    Some(match (lang, effective_shift) {
        (Lang::En, false) => en_lo,
        (Lang::En, true) => en_hi,
        (Lang::Ru, false) => ru_lo,
        (Lang::Ru, true) => ru_hi,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_privet_example() {
        // Typing "privet" (Russian "привет") on an English layout, physically
        // pressing g h b d t n, should reconstruct as "привет" under Ru.
        let keys = [
            KeyCode::KEY_G,
            KeyCode::KEY_H,
            KeyCode::KEY_B,
            KeyCode::KEY_D,
            KeyCode::KEY_T,
            KeyCode::KEY_N,
        ];
        let word: String = keys
            .iter()
            .map(|&k| char_for(k, false, false, Lang::Ru).unwrap())
            .collect();
        assert_eq!(word, "привет");
    }

    #[test]
    fn capslock_only_shifts_az_in_english() {
        assert_eq!(
            char_for(KeyCode::KEY_SEMICOLON, false, true, Lang::En),
            Some(';')
        );
        assert_eq!(
            char_for(KeyCode::KEY_SEMICOLON, false, true, Lang::Ru),
            Some('Ж')
        );
    }
}
