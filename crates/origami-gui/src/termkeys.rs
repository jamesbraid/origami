//! Slint key events as the bytes a serial terminal sends.

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
fn special(c: char) -> Option<&'static str> {
    use slint::platform::Key as K;
    let table: [(K, &str); 24] = [
        (K::UpArrow, "A"),
        (K::DownArrow, "B"),
        (K::RightArrow, "C"),
        (K::LeftArrow, "D"),
        (K::Home, "H"),
        (K::End, "F"),
        (K::PageUp, "5~"),
        (K::PageDown, "6~"),
        (K::Insert, "2~"),
        (K::Delete, "3~"),
        (K::F1, "OP"),
        (K::F2, "OQ"),
        (K::F3, "OR"),
        (K::F4, "OS"),
        (K::F5, "15~"),
        (K::F6, "17~"),
        (K::F7, "18~"),
        (K::F8, "19~"),
        (K::F9, "20~"),
        (K::F10, "21~"),
        (K::F11, "23~"),
        (K::F12, "24~"),
        (K::Backtab, "Z"),
        (K::Menu, ""),
    ];
    table
        .iter()
        .find(|(k, _)| char::from(*k) == c)
        .map(|(_, s)| *s)
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
        if let Some(seq) = special(c) {
            if seq.is_empty() {
                return KeyAction::Ignore;
            }
            let mut out = vec![0x1b];
            let (intro, body) = match seq.strip_prefix('O') {
                Some(rest) => (b'O', rest),
                None => (
                    if app_cursor && seq.len() == 1 {
                        b'O'
                    } else {
                        b'['
                    },
                    seq,
                ),
            };
            out.push(intro);
            out.extend_from_slice(body.as_bytes());
            if mods.alt {
                out.insert(0, 0x1b);
            }
            return KeyAction::Send(out);
        }
        if is_key_code(c) {
            return KeyAction::Ignore;
        }
        // Alt is Meta only for a plain ASCII key. Where Option or AltGr
        // composes a character (a layout's `@` or `|`), the character itself
        // is what the user typed, and a held Control with it is AltGr on
        // Windows, not a control key.
        let composed = !c.is_ascii() || (cfg!(windows) && mods.alt && mods.ctrl);
        let mods = if cfg!(windows) && mods.alt && mods.ctrl {
            Mods {
                ctrl: false,
                alt: false,
                ..mods
            }
        } else {
            mods
        };
        let send = |mut b: Vec<u8>| {
            if mods.alt && !composed {
                b.insert(0, 0x1b);
            }
            KeyAction::Send(b)
        };
        return match c {
            '\n' | '\r' => send(vec![b'\r']),
            // Backspace sends DEL, as xterm and the Linux console do; Ctrl-H
            // is still ^H.
            '\u{8}' => send(vec![0x7f]),
            '\u{7f}' => KeyAction::Send(b"\x1b[3~".to_vec()),
            '\u{1b}' => KeyAction::Send(vec![0x1b]),
            '\t' => send(vec![b'\t']),
            // A control character a platform made of a held Ctrl (Linux and
            // Windows send the letter and the modifier instead).
            c if (c as u32) < 0x20 => send(vec![c as u8]),
            c if mods.ctrl => match c {
                'a'..='z' | 'A'..='Z' => send(vec![(c.to_ascii_lowercase() as u8) & 0x1f]),
                '@' | ' ' | '2' => send(vec![0]),
                '[' | '3' => send(vec![0x1b]),
                '\\' | '4' => send(vec![0x1c]),
                ']' | '5' => send(vec![0x1d]),
                '^' | '6' => send(vec![0x1e]),
                '_' | '7' | '/' => send(vec![0x1f]),
                '8' => send(vec![0x7f]),
                _ => KeyAction::Ignore,
            },
            c => send(c.to_string().into_bytes()),
        };
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
