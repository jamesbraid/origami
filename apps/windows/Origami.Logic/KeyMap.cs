namespace Origami.Logic;

/// <summary>Windows scan codes (set 1) to evdev codes: key positions, so the host layout does not matter.</summary>
public static class KeyMap
{
    // evdev codes 1..88 equal the set-1 make codes for the main block.
    static readonly Dictionary<int, int> Extended = new()
    {
        [0x1C] = 96,  // keypad enter
        [0x1D] = 97,  // right ctrl
        [0x35] = 98,  // keypad slash
        [0x37] = 99,  // print screen
        [0x38] = 100, // right alt
        [0x47] = 102, // home
        [0x48] = 103, // up
        [0x49] = 104, // page up
        [0x4B] = 105, // left
        [0x4D] = 106, // right
        [0x4F] = 107, // end
        [0x50] = 108, // down
        [0x51] = 109, // page down
        [0x52] = 110, // insert
        [0x53] = 111, // delete
        [0x5B] = 125, // left windows
        [0x5C] = 126, // right windows
        [0x5D] = 127, // menu
    };

    public const int LeftCtrl = 29, RightCtrl = 97, LeftAlt = 56, RightAlt = 100, Enter = 28;

    /// <summary>Returns the evdev code, or 0 when the key has no guest position.</summary>
    public static int FromScanCode(int scanCode, bool extended)
    {
        if (extended) return scanCode == 0x45 ? 69 : Extended.TryGetValue(scanCode, out var e) ? e : 0;
        // Windows reports NumLock as 0x45 with the extended flag and Pause as 0x45 without it.
        if (scanCode == 0x45) return 119;
        return scanCode is >= 1 and <= 88 ? scanCode : 0;
    }

    public static bool IsCtrl(int evdev) => evdev is LeftCtrl or RightCtrl;
    public static bool IsAlt(int evdev) => evdev is LeftAlt or RightAlt;

    /// <summary>Unshifted ASCII for a letter, digit or space key; used by the fake console.</summary>
    public static char? Ascii(int evdev)
    {
        const string row1 = "1234567890";   // evdev 2..11
        const string q = "qwertyuiop";       // 16..25
        const string a = "asdfghjkl";        // 30..38
        const string z = "zxcvbnm";          // 44..50
        if (evdev is >= 2 and <= 11) return row1[evdev - 2];
        if (evdev is >= 16 and <= 25) return q[evdev - 16];
        if (evdev is >= 30 and <= 38) return a[evdev - 30];
        if (evdev is >= 44 and <= 50) return z[evdev - 44];
        if (evdev == 57) return ' ';
        return null;
    }
}

/// <summary>The core's keys (<c>OrigamiKey</c>, in this order) and their evdev codes. </summary>
public static class GuestKeys
{
    public static readonly string[] Names =
    {
        "A","B","C","D","E","F","G","H","I","J","K","L","M","N","O","P","Q","R","S","T","U","V","W","X","Y","Z",
        "Digit0","Digit1","Digit2","Digit3","Digit4","Digit5","Digit6","Digit7","Digit8","Digit9",
        "F1","F2","F3","F4","F5","F6","F7","F8","F9","F10","F11","F12",
        "Escape","Tab","CapsLock","Space","Enter","Backspace","Minus","Equal","BracketLeft","BracketRight",
        "Backslash","Semicolon","Apostrophe","Grave","Comma","Period","Slash","ShiftLeft","ShiftRight",
        "ControlLeft","ControlRight","AltLeft","AltRight","MetaLeft","MetaRight","Menu","Insert","Delete",
        "Home","End","PageUp","PageDown","Up","Down","Left","Right","PrintScreen","ScrollLock","Pause",
        "NumLock","Numpad0","Numpad1","Numpad2","Numpad3","Numpad4","Numpad5","Numpad6","Numpad7","Numpad8","Numpad9",
        "NumpadAdd","NumpadSubtract","NumpadMultiply","NumpadDivide","NumpadDecimal","NumpadEnter",
    };

