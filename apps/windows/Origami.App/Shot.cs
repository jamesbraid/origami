using System.Text;
using Microsoft.UI.Xaml;
using Origami.Logic;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace Origami.App;

/// <summary>In-process pictures for CI evidence: XAML's own render and the GPU readback.</summary>
public static class Shot
{
    public static async Task SavePng(string path, byte[] bgra, int w, int h)
    {
        using var ms = new InMemoryRandomAccessStream();
        var enc = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, ms);
        enc.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Ignore, (uint)w, (uint)h, 96, 96, bgra);
        await enc.FlushAsync();
        ms.Seek(0);
        var bytes = new byte[ms.Size];
        using (var r = new DataReader(ms)) { await r.LoadAsync((uint)ms.Size); r.ReadBytes(bytes); }
        await File.WriteAllBytesAsync(path, bytes);
    }

    /// <summary>XAML's own rendering of the element (a SwapChainPanel's content is not in it).</summary>
    public static async Task Xaml(UIElement el, string path)
    {
        var rtb = new RenderTargetBitmap();
        await rtb.RenderAsync(el);
        var buf = await rtb.GetPixelsAsync();
        var bytes = System.Runtime.InteropServices.WindowsRuntime.WindowsRuntimeBufferExtensions.ToArray(buf);
        await SavePng(path, bytes, rtb.PixelWidth, rtb.PixelHeight);
    }

    /// <summary>The frame plane's readback (BGRX) as a PNG.</summary>
    public static Task Readback(byte[] bgrx, int w, int h, int pitch, string path)
    {
        var tight = new byte[w * h * 4];
        for (int y = 0; y < h; y++) System.Buffer.BlockCopy(bgrx, y * pitch, tight, y * w * 4, w * 4);
        return SavePng(path, tight, w, h);
    }

    /// <summary>Reads a binary PPM (P6) as BGRX, the layout the presenter reads back.</summary>
    public static (int W, int H, byte[] Bgrx) ReadPpm(string path) => Ppm.ReadBgrx(File.ReadAllBytes(path));
}
