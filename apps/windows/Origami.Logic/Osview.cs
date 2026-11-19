namespace Origami.Logic;

/// <summary>One legend entry of a bar, which is also its segment's name. <see cref="Rgb"/> is 0xRRGGBB.</summary>
public sealed record OsviewKey(string Label, uint Rgb);

/// <summary>A share of a bar's length, 0 to 1, in order from the left.</summary>
public sealed record OsviewSegment(double Fraction, uint Rgb);

public sealed record OsviewBar(string Title, IReadOnlyList<OsviewKey> Legend, IReadOnlyList<OsviewSegment> Segments);

/// <summary>The activity view as the core describes it, for the app to paint; with no bars, <see cref="Note"/> says why.</summary>
public sealed record OsviewScene(IReadOnlyList<OsviewBar> Bars, uint Ticks, uint Background, uint BarBackground, string Caption, string? Note);

public static class CpuPanel
{
    public static readonly TimeSpan Interval = TimeSpan.FromMilliseconds(500); // 2 Hz

    /// <summary>The scene to show when the core cannot be asked; the caption is the last one the core gave.</summary>
    public static OsviewScene Message(string why, string caption = "") => new(Array.Empty<OsviewBar>(), 10, 0xBFBFBF, 0x000000, caption, why);

    /// <summary>What to show when there are no bars.</summary>
    public static string Empty(OsviewScene v) => string.IsNullOrWhiteSpace(v.Note) ? "measuring…" : v.Note!;

    /// <summary>A bar in words, for screen readers and the self-test: its title, then each segment's percentage and name.</summary>
    public static string Summary(OsviewBar b) =>
        $"{b.Title}: " + string.Join(", ", b.Legend.Zip(b.Segments, (k, s) => $"{Math.Round(Math.Clamp(s.Fraction, 0, 1) * 100):0}% {k.Label}"));
}
