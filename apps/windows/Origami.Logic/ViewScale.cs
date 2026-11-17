namespace Origami.Logic;

/// <summary>Fit to Window (the default) takes whole device pixels per guest pixel while that is at least 1 DIP, else fills fractionally.
/// Scale 1x and 2x are 1 and 2 DIPs per guest pixel and fall back to Fit; Actual Size is 1 device pixel.</summary>
public enum ViewMode { FitToWindow, Scale1x, Scale2x, ActualSize }

public static class ViewScale
{
    /// <summary>Device pixels per guest pixel for a pane of <paramref name="availW"/> by <paramref name="availH"/> device pixels
    /// at <paramref name="displayScale"/> device pixels per DIP.</summary>
    public static double Factor(ViewMode mode, int headW, int headH, double availW, double availH, double displayScale)
    {
        if (headW <= 0 || headH <= 0 || availW <= 0 || availH <= 0) return 1;
        var f = Math.Min(availW / headW, availH / headH);
        var w = Math.Floor(f + 1e-9);
        var fitted = w >= displayScale ? w : f;
        return mode switch
        {
            ViewMode.ActualSize => Math.Min(1.0, f),
            ViewMode.Scale1x => displayScale <= f + 1e-9 ? displayScale : fitted,
            ViewMode.Scale2x => 2 * displayScale <= f + 1e-9 ? 2 * displayScale : fitted,
            _ => fitted,
        };
    }
}
