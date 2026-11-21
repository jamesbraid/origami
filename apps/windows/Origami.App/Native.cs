using System.Runtime.InteropServices;

namespace Origami.App;

[StructLayout(LayoutKind.Sequential)]
public struct PresentCounters
{
    public ulong Presents, Skipped, CpuBytesCopiedPicture, ReadbackBytesCheckOnly, LastMismatch, LastSeq,
                 PresentUsTotal, PresentUsMax;
    public uint Width, Height, IsWarp, Reserved;
}

/// <summary>The fallback presenter (native/): D3D12 into a window, fed with the core's copy-path frames.</summary>
public static unsafe class Presenter
{
    const string Dll = "origami_present.dll";
    [DllImport(Dll)] public static extern IntPtr op_create(IntPtr hwnd, uint w, uint h, int warp);
    [DllImport(Dll)] public static extern int op_apply_rows(IntPtr h, byte* pixels, nuint nbytes, void* runs, nuint nruns);
    [DllImport(Dll)] public static extern int op_present(IntPtr h, ulong seq, int verify);
    [DllImport(Dll)] public static extern nint op_screendump(IntPtr h, byte* outp, nuint len);
    [DllImport(Dll)] public static extern void op_counters(IntPtr h, out PresentCounters c);
    [DllImport(Dll)] public static extern void op_destroy(IntPtr h);
    [DllImport(Dll)] static extern nuint op_last_error(byte* outp, nuint len);

    public static string LastError()
    {
        var b = new byte[512];
        fixed (byte* p = b) { var n = (int)op_last_error(p, (nuint)b.Length); return System.Text.Encoding.UTF8.GetString(b, 0, n); }
    }
}

/// <summary>The core's C frame plane (origami_frame.h), exported by origami_ffi.dll.</summary>
public static unsafe class Frame
{
    const string Dll = "origami_ffi.dll";

    public const uint ScaleFit = 1; // fill keeping aspect; the default, 0, floors to whole steps
    public const int Ok = 0, ErrBufferTooSmall = -4, ErrUnsupported = -5, ErrNoGpu = -6;
    public const uint FrameNone = 0, FrameRows = 1;
    public const uint SurfaceWin32Hwnd = 3;
    public const uint DrawnPresented = 1, DrawnAgain = 2;

    [StructLayout(LayoutKind.Sequential)]
    public struct HeadInfo { public uint Width, Height; public ulong Seq; public uint Active, Reserved; }

    [StructLayout(LayoutKind.Sequential)]
    public struct Run { public uint Y0, Y1; }

