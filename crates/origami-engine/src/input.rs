//! Input as the guest's keyboard and mouse see it, independent of any
//! toolkit's key type.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}

/// A key on a US keyboard. These are positions, not characters: the guest
/// does its own layout, so `Key::Digit1` is the key whether or not shift is
/// down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Escape,
    Tab,
    CapsLock,
    Space,
    Enter,
    Backspace,
    Minus,
    Equal,
    BracketLeft,
    BracketRight,
    Backslash,
    Semicolon,
    Apostrophe,
    Grave,
    Comma,
    Period,
    Slash,
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    MetaLeft,
    MetaRight,
    Menu,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    PrintScreen,
    ScrollLock,
    Pause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Key {
        key: Key,
        down: bool,
    },
    /// Relative motion in guest pointer counts, for a front end without a
    /// cursor to place; the tab view uses [`InputEvent::PointerAt`].
    #[allow(dead_code)]
    PointerMove {
        dx: i32,
        dy: i32,
    },
    /// The pointer at a position in a head's pixels. The engine moves the
    /// guest's cursor there.
    PointerAt {
        head: u32,
        x: i32,
        y: i32,
    },
    Button {
        button: PointerButton,
        down: bool,
    },
    /// Wheel notches, positive away from the user.
    Wheel {
        notches: i32,
    },
}
