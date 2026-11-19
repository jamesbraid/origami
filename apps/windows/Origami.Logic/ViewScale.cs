namespace Origami.Logic;

/// <summary>Fit to Window (the default) takes whole device pixels per guest pixel while that is at least 1, else fills fractionally.
/// Scale 1x and 2x are 1 and 2 DIPs per guest pixel and fall back to Fit; Actual Size is 1 device pixel.</summary>
public enum ViewMode { FitToWindow, Scale1x, Scale2x, ActualSize }

/// <summary>A rectangle in device pixels.</summary>
public readonly record struct PixelRect(int X, int Y, int W, int H);

public static class ViewScale
{
    /// <summary>DIPs the picture's pane keeps clear of the guest picture on each axis (6 per side).</summary>
    public const int PanePadding = 12;

    /// <summary>Device pixels per guest pixel for a pane of <paramref name="availW"/> by <paramref name="availH"/> device pixels
    /// at <paramref name="displayScale"/> device pixels per DIP.</summary>
    public static double Factor(ViewMode mode, int headW, int headH, double availW, double availH, double displayScale)
    {
        if (headW <= 0 || headH <= 0 || availW <= 0 || availH <= 0) return 1;
        var f = Math.Min(availW / headW, availH / headH);
        var w = Math.Floor(f + 1e-9);
        var fitted = w >= 1 ? w : f;
        return mode switch
        {
            ViewMode.ActualSize => Math.Min(1.0, f),
            ViewMode.Scale1x => displayScale <= f + 1e-9 ? displayScale : fitted,
            ViewMode.Scale2x => 2 * displayScale <= f + 1e-9 ? 2 * displayScale : fitted,
            _ => fitted,
        };
    }

    /// <summary>The status bar's text: device pixels per guest pixel as a percentage, led by Fit when that is the mode.</summary>
    public static string Label(ViewMode mode, double factor) =>
        (mode == ViewMode.FitToWindow ? "Fit " : "") + (int)Math.Round(factor * 100) + "%";

    /// <summary>The window resized to <paramref name="w"/> by <paramref name="h"/>, no larger than the work area and moved inside it.</summary>
    public static PixelRect Within(PixelRect window, PixelRect work, int w, int h)
    {
        w = Math.Min(w, work.W); h = Math.Min(h, work.H);
        return new(Math.Clamp(window.X, work.X, work.X + work.W - w), Math.Clamp(window.Y, work.Y, work.Y + work.H - h), w, h);
    }

    /// <summary>The window that shows the picture at 1 DIP per guest pixel: its size changes, growing or shrinking, by what the
    /// pane lacks or has to spare, never below <paramref name="minW"/> by <paramref name="minH"/> DIPs, and stays in the work area.</summary>
    public static PixelRect WindowFor(PixelRect window, PixelRect work, double scale, double paneW, double paneH, int headW, int headH, double minW, double minH) =>
        Within(window, work,
            Math.Max((int)Math.Round(window.W + (headW + PanePadding - paneW) * scale), (int)Math.Round(minW * scale)),
            Math.Max((int)Math.Round(window.H + (headH + PanePadding - paneH) * scale), (int)Math.Round(minH * scale)));
}
