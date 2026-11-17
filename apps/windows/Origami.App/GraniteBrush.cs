using System.Runtime.InteropServices.WindowsRuntime;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Origami.Logic;
using Windows.Graphics.Imaging;
using Windows.Storage;

namespace Origami.App;

/// <summary>The granite tile, recoloured to two tones and repeated, each tile pixel doubled at 2x and never smoothed.</summary>
public static class GraniteBrush
{
    static (byte[] Bgra, int W, int H)? s_tile;

    static async Task<(byte[] Bgra, int W, int H)?> Tile()
    {
        if (s_tile != null) return s_tile;
        var path = Path.Combine(AppContext.BaseDirectory, "identity", "granite-tile.png");
        if (!File.Exists(path)) return null;
        var file = await StorageFile.GetFileFromPathAsync(path);
        using var stream = await file.OpenReadAsync();
        var dec = await BitmapDecoder.CreateAsync(stream);
        var px = await dec.GetPixelDataAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Ignore, new BitmapTransform(), ExifOrientationMode.IgnoreExifOrientation, ColorManagementMode.DoNotColorManage);
        return s_tile = (px.DetachPixelData(), (int)dec.PixelWidth, (int)dec.PixelHeight);
    }

    /// <summary>A brush covering at least <paramref name="w"/> by <paramref name="h"/> device pixels, drawn one device pixel per
    /// bitmap pixel at any scale; null if the tile is missing. The fill runs off the UI thread.</summary>
    public static async Task<ImageBrush?> Make(int w, int h, double scale, uint hi, uint lo)
    {
        if (await Tile() is not { } t || w <= 0 || h <= 0) return null;
        var px = await Task.Run(() => Granite.Fill(t.Bgra, t.W, t.H, w, h, (int)Math.Round(scale), hi, lo));
        var bmp = new WriteableBitmap(w, h);
        using (var s = bmp.PixelBuffer.AsStream()) s.Write(px, 0, px.Length);
        bmp.Invalidate();
        // Stretch.None draws one bitmap pixel per DIP; undo the display scale so each lands on one device pixel.
        return new ImageBrush
        {
            ImageSource = bmp, Stretch = Stretch.None, AlignmentX = AlignmentX.Left, AlignmentY = AlignmentY.Top,
            Transform = new ScaleTransform { ScaleX = 1 / scale, ScaleY = 1 / scale },
        };
    }
}