    static readonly Dictionary<int, string> ByEvdev = Build();

    static Dictionary<int, string> Build()
    {
        var d = new Dictionary<int, string>();
        const string q = "QWERTYUIOP", a = "ASDFGHJKL", z = "ZXCVBNM";
        for (int i = 0; i < q.Length; i++) d[16 + i] = q[i].ToString();
        for (int i = 0; i < a.Length; i++) d[30 + i] = a[i].ToString();
        for (int i = 0; i < z.Length; i++) d[44 + i] = z[i].ToString();
        for (int i = 0; i < 9; i++) d[2 + i] = "Digit" + (i + 1);
        d[11] = "Digit0";
        for (int i = 0; i < 10; i++) d[59 + i] = "F" + (i + 1);
        d[87] = "F11"; d[88] = "F12";
        d[1] = "Escape"; d[15] = "Tab"; d[58] = "CapsLock"; d[57] = "Space"; d[28] = "Enter"; d[14] = "Backspace";
        d[12] = "Minus"; d[13] = "Equal"; d[26] = "BracketLeft"; d[27] = "BracketRight"; d[43] = "Backslash";
        d[39] = "Semicolon"; d[40] = "Apostrophe"; d[41] = "Grave"; d[51] = "Comma"; d[52] = "Period"; d[53] = "Slash";
        d[42] = "ShiftLeft"; d[54] = "ShiftRight"; d[29] = "ControlLeft"; d[97] = "ControlRight";
        d[56] = "AltLeft"; d[100] = "AltRight"; d[125] = "MetaLeft"; d[126] = "MetaRight"; d[127] = "Menu";
        d[110] = "Insert"; d[111] = "Delete"; d[102] = "Home"; d[107] = "End"; d[104] = "PageUp"; d[109] = "PageDown";
        d[103] = "Up"; d[108] = "Down"; d[105] = "Left"; d[106] = "Right";
        d[99] = "PrintScreen"; d[70] = "ScrollLock"; d[119] = "Pause";
        d[69] = "NumLock"; d[82] = "Numpad0"; d[79] = "Numpad1"; d[80] = "Numpad2"; d[81] = "Numpad3"; d[75] = "Numpad4";
        d[76] = "Numpad5"; d[77] = "Numpad6"; d[71] = "Numpad7"; d[72] = "Numpad8"; d[73] = "Numpad9";
        d[78] = "NumpadAdd"; d[74] = "NumpadSubtract"; d[55] = "NumpadMultiply"; d[98] = "NumpadDivide";
        d[83] = "NumpadDecimal"; d[96] = "NumpadEnter";
        return d;
    }

    /// <summary>The core's key ordinal for an evdev code, or -1 when the core has no such key.</summary>
    public static int Ordinal(int evdev) =>
        ByEvdev.TryGetValue(evdev, out var n) ? Array.IndexOf(Names, n) : -1;
}

/// <summary>Which keys the guest believes are down, for the release chord and repeat filtering.</summary>
public sealed class KeyboardState
{
    readonly HashSet<int> _down = new();
    public int DownCount => _down.Count;
    // Left keys only: AltGr is a fake Left Ctrl plus Right Alt and must not release a capture.
    public bool CtrlDown => _down.Contains(KeyMap.LeftCtrl);
    public bool AltDown => _down.Contains(KeyMap.LeftAlt);
    public bool IsDown(int evdev) => _down.Contains(evdev);

    /// <summary>Returns false for an auto-repeat of a key already down (the guest repeats itself).</summary>
    public bool Press(int evdev) => _down.Add(evdev);
    public bool Release(int evdev) => _down.Remove(evdev);

    public List<InputEvent> ReleaseAll()
    {
        var list = _down.OrderBy(k => k).Select(k => InputEvent.Key(k, false)).ToList();
        _down.Clear();
        return list;
    }
}
