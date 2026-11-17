//! Slint key events as the bytes a serial terminal sends.

use origami_terminal::{key_bytes, Key};

/// Modifier keys, as the terminal's owner sees them. `command` is the key
/// that copies and pastes where the platform has one (Command on macOS).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub command: bool,
}

/// What a key press does to a terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Send these bytes to the guest.
    Send(Vec<u8>),
    Copy,
    Paste,
    /// Scroll the history by a page (positive: back).
    Page(isize),
    /// Nothing: a modifier on its own, or a key the terminal has no byte for.
    Ignore,
}

/// Slint's code for a special key, as the text of its key event.
fn special(c: char) -> Option<Option<Key>> {
    use slint::platform::Key as K;
    let table: [(K, Option<Key>); 24] = [
        (K::UpArrow, Some(Key::Up)),
        (K::DownArrow, Some(Key::Down)),
        (K::RightArrow, Some(Key::Right)),
        (K::LeftArrow, Some(Key::Left)),
        (K::Home, Some(Key::Home)),
        (K::End, Some(Key::End)),
        (K::PageUp, Some(Key::PageUp)),
        (K::PageDown, Some(Key::PageDown)),
        (K::Insert, Some(Key::Insert)),
        (K::Delete, Some(Key::Delete)),
        (K::F1, Some(Key::F(1))),
        (K::F2, Some(Key::F(2))),
        (K::F3, Some(Key::F(3))),
        (K::F4, Some(Key::F(4))),
        (K::F5, Some(Key::F(5))),
        (K::F6, Some(Key::F(6))),
        (K::F7, Some(Key::F(7))),
        (K::F8, Some(Key::F(8))),
        (K::F9, Some(Key::F(9))),
        (K::F10, Some(Key::F(10))),
        (K::F11, Some(Key::F(11))),
        (K::F12, Some(Key::F(12))),
        (K::Backtab, Some(Key::BackTab)),
        (K::Menu, None),
    ];
    table
        .iter()
        .find(|(k, _)| char::from(*k) == c)
        .map(|(_, k)| *k)
}

/// Whether `c` is one of Slint's codes for a key with no character (a
/// modifier, a lock, a function key without a sequence here).
fn is_key_code(c: char) -> bool {
    // Slint gives the modifier and lock keys codes in the control range
    // (Shift is U+0010 ... CapsLock U+0014 ... MetaR U+0018). A held Ctrl does
    // not turn a letter into one of these on Linux or Windows: it arrives as
    // the letter with the control modifier, which the match below handles.
    ('\u{f700}'..='\u{f8ff}').contains(&c) || ('\u{10}'..='\u{18}').contains(&c)
}

