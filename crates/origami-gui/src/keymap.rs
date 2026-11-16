//! Slint key events to the core's typed keys.

use crate::core::Key;
use slint::platform::Key as SK;

/// The key a Slint key event's text stands for, on a US layout. Shift is a
/// key of its own in Slint's events, so a shifted symbol maps to the key
/// that makes it and the guest sees the shift press that came before.
pub fn from_text(text: &str) -> Option<Key> {
    let c = text.chars().next()?;
    let special = [
        (SK::Escape, Key::Escape),
        (SK::Tab, Key::Tab),
        (SK::Backspace, Key::Backspace),
        (SK::Return, Key::Enter),
        (SK::Delete, Key::Delete),
        (SK::Insert, Key::Insert),
        (SK::Home, Key::Home),
        (SK::End, Key::End),
        (SK::PageUp, Key::PageUp),
        (SK::PageDown, Key::PageDown),
        (SK::UpArrow, Key::Up),
        (SK::DownArrow, Key::Down),
        (SK::LeftArrow, Key::Left),
        (SK::RightArrow, Key::Right),
        (SK::Shift, Key::ShiftLeft),
        (SK::ShiftR, Key::ShiftRight),
        (SK::Control, Key::ControlLeft),
        (SK::ControlR, Key::ControlRight),
        (SK::Alt, Key::AltLeft),
        (SK::AltGr, Key::AltRight),
        (SK::Meta, Key::MetaLeft),
        (SK::MetaR, Key::MetaRight),
        (SK::CapsLock, Key::CapsLock),
        (SK::ScrollLock, Key::ScrollLock),
        (SK::Pause, Key::Pause),
        (SK::SysReq, Key::PrintScreen),
        (SK::Menu, Key::Menu),
        (SK::F1, Key::F1),
        (SK::F2, Key::F2),
        (SK::F3, Key::F3),
        (SK::F4, Key::F4),
        (SK::F5, Key::F5),
        (SK::F6, Key::F6),
        (SK::F7, Key::F7),
        (SK::F8, Key::F8),
        (SK::F9, Key::F9),
        (SK::F10, Key::F10),
        (SK::F11, Key::F11),
        (SK::F12, Key::F12),
    ];
    if let Some((_, k)) = special.iter().find(|(s, _)| char::from(*s) == c) {
        return Some(*k);
    }
    use Key::*;
    let letters = [
        A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    ];
    let digits = [
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ];
    Some(match c {
        'a'..='z' => letters[(c as u8 - b'a') as usize],
        'A'..='Z' => letters[(c as u8 - b'A') as usize],
        // Control characters a held control key turns letters into.
        '\u{1}'..='\u{1a}' if !matches!(c, '\t' | '\n' | '\r' | '\u{8}') => {
            letters[(c as u8 - 1) as usize]
        }
        '0'..='9' => digits[(c as u8 - b'0') as usize],
        ')' => Digit0,
        '!' => Digit1,
        '@' => Digit2,
        '#' => Digit3,
        '$' => Digit4,
        '%' => Digit5,
        '^' => Digit6,
        '&' => Digit7,
        '*' => Digit8,
        '(' => Digit9,
        ' ' => Space,
        '\n' | '\r' => Enter,
        '\t' => Tab,
        '\u{8}' => Backspace,
        '\u{1b}' => Escape,
        '\u{7f}' => Delete,
        '-' | '_' => Minus,
        '=' | '+' => Equal,
        '[' | '{' => BracketLeft,
        ']' | '}' => BracketRight,
        '\\' | '|' => Backslash,
        ';' | ':' => Semicolon,
        '\'' | '"' => Apostrophe,
        '`' | '~' => Grave,
        ',' | '<' => Comma,
        '.' | '>' => Period,
        '/' | '?' => Slash,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_shifted_symbols_map_to_their_keys() {
        assert_eq!(from_text("q"), Some(Key::Q));
        assert_eq!(from_text("Q"), Some(Key::Q));
        assert_eq!(from_text("7"), Some(Key::Digit7));
        assert_eq!(from_text("&"), Some(Key::Digit7));
        assert_eq!(from_text("?"), Some(Key::Slash));
        assert_eq!(from_text(" "), Some(Key::Space));
        assert_eq!(from_text("\n"), Some(Key::Enter));
        assert_eq!(from_text("\u{3}"), Some(Key::C));
        assert_eq!(from_text("é"), None);
        assert_eq!(from_text(""), None);
    }

    #[test]
    fn slints_special_keys_map_to_theirs() {
        assert_eq!(
            from_text(&String::from(char::from(SK::DownArrow))),
            Some(Key::Down)
        );
        assert_eq!(
            from_text(&String::from(char::from(SK::Escape))),
            Some(Key::Escape)
        );
        assert_eq!(
            from_text(&String::from(char::from(SK::Shift))),
            Some(Key::ShiftLeft)
        );
        assert_eq!(from_text(&String::from(char::from(SK::F5))), Some(Key::F5));
    }
}
