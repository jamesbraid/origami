using System.Runtime.InteropServices;

namespace Origami.App;

/// <summary>Real OS input and window focus for the self-tests.</summary>
internal static class Win
{
    [StructLayout(LayoutKind.Explicit, Size = 40)]
    struct INPUT
    {
        [FieldOffset(0)] public uint type;
        // MOUSEINPUT at 8 (dx, dy, mouseData, flags); KEYBDINPUT at 8 (wVk|wScan<<16, flags).
        [FieldOffset(8)] public int a;
        [FieldOffset(12)] public int b;
        [FieldOffset(16)] public uint c;
        [FieldOffset(20)] public uint d;
    }
    [DllImport("user32", SetLastError = true)] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32")] static extern int GetSystemMetrics(int i);
    [DllImport("user32")] static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("kernel32")] static extern uint GetCurrentThreadId();
    [DllImport("user32")] static extern bool AttachThreadInput(uint a, uint b, bool attach);
    [DllImport("user32")] static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32")] static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32")] static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);

    [StructLayout(LayoutKind.Sequential)] struct CURSORINFO { public int cbSize, flags; public IntPtr hCursor; public int x, y; }
    [DllImport("user32")] static extern bool GetCursorInfo(ref CURSORINFO c);
    [DllImport("user32")] static extern bool GetClipCursor(out Win32.RECT r);

    [DllImport("user32")] static extern IntPtr GetCapture();
    public static IntPtr CaptureOwner() => GetCapture();
    public static bool CursorHidden() { var c = new CURSORINFO { cbSize = Marshal.SizeOf<CURSORINFO>() }; GetCursorInfo(ref c); return (c.flags & 1) == 0; }
    /// <summary>True when the cursor is confined to less than the screen.</summary>
    public static bool ClipIsSmall() { GetClipCursor(out var r); return r.Right - r.Left < GetSystemMetrics(0) || r.Bottom - r.Top < GetSystemMetrics(1); }

    public static IntPtr ForegroundHandle() => GetForegroundWindow();
    public static bool IsForeground(IntPtr h) => GetForegroundWindow() == h;

    public static void Foreground(IntPtr h)
    {
        // Windows refuses this unless the caller got the last input: an Alt tap counts. Retry; the step's console steals it back.
        for (int i = 0; i < 5 && !IsForeground(h); i++)
        {
            keybd_event(0x12, 0, 0, UIntPtr.Zero); keybd_event(0x12, 0, 2, UIntPtr.Zero);
            SetWindowPos(h, new IntPtr(-1), 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0040); // TOPMOST, NOSIZE|NOMOVE|SHOWWINDOW
            ForegroundOnce(h);
            System.Threading.Thread.Sleep(150);
        }
    }

    static void ForegroundOnce(IntPtr h)
    {
        var fg = GetForegroundWindow();
        var fgThread = GetWindowThreadProcessId(fg, out _);
        var me = GetCurrentThreadId();
        if (fgThread != me) AttachThreadInput(me, fgThread, true);
        ShowWindow(h, 9); // SW_RESTORE
        BringWindowToTop(h);
        SetForegroundWindow(h);
        if (fgThread != me) AttachThreadInput(me, fgThread, false);
    }

    static void Send(INPUT i) => SendInput(1, new[] { i }, Marshal.SizeOf<INPUT>());

    public static void Scan(int scan, bool extended, bool down)
    {
        uint flags = 0x0008 | (extended ? 1u : 0u) | (down ? 0u : 2u);
        Send(new INPUT { type = 1, a = scan << 16, b = (int)flags });
    }

    public static void MouseAbs(int x, int y)
    {
        int w = GetSystemMetrics(0), h = GetSystemMetrics(1);
        Send(new INPUT { type = 0, a = (int)((long)x * 65535 / (w - 1)), b = (int)((long)y * 65535 / (h - 1)), d = 0x8001 });
    }

    public static void MouseRel(int dx, int dy) => Send(new INPUT { type = 0, a = dx, b = dy, d = 0x0001 });
    public static void MouseButton(bool down) => Send(new INPUT { type = 0, d = down ? 0x0002u : 0x0004u });
}