/// The action for a key press whose Slint text is `text`.
pub fn key_action(text: &str, mods: Mods, app_cursor: bool) -> KeyAction {
    let mut chars = text.chars();
    let Some(c) = chars.next() else {
        return KeyAction::Ignore;
    };
    let single = chars.next().is_none();

    // Copy and paste: Command+C/V where there is a Command key, else
    // Ctrl+Shift+C/V, which leaves Ctrl+C for the guest. Shift+Insert pastes.
    if single {
        let letter = c.to_ascii_lowercase();
        let chord = if mods.command {
            true
        } else {
            mods.ctrl && mods.shift
        };
        if chord && letter == 'c' || chord && letter == '\u{3}' && mods.shift {
            return KeyAction::Copy;
        }
        if chord && letter == 'v' || chord && letter == '\u{16}' && mods.shift {
            return KeyAction::Paste;
        }
        if mods.shift && c == char::from(slint::platform::Key::Insert) {
            return KeyAction::Paste;
        }
        if mods.shift && c == char::from(slint::platform::Key::PageUp) {
            return KeyAction::Page(1);
        }
        if mods.shift && c == char::from(slint::platform::Key::PageDown) {
            return KeyAction::Page(-1);
        }
    }
    if mods.command {
        return KeyAction::Ignore;
    }

    if single {
        if let Some(key) = special(c) {
            let send = key.and_then(|k| key_bytes(k, mods.shift, mods.ctrl, mods.alt, app_cursor));
            return send.map_or(KeyAction::Ignore, KeyAction::Send);
        }
        if is_key_code(c) {
            return KeyAction::Ignore;
        }
        // Alt is Meta only for a plain ASCII key. Where Option or AltGr
        // composes a character (a layout's `@` or `|`), the character itself
        // is what the user typed, and a held Control with it is AltGr on
        // Windows, not a control key.
        let altgr = cfg!(windows) && mods.alt && mods.ctrl;
        let (ctrl, alt) = if altgr || !c.is_ascii() {
            (mods.ctrl && !altgr, false)
        } else {
            (mods.ctrl, mods.alt)
        };
        let key = match c {
            '\n' | '\r' => Key::Enter,
            '\u{8}' => Key::Backspace,
            '\u{7f}' => return KeyAction::Send(b"\x1b[3~".to_vec()),
            '\u{1b}' => return KeyAction::Send(vec![0x1b]),
            '\t' => Key::Tab,
            c => Key::Char(c),
        };
        return key_bytes(key, false, ctrl, alt, false).map_or(KeyAction::Ignore, KeyAction::Send);
    }
    // Several characters at once (an input method's commit).
    if text.chars().any(is_key_code) {
        KeyAction::Ignore
    } else {
        KeyAction::Send(text.as_bytes().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::Key as K;

    fn press(text: &str, mods: Mods) -> KeyAction {
        key_action(text, mods, false)
    }
    fn plain(text: &str) -> KeyAction {
        press(text, Mods::default())
    }
    fn sent(bytes: &[u8]) -> KeyAction {
        KeyAction::Send(bytes.to_vec())
    }
    fn key(k: K) -> String {
        char::from(k).to_string()
    }

    #[test]
    fn ctrl_c_and_escape_reach_the_guest() {
        // Slint hands over the control character itself for a held Ctrl.
        assert_eq!(plain("\u{3}"), sent(&[3]));
        assert_eq!(
            press(
                "c",
                Mods {
                    ctrl: true,
                    ..Default::default()
                }
            ),
            sent(&[3])
        );
        assert_eq!(plain(&key(K::Escape)), sent(&[0x1b]));
        assert_eq!(
            press(
                "d",
                Mods {
                    ctrl: true,
                    ..Default::default()
                }
            ),
            sent(&[4])
        );
    }

    #[test]
    fn copy_and_paste_chords_do_not_take_ctrl_c() {
        let cs = Mods {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(press("C", cs), KeyAction::Copy);
        assert_eq!(press("V", cs), KeyAction::Paste);
        assert_eq!(press("\u{3}", cs), KeyAction::Copy);
        let cmd = Mods {
            command: true,
            ..Default::default()
        };
        assert_eq!(press("c", cmd), KeyAction::Copy);
        assert_eq!(press("v", cmd), KeyAction::Paste);
        assert_eq!(press("x", cmd), KeyAction::Ignore);
        let shift = Mods {
            shift: true,
            ..Default::default()
        };
        assert_eq!(press(&key(K::Insert), shift), KeyAction::Paste);
    }

    #[test]
    fn enter_backspace_tab_and_text() {
        assert_eq!(plain("\n"), sent(b"\r"));
        assert_eq!(plain(&key(K::Return)), sent(b"\r"));
        assert_eq!(plain(&key(K::Backspace)), sent(&[0x7f]));
        assert_eq!(plain("\t"), sent(b"\t"));
        assert_eq!(plain("a"), sent(b"a"));
        assert_eq!(plain("\u{e9}"), sent("\u{e9}".as_bytes()));
        assert_eq!(
            press(
                "x",
                Mods {
                    alt: true,
                    ..Default::default()
                }
            ),
            sent(b"\x1bx")
        );
    }

    #[test]
    fn modifiers_on_navigation_keys_use_the_xterm_forms() {
        let m = |ctrl, alt, shift| Mods {
            ctrl,
            alt,
            shift,
            ..Mods::default()
        };
        assert_eq!(
            press(&key(K::UpArrow), m(true, false, false)),
            sent(b"\x1b[1;5A")
        );
        assert_eq!(
            press(&key(K::LeftArrow), m(false, true, false)),
            sent(b"\x1b[1;3D")
        );
        assert_eq!(
            press(&key(K::F5), m(false, false, true)),
            sent(b"\x1b[15;2~")
        );
        assert_eq!(
            press(&key(K::Delete), m(true, true, false)),
            sent(b"\x1b[3;7~")
        );
    }

    #[test]
    fn arrows_follow_the_cursor_key_mode() {
        assert_eq!(plain(&key(K::UpArrow)), sent(b"\x1b[A"));
        assert_eq!(
            key_action(&key(K::UpArrow), Mods::default(), true),
            sent(b"\x1bOA")
        );
        assert_eq!(plain(&key(K::PageUp)), sent(b"\x1b[5~"));
        assert_eq!(plain(&key(K::Delete)), sent(b"\x1b[3~"));
        assert_eq!(plain(&key(K::F1)), sent(b"\x1bOP"));
        assert_eq!(plain(&key(K::F5)), sent(b"\x1b[15~"));
    }

    #[test]
    fn modifier_keys_alone_send_nothing_and_shift_page_scrolls() {
        assert_eq!(plain(&key(K::Shift)), KeyAction::Ignore);
        assert_eq!(plain(&key(K::Control)), KeyAction::Ignore);
        assert_eq!(plain(&key(K::CapsLock)), KeyAction::Ignore);
        assert_eq!(plain(""), KeyAction::Ignore);
        let shift = Mods {
            shift: true,
            ..Default::default()
        };
        assert_eq!(press(&key(K::PageUp), shift), KeyAction::Page(1));
        assert_eq!(press(&key(K::PageDown), shift), KeyAction::Page(-1));
    }

    #[test]
    fn a_character_composed_with_alt_is_sent_as_typed() {
        let alt = Mods {
            alt: true,
            ..Default::default()
        };
        assert_eq!(press("\u{e9}", alt), sent("\u{e9}".as_bytes()));
        assert_eq!(press("x", alt), sent(b"\x1bx"));
    }
}
