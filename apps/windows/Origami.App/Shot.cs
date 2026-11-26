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

    [System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
    struct Rc { public int L, T, R, B; }
    [System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
    struct BmiHeader { public uint Size; public int Width, Height; public ushort Planes, Bits; public uint Compression, ImageSize; public int Xppm, Yppm; public uint Used, Important; }
    [System.Runtime.InteropServices.DllImport("user32")] static extern bool GetWindowRect(IntPtr h, out Rc r);
    [System.Runtime.InteropServices.DllImport("user32")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [System.Runtime.InteropServices.DllImport("user32")] static extern IntPtr GetDC(IntPtr h);
    [System.Runtime.InteropServices.DllImport("user32")] static extern int ReleaseDC(IntPtr h, IntPtr dc);
    [System.Runtime.InteropServices.DllImport("gdi32")] static extern bool BitBlt(IntPtr dst, int x, int y, int w, int h, IntPtr src, int sx, int sy, uint rop);
    [System.Runtime.InteropServices.DllImport("gdi32")] static extern IntPtr CreateCompatibleDC(IntPtr dc);
    [System.Runtime.InteropServices.DllImport("gdi32")] static extern IntPtr CreateCompatibleBitmap(IntPtr dc, int w, int h);
    [System.Runtime.InteropServices.DllImport("gdi32")] static extern IntPtr SelectObject(IntPtr dc, IntPtr o);
    [System.Runtime.InteropServices.DllImport("gdi32")] static extern bool DeleteObject(IntPtr o);
    [System.Runtime.InteropServices.DllImport("gdi32")] static extern bool DeleteDC(IntPtr dc);
    [System.Runtime.InteropServices.DllImport("gdi32")] static extern int GetDIBits(IntPtr dc, IntPtr bmp, uint start, uint lines, byte[] bits, ref BmiHeader bi, uint usage);

    /// <summary>The whole window as the compositor draws it, popups and the picture's child window included, saved as a PNG.</summary>
    public static async Task Window(IntPtr hwnd, string path)
    {
        GetWindowRect(hwnd, out var r);
        int w = r.R - r.L, h = r.B - r.T;
        var screen = GetDC(IntPtr.Zero);
        var dc = CreateCompatibleDC(screen);
        var bmp = CreateCompatibleBitmap(screen, w, h);
        var old = SelectObject(dc, bmp);
        var bytes = new byte[w * h * 4];
        try
        {
            PrintWindow(hwnd, dc, 2); // PW_RENDERFULLCONTENT
            var bi = new BmiHeader { Size = 40, Width = w, Height = -h, Planes = 1, Bits = 32 };
            GetDIBits(dc, bmp, 0, (uint)h, bytes, ref bi, 0);
        }
        finally { SelectObject(dc, old); DeleteObject(bmp); DeleteDC(dc); ReleaseDC(IntPtr.Zero, screen); }
        await SavePng(path, bytes, w, h);
    }

    /// <summary>The window's rectangle as the screen shows it, which has the menus and flyouts that are windows of their own
    /// and that PrintWindow leaves out.</summary>
    public static async Task Screen(IntPtr hwnd, string path)
    {
        GetWindowRect(hwnd, out var r);
        int w = r.R - r.L, h = r.B - r.T;
        var screen = GetDC(IntPtr.Zero);
        var dc = CreateCompatibleDC(screen);
        var bmp = CreateCompatibleBitmap(screen, w, h);
        var old = SelectObject(dc, bmp);
        var bytes = new byte[w * h * 4];
        try
        {
            BitBlt(dc, 0, 0, w, h, screen, r.L, r.T, 0x00CC0020 | 0x40000000); // SRCCOPY | CAPTUREBLT
            var bi = new BmiHeader { Size = 40, Width = w, Height = -h, Planes = 1, Bits = 32 };
            GetDIBits(dc, bmp, 0, (uint)h, bytes, ref bi, 0);
        }
        finally { SelectObject(dc, old); DeleteObject(bmp); DeleteDC(dc); ReleaseDC(IntPtr.Zero, screen); }
        for (var i = 3; i < bytes.Length; i += 4) bytes[i] = 255;
        await SavePng(path, bytes, w, h);
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
