namespace Origami.Logic;

/// <summary>A command from the core's table (or one only this app has), with its Windows shortcut.</summary>
public sealed record CommandInfo(string Id, string Title, string? WindowsAccelerator)
{
    /// <summary>The menu this command lives in, from its ID prefix.</summary>
    public string Menu => Id.Split('.')[0] switch
    {
        "machine" => "Machine",
        "window" => "Window",
        "view" => "View",
        _ => "File",
    };
}

/// <summary>A shortcut as the core writes it: modifiers then one key joined by '+', for example "Ctrl+Shift+R".</summary>
public readonly record struct Accelerator(bool Ctrl, bool Shift, bool Alt, string Key)
{
    public static bool TryParse(string? text, out Accelerator a)
    {
        a = default;
        if (string.IsNullOrWhiteSpace(text)) return false;
        bool ctrl = false, shift = false, alt = false;
        var parts = text.Split('+');
        for (int i = 0; i < parts.Length - 1; i++)
        {
            switch (parts[i])
            {
                case "Ctrl": ctrl = true; break;
                case "Shift": shift = true; break;
                case "Alt": alt = true; break;
                default: return false; // Cmd and anything unknown are not Windows modifiers
            }
        }
        var key = parts[^1];
        if (key.Length == 0) return false;
        a = new Accelerator(ctrl, shift, alt, key);
        return true;
    }

    /// <summary>The Windows virtual-key code for the key name, or 0 for one this app does not map.</summary>
    public int VirtualKey
    {
        get
        {
            if (Key.Length == 1 && char.IsAsciiLetterOrDigit(Key[0])) return char.ToUpperInvariant(Key[0]);
            if (Key.Length is 2 or 3 && Key[0] == 'F' && int.TryParse(Key.AsSpan(1), out var n) && n is >= 1 and <= 24) return 0x70 + n - 1;
            return Key switch { "," => 0xBC, "." => 0xBE, "Enter" => 0x0D, _ => 0 };
        }
    }

    /// <summary>Whether the shortcut is safe to take while the guest has the keyboard: the guest keeps bare Ctrl letters.</summary>
    public bool TakenOverGuest => (Ctrl && Shift) || (VirtualKey is >= 0x70 and <= 0x87);
}
