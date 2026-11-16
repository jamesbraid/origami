//! Typed input events as version 3 `INPUT` messages: Linux evdev key codes,
//! QAPI `InputButton` numbers, relative motion and absolute positions.

use super::proto::{Input, InputKind};
use crate::core::{InputEvent, Key, PointerButton};

/// The Linux evdev key code (`input-event-codes.h`) for a key.
pub fn evdev(key: Key) -> u32 {
    use Key::*;
    match key {
        Escape => 1,
        Digit1 => 2,
        Digit2 => 3,
        Digit3 => 4,
        Digit4 => 5,
        Digit5 => 6,
        Digit6 => 7,
        Digit7 => 8,
        Digit8 => 9,
        Digit9 => 10,
        Digit0 => 11,
        Minus => 12,
        Equal => 13,
        Backspace => 14,
        Tab => 15,
        Q => 16,
        W => 17,
        E => 18,
        R => 19,
        T => 20,
        Y => 21,
        U => 22,
        I => 23,
        O => 24,
        P => 25,
        BracketLeft => 26,
        BracketRight => 27,
        Enter => 28,
        ControlLeft => 29,
        A => 30,
        S => 31,
        D => 32,
        F => 33,
        G => 34,
        H => 35,
        J => 36,
        K => 37,
        L => 38,
        Semicolon => 39,
        Apostrophe => 40,
        Grave => 41,
        ShiftLeft => 42,
        Backslash => 43,
        Z => 44,
        X => 45,
        C => 46,
        V => 47,
        B => 48,
        N => 49,
        M => 50,
        Comma => 51,
        Period => 52,
        Slash => 53,
        ShiftRight => 54,
        AltLeft => 56,
        Space => 57,
        CapsLock => 58,
        F1 => 59,
        F2 => 60,
        F3 => 61,
        F4 => 62,
        F5 => 63,
        F6 => 64,
        F7 => 65,
        F8 => 66,
        F9 => 67,
        F10 => 68,
        ScrollLock => 70,
        F11 => 87,
        F12 => 88,
        ControlRight => 97,
        PrintScreen => 99,
        AltRight => 100,
        Home => 102,
        Up => 103,
        PageUp => 104,
        Left => 105,
        Right => 106,
        End => 107,
        Down => 108,
        PageDown => 109,
        Insert => 110,
        Delete => 111,
        Pause => 119,
        MetaLeft => 125,
        MetaRight => 126,
        Menu => 127,
    }
}

/// QAPI `InputButton`: left, middle, right, wheel-up, wheel-down, ...
fn button(b: PointerButton) -> u32 {
    match b {
        PointerButton::Left => 0,
        PointerButton::Middle => 1,
        PointerButton::Right => 2,
    }
}

const WHEEL_UP: u32 = 3;
const WHEEL_DOWN: u32 = 4;

/// Merge consecutive relative motion, and keep only the last of consecutive
/// absolute positions for a head, so a burst of pointer events is one. Order
/// against keys and buttons is kept.
pub fn coalesce(events: &[InputEvent]) -> Vec<InputEvent> {
    let mut out: Vec<InputEvent> = Vec::with_capacity(events.len());
    for &e in events {
        match (e, out.last_mut()) {
            (
                InputEvent::PointerMove { dx, dy },
                Some(InputEvent::PointerMove { dx: px, dy: py }),
            ) => {
                *px += dx;
                *py += dy;
            }
            (
                InputEvent::PointerAt { head, x, y },
                Some(InputEvent::PointerAt {
                    head: ph,
                    x: px,
                    y: py,
                }),
            ) if *ph == head => {
                *px = x;
                *py = y;
            }
            _ => out.push(e),
        }
    }
    out
}

