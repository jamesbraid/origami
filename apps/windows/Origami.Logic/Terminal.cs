using System.Text;

namespace Origami.Logic;

/// <summary>A terminal colour: the theme default, a palette index, or true colour.</summary>
public readonly record struct TermColour(byte Kind, byte A = 0, byte B = 0, byte C = 0)
{
    public static TermColour Default => new(0);
    public static TermColour Indexed(byte i) => new(1, i);
    public static TermColour Rgb(byte r, byte g, byte b) => new(2, r, g, b);
}

public sealed record TermCell(string Text, TermColour Fg, TermColour Bg, bool Bold, bool Dim, bool Italic, bool Underline, bool Inverse, bool Wide, bool Continuation);

public sealed record TermDamage(ulong Sequence, bool Full, IReadOnlyList<int> Rows);

public enum TermKeyKind { Text, Enter, Backspace, Tab, BackTab, Escape, Up, Down, Left, Right, Home, End, PageUp, PageDown, Insert, Delete, Function }

public readonly record struct TermKey(TermKeyKind Kind, string Text = "", int Number = 0);

/// <summary>The core's terminal for one machine's console: a pump in the core feeds it, the app asks what changed.</summary>
public interface ITerminal : IDisposable
{
    (int Rows, int Cols) Size { get; }
    void Resize(int rows, int cols);
    TermCell[] Row(int index);
    (int Row, int Col, bool Visible) Cursor { get; }
    int ScrollbackCount { get; }
    TermCell[]? ScrollbackRow(int index);
    void Scroll(int lines);
    int ScrollOffset { get; }
    TermDamage ChangedSince(ulong since);
    void SendKey(TermKey key, bool shift, bool ctrl, bool alt);
    void Paste(string text);
}

public static class TermPalette
{
    static readonly uint[] Ansi =
    {
        0x000000, 0xCD3131, 0x0DBC79, 0xE5E510, 0x2472C8, 0xBC3FBC, 0x11A8CD, 0xE5E5E5,
        0x666666, 0xF14C4C, 0x23D18B, 0xF5F543, 0x3B8EEA, 0xD670D6, 0x29B8DB, 0xFFFFFF,
    };

    /// <summary>The 256-colour palette: 16 ANSI, a 6x6x6 cube, then 24 greys.</summary>
    public static uint Indexed(int i)
    {
        if (i < 16) return Ansi[i];
        if (i < 232)
        {
            i -= 16;
            static uint Level(int v) => v == 0 ? 0u : (uint)(55 + 40 * v);
            return Level(i / 36) << 16 | Level(i / 6 % 6) << 8 | Level(i % 6);
        }
        var g = (uint)(8 + 10 * (i - 232));
        return g << 16 | g << 8 | g;
    }

    /// <summary>0xRRGGBB for a cell's colour; <paramref name="themeDefault"/> stands for Default.</summary>
    public static uint Rgb(TermColour c, uint themeDefault) => c.Kind switch
    {
        1 => Indexed(c.A),
        2 => (uint)c.A << 16 | (uint)c.B << 8 | c.C,
        _ => themeDefault,
    };
}

/// <summary>A selection between two cells (row, column), reading in text order; the text is what Copy puts on the clipboard.</summary>
public sealed class TermSelection
{
    (int R, int C)? _a, _b;
    public bool Active => _a != null && _b != null && _a != _b;

    public void Begin(int row, int col) { _a = (row, col); _b = (row, col); }
    public void Extend(int row, int col) { if (_a != null) _b = (row, col); }
    public void Clear() { _a = _b = null; }

    public ((int R, int C) From, (int R, int C) To)? Range()
    {
        if (!Active) return null;
        var (x, y) = (_a!.Value, _b!.Value);
        return (x.R, x.C).CompareTo((y.R, y.C)) <= 0 ? (x, y) : (y, x);
    }

    public bool Contains(int row, int col) =>
        Range() is { } r && (row, col).CompareTo((r.From.R, r.From.C)) >= 0 && (row, col).CompareTo((r.To.R, r.To.C)) <= 0;

    /// <summary>The selected text: trailing blanks trimmed on each line, lines joined with newlines, wide characters once.</summary>
    public string Text(Func<int, IReadOnlyList<TermCell>> row, int cols)
    {
        if (Range() is not { } r) return "";
        var sb = new StringBuilder();
        for (int y = r.From.R; y <= r.To.R; y++)
        {
            var cells = row(y);
            int from = y == r.From.R ? r.From.C : 0, to = y == r.To.R ? r.To.C : cols - 1;
            var line = new StringBuilder();
            for (int x = from; x <= Math.Min(to, cells.Count - 1); x++)
                if (!cells[x].Continuation) line.Append(cells[x].Text.Length == 0 ? " " : cells[x].Text);
            sb.Append(line.ToString().TrimEnd());
            if (y < r.To.R) sb.Append('\n');
        }
        return sb.ToString();
    }
}

public static class TermKeys
{
    /// <summary>Windows virtual-key codes that are not text; null for a key the text path (CharacterReceived) handles.</summary>
    public static TermKey? FromVirtualKey(int vk) => vk switch
    {
        0x0D => new TermKey(TermKeyKind.Enter), 0x08 => new TermKey(TermKeyKind.Backspace), 0x09 => new TermKey(TermKeyKind.Tab),
        0x1B => new TermKey(TermKeyKind.Escape), 0x26 => new TermKey(TermKeyKind.Up), 0x28 => new TermKey(TermKeyKind.Down),
        0x25 => new TermKey(TermKeyKind.Left), 0x27 => new TermKey(TermKeyKind.Right), 0x24 => new TermKey(TermKeyKind.Home),
        0x23 => new TermKey(TermKeyKind.End), 0x21 => new TermKey(TermKeyKind.PageUp), 0x22 => new TermKey(TermKeyKind.PageDown),
        0x2D => new TermKey(TermKeyKind.Insert), 0x2E => new TermKey(TermKeyKind.Delete),
        >= 0x70 and <= 0x7B => new TermKey(TermKeyKind.Function, "", vk - 0x70 + 1),
        _ => null,
    };
}