    [StructLayout(LayoutKind.Sequential)]
    public struct FrameOut
    {
        public uint Size, Kind;
        public ulong Seq;
        public uint Width, Height, Resized, RunCount;
        public ulong BytesUsed;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Surface
    {
        public uint Size, Kind;
        public IntPtr Window, Display;
        public ulong WindowId;
        public int Screen;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct FrameCounters { public uint Size, Counted; public ulong Frames, Lines, ImportedLines, CopiedBytes; }

    [StructLayout(LayoutKind.Sequential)]
    public struct ViewCounters { public uint Size, Reserved; public ulong Presents, TakeNs, DrawNs, PresentNs; }

#if ORIGAMI_FAKE
    /// <summary>What the view's last present drew: its surface and the picture's rectangle in it, physical pixels.</summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct LastDraw { public uint Size, Reserved, SurfaceWidth, SurfaceHeight; public int X, Y; public uint Width, Height; public ulong Presents; }

    [DllImport(Dll)] public static extern int origami_view_get_last_draw(IntPtr view, ref LastDraw d);
#endif

    [DllImport(Dll)] public static extern int origami_frame_stats(IntPtr machine, ref FrameCounters c);
    [DllImport(Dll)] public static extern int origami_view_stats(IntPtr view, ref ViewCounters c);
    [DllImport(Dll)] public static extern int origami_frame_take(IntPtr machine, uint head, byte* pixels, nuint pixelsCap, Run* runs, nuint runsCap, ref FrameOut o);
    [DllImport(Dll)] public static extern int origami_frame_heads(IntPtr machine, HeadInfo* o, nuint cap, out nuint count);
    [DllImport(Dll)] public static extern int origami_frame_wake_handle(IntPtr machine, out nint handle);
    [DllImport(Dll)] public static extern int origami_view_attach(IntPtr machine, uint head, ref Surface s, uint w, uint h, double scale, out IntPtr view);
    [DllImport(Dll)] public static extern int origami_view_resize(IntPtr view, uint w, uint h);
    [DllImport(Dll)] public static extern int origami_view_set_scale(IntPtr view, double scale);
    [DllImport(Dll)] public static extern int origami_view_set_scale_mode(IntPtr view, uint mode);
    [DllImport(Dll)] public static extern int origami_view_set_visible(IntPtr view, uint visible);
    [DllImport(Dll)] public static extern int origami_view_readback(IntPtr view, byte* buf, nuint cap, out uint w, out uint h, out uint stride);
    public const uint CubeEnvStudio = 0, CubeEnvScene = 1, CubeEnvMachine = 2;
    public const uint CubeNoColour = 0xFFFFFFFF, CubeBackdropNone = 0, CubeBackdropBand = 1;
    public const uint CubePointerPress = 0, CubePointerMove = 1, CubePointerRelease = 2, CubePointerDoubleClick = 3;

    [StructLayout(LayoutKind.Sequential)]
    public struct CubeStyle
    {
        public uint Size, Env, Scene, HorizonRgb, GraniteLightRgb, GraniteDarkRgb, LightLook, Spin, ReduceMotion, Backdrop, SurroundRgb, Settle, FillPercent;
    }

    [DllImport(Dll)] public static extern int origami_cube_attach(ref Surface s, uint w, uint h, double scale, out IntPtr cube);
    [DllImport(Dll)] public static extern int origami_cube_attach_offscreen(uint w, uint h, double scale, out IntPtr cube);
    [DllImport(Dll)] public static extern int origami_cube_pointer(IntPtr cube, uint phase, float x, float y, ulong timeMs);
    [DllImport(Dll)] public static extern int origami_cube_resize(IntPtr cube, uint w, uint h, double scale);
    [DllImport(Dll)] public static extern int origami_cube_set_visible(IntPtr cube, uint visible);
    [DllImport(Dll)] public static extern int origami_cube_set_style(IntPtr cube, ref CubeStyle style);
    [DllImport(Dll)] public static extern uint origami_cube_scene_count();
    [DllImport(Dll)] public static extern uint origami_cube_scene_for_time(uint hour, uint minute);
    [DllImport(Dll)] public static extern int origami_cube_next_scene(IntPtr cube);
    [DllImport(Dll)] public static extern int origami_cube_current_scene(IntPtr cube);
    [DllImport(Dll)] public static extern nuint origami_cube_scene_text(uint index, uint field, byte* buf, nuint cap);
    [DllImport(Dll)] public static extern int origami_cube_set_machine(IntPtr cube, IntPtr machine, uint head);
    [DllImport(Dll)] public static extern int origami_cube_present(IntPtr cube, out uint drawn, out uint moving);
    [DllImport(Dll)] public static extern int origami_cube_readback(IntPtr cube, byte* buf, nuint cap, out uint w, out uint h, out uint stride);
    [DllImport(Dll)] public static extern void origami_cube_detach(IntPtr cube);

    /// <summary>A scene's text: field 0 id, 1 title, 2 credit line, 3 source URL.</summary>
    public static string SceneText(uint index, uint field)
    {
        var b = new byte[1024];
        fixed (byte* p = b) { var n = (int)origami_cube_scene_text(index, field, p, (nuint)b.Length); return System.Text.Encoding.UTF8.GetString(b, 0, Math.Min(n, b.Length - 1)); }
    }

    [DllImport(Dll)] public static extern int origami_view_present(IntPtr view, out uint drawn);
    [DllImport(Dll)] public static extern void origami_view_detach(IntPtr view);
    [DllImport(Dll)] public static extern void origami_machine_release(IntPtr machine);
    [DllImport(Dll)] static extern nuint origami_last_error(byte* buf, nuint cap);

    public static string LastError()
    {
        var b = new byte[512];
        fixed (byte* p = b) { var n = (int)origami_last_error(p, (nuint)b.Length); return System.Text.Encoding.UTF8.GetString(b, 0, Math.Min(n, b.Length - 1)); }
    }
}

public static class Win32
{
    [DllImport("user32")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32")] public static extern int ShowCursor(bool show);
    [DllImport("user32")] public static extern bool ClipCursor(ref RECT r);
    [DllImport("user32")] public static extern bool ClipCursor(IntPtr none);
    [DllImport("user32")] public static extern bool ClientToScreen(IntPtr hwnd, ref POINT p);
    [DllImport("user32")] public static extern bool GetClientRect(IntPtr hwnd, out RECT r);
    [DllImport("user32")] public static extern IntPtr SetFocus(IntPtr hwnd);
    [DllImport("user32")] public static extern IntPtr SetCapture(IntPtr hwnd);
    [DllImport("user32")] public static extern int GetMessageTime();
    [DllImport("user32")] public static extern bool ReleaseCapture();
    [DllImport("user32")] public static extern IntPtr GetFocus();
    [DllImport("user32")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32")] public static extern bool DestroyWindow(IntPtr hwnd);
    [DllImport("user32")] public static extern bool ShowWindow(IntPtr hwnd, int cmd);
    [DllImport("user32")] public static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32")] public static extern IntPtr SendMessageW(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32")] public static extern IntPtr DefWindowProcW(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32", CharSet = CharSet.Unicode)] public static extern ushort RegisterClassExW(ref WNDCLASSEX c);
    [DllImport("user32", CharSet = CharSet.Unicode)]
    public static extern IntPtr CreateWindowExW(uint ex, string cls, string? name, uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr inst, IntPtr param);
    [DllImport("user32")] public static extern IntPtr LoadCursorW(IntPtr inst, IntPtr name);
    [DllImport("gdi32")] public static extern IntPtr GetStockObject(int i);
    [DllImport("kernel32")] public static extern IntPtr GetModuleHandleW(string? name);
    [DllImport("kernel32")] public static extern uint WaitForSingleObject(IntPtr h, uint ms);
    [DllImport("user32")] public static extern short GetKeyState(int vk);

    public delegate IntPtr WndProc(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);

    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct WNDCLASSEX
    {
        public uint cbSize, style;
        public WndProc lpfnWndProc;
        public int cbClsExtra, cbWndExtra;
        public IntPtr hInstance, hIcon, hCursor, hbrBackground;
        public string? lpszMenuName, lpszClassName;
        public IntPtr hIconSm;
    }
}