/// The messages for `events`, in order.
pub fn messages(events: &[InputEvent]) -> Vec<Input> {
    let mut out = Vec::new();
    let msg = |kind, head, code, value, x, y| Input {
        kind,
        head,
        code,
        value,
        x,
        y,
    };
    for e in events {
        match *e {
            InputEvent::Key { key, down } => {
                out.push(msg(InputKind::Key, 0, evdev(key), u32::from(down), 0, 0))
            }
            InputEvent::Button { button: b, down } => {
                out.push(msg(InputKind::Button, 0, button(b), u32::from(down), 0, 0))
            }
            InputEvent::PointerMove { dx, dy } => {
                if dx != 0 || dy != 0 {
                    out.push(msg(InputKind::Relative, 0, 0, 0, dx, dy));
                }
            }
            InputEvent::PointerAt { head, x, y } => {
                out.push(msg(InputKind::Absolute, head, 0, 0, x, y))
            }
            InputEvent::Wheel { notches } => {
                let code = if notches > 0 { WHEEL_UP } else { WHEEL_DOWN };
                for _ in 0..notches.unsigned_abs() {
                    for down in [1, 0] {
                        out.push(msg(InputKind::Button, 0, code, down, 0, 0));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_evdev_codes() {
        assert_eq!(evdev(Key::Escape), 1);
        assert_eq!(evdev(Key::A), 30);
        assert_eq!(evdev(Key::Space), 57);
        assert_eq!(evdev(Key::Down), 108);
        assert_eq!(evdev(Key::F12), 88);
        let m = messages(&[InputEvent::Key {
            key: Key::Q,
            down: true,
        }]);
        assert_eq!(
            m,
            vec![Input {
                kind: InputKind::Key,
                head: 0,
                code: 16,
                value: 1,
                x: 0,
                y: 0
            }]
        );
    }

    #[test]
    fn every_key_has_a_distinct_code_inside_the_accepted_range() {
        use std::collections::HashSet;
        let keys = [
            Key::A,
            Key::B,
            Key::C,
            Key::D,
            Key::E,
            Key::F,
            Key::G,
            Key::H,
            Key::I,
            Key::J,
            Key::K,
            Key::L,
            Key::M,
            Key::N,
            Key::O,
            Key::P,
            Key::Q,
            Key::R,
            Key::S,
            Key::T,
            Key::U,
            Key::V,
            Key::W,
            Key::X,
            Key::Y,
            Key::Z,
            Key::Digit0,
            Key::Digit1,
            Key::Digit2,
            Key::Digit3,
            Key::Digit4,
            Key::Digit5,
            Key::Digit6,
            Key::Digit7,
            Key::Digit8,
            Key::Digit9,
            Key::F1,
            Key::F2,
            Key::F3,
            Key::F4,
            Key::F5,
            Key::F6,
            Key::F7,
            Key::F8,
            Key::F9,
            Key::F10,
            Key::F11,
            Key::F12,
            Key::Escape,
            Key::Tab,
            Key::CapsLock,
            Key::Space,
            Key::Enter,
            Key::Backspace,
            Key::Minus,
            Key::Equal,
            Key::BracketLeft,
            Key::BracketRight,
            Key::Backslash,
            Key::Semicolon,
            Key::Apostrophe,
            Key::Grave,
            Key::Comma,
            Key::Period,
            Key::Slash,
            Key::ShiftLeft,
            Key::ShiftRight,
            Key::ControlLeft,
            Key::ControlRight,
            Key::AltLeft,
            Key::AltRight,
            Key::MetaLeft,
            Key::MetaRight,
            Key::Menu,
            Key::Insert,
            Key::Delete,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::Up,
            Key::Down,
            Key::Left,
            Key::Right,
            Key::PrintScreen,
            Key::ScrollLock,
            Key::Pause,
        ];
        let codes: HashSet<u32> = keys.iter().map(|&k| evdev(k)).collect();
        assert_eq!(codes.len(), keys.len());
        assert!(codes.iter().all(|&c| (1..=0x2ff).contains(&c)));
    }

    #[test]
    fn buttons_are_qapi_numbers_and_a_wheel_notch_is_press_and_release() {
        let m = messages(&[
            InputEvent::Button {
                button: PointerButton::Right,
                down: true,
            },
            InputEvent::Wheel { notches: -2 },
        ]);
        assert_eq!(m[0].code, 2);
        assert_eq!(m.len(), 5);
        assert!(m[1..]
            .iter()
            .all(|i| i.kind == InputKind::Button && i.code == 4));
        assert_eq!(m[1].value, 1);
        assert_eq!(m[2].value, 0);
    }

    #[test]
    fn motion_is_merged_and_the_last_absolute_position_wins() {
        let c = coalesce(&[
            InputEvent::PointerMove { dx: 2, dy: 0 },
            InputEvent::PointerMove { dx: 3, dy: -4 },
            InputEvent::Button {
                button: PointerButton::Left,
                down: true,
            },
            InputEvent::PointerAt {
                head: 0,
                x: 1,
                y: 1,
            },
            InputEvent::PointerAt {
                head: 0,
                x: 5,
                y: 6,
            },
            InputEvent::PointerAt {
                head: 1,
                x: 9,
                y: 9,
            },
        ]);
        assert_eq!(c.len(), 4);
        assert_eq!(c[0], InputEvent::PointerMove { dx: 5, dy: -4 });
        assert_eq!(
            c[2],
            InputEvent::PointerAt {
                head: 0,
                x: 5,
                y: 6
            }
        );
        let m = messages(&c);
        assert_eq!(m[0].kind, InputKind::Relative);
        assert_eq!((m[0].x, m[0].y), (5, -4));
        assert_eq!(
            m[2],
            Input {
                kind: InputKind::Absolute,
                head: 0,
                code: 0,
                value: 0,
                x: 5,
                y: 6
            }
        );
        assert!(messages(&[InputEvent::PointerMove { dx: 0, dy: 0 }]).is_empty());
    }
}
