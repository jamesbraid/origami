namespace Origami.Logic;

/// <summary>A rectangle in device pixels.</summary>
public readonly record struct PixelRect(int X, int Y, int W, int H);

public static class ViewScale
{
    /// <summary>DIPs the picture's pane keeps clear of the guest picture on each axis (6 per side).</summary>
    public const int PanePadding = 12;

    /// <summary>Device pixels per guest pixel when the picture fills a pane of <paramref name="availW"/> by <paramref name="availH"/> device pixels, aspect kept.</summary>
    public static double Factor(int headW, int headH, double availW, double availH)
    {
        if (headW <= 0 || headH <= 0 || availW <= 0 || availH <= 0) return 1;
        var f = Math.Min(availW / headW, availH / headH);
        var w = Math.Round(f);
        return Math.Abs(f - w) < 1e-9 ? w : f; // a whole fill must stay whole despite float error
    }

    /// <summary>DIPs per guest pixel for a machine's first window: 1 if that fits the work area, else the largest picture that does (it fills fractionally).
    /// The pane is <paramref name="paneW"/> by <paramref name="paneH"/> DIPs inside <paramref name="window"/>.</summary>
    public static double FirstScale(PixelRect window, PixelRect work, double scale, double paneW, double paneH, int headW, int headH)
    {
        var pad = PanePadding * scale;
        var availW = work.W - (window.W - paneW * scale) - pad;
        var availH = work.H - (window.H - paneH * scale) - pad;
        return Math.Min(1, Factor(headW, headH, availW, availH) / scale);
    }

    /// <summary>The window resized to <paramref name="w"/> by <paramref name="h"/>, no larger than the work area and moved inside it.</summary>
    public static PixelRect Within(PixelRect window, PixelRect work, int w, int h)
    {
        w = Math.Min(w, work.W); h = Math.Min(h, work.H);
        return new(Math.Clamp(window.X, work.X, work.X + work.W - w), Math.Clamp(window.Y, work.Y, work.Y + work.H - h), w, h);
    }

    /// <summary>The window grown to <paramref name="want"/> on each axis where that is larger, or null if it is already as large on both.</summary>
    public static PixelRect? Grown(PixelRect window, PixelRect work, PixelRect want) =>
        want.W <= window.W && want.H <= window.H ? null : Within(window, work, Math.Max(window.W, want.W), Math.Max(window.H, want.H));

    /// <summary>The window that shows the picture at <paramref name="dipsPerGuest"/> DIPs per guest pixel: its size changes, growing or shrinking, by what the
    /// pane lacks or has to spare, never below <paramref name="minW"/> by <paramref name="minH"/> DIPs, and stays in the work area.</summary>
    public static PixelRect WindowFor(PixelRect window, PixelRect work, double scale, double paneW, double paneH, int headW, int headH, double minW, double minH, double dipsPerGuest = 1) =>
        Within(window, work,
            Math.Max((int)Math.Round(window.W + (headW * dipsPerGuest + PanePadding - paneW) * scale), (int)Math.Round(minW * scale)),
            Math.Max((int)Math.Round(window.H + (headH * dipsPerGuest + PanePadding - paneH) * scale), (int)Math.Round(minH * scale)));
}
