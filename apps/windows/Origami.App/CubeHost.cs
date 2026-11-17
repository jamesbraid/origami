using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

/// <summary>The tube cube: a child window the core draws into, placed over this element. It takes no pointer input,
/// and a timer draws only while the cube moves.</summary>
public sealed class CubeHost : Grid
{
    const uint WM_ERASEBKGND = 0x14, WM_LBUTTONDOWN = 0x201;
    const long MK_SHIFT = 0x4;
    const string ClassName = "OrigamiCube";
    static Win32.WndProc? s_proc;
    static bool s_registered;

    IntPtr _hwnd, _cube;
    DispatcherQueueTimer? _timer;
    bool _visible = true;
    (int, int, int, int, double) _last;
    Frame.CubeStyle _style;
    (uint, uint, uint?, bool, bool, bool, uint?)? _styleKey;

    static readonly Dictionary<IntPtr, CubeHost> s_hosts = new();

    /// <summary>A left click with Shift held, on the cube.</summary>
    public event Action? ShiftClicked;

    public Func<IntPtr> WindowHandle { get; set; } = () => IntPtr.Zero;
    public bool Attached => _cube != IntPtr.Zero;
    /// <summary>Whether the core said the cube is still moving at the last draw, for the self-test.</summary>
    public bool Moving { get; private set; }
    public string? LastError { get; private set; }
    public int Status { get; private set; }

    public CubeHost()
    {
        SizeChanged += (_, _) => Place();
        LayoutUpdated += (_, _) => Place();
        Unloaded += (_, _) => Close();
    }

    /// <summary>How the cube looks; a repeat of the current style is ignored. Draws one frame, and more while it moves.
    /// With <paramref name="surround"/> (0xRRGGBB) the cube has no granite and the child window is filled with that colour,
    /// which a child window cannot do as alpha.</summary>
    public void SetStyle(uint env, uint scene, uint? horizon, bool dark, bool spin, bool reduceMotion, uint? surround = null)
    {
        var key = (env, scene, horizon, dark, spin, reduceMotion, surround);
        if (_styleKey == key) return;
        _styleKey = key;
        _style = new Frame.CubeStyle
        {
            Size = (uint)System.Runtime.InteropServices.Marshal.SizeOf<Frame.CubeStyle>(),
            Env = env, Scene = scene, HorizonRgb = horizon ?? Frame.CubeNoColour,
            GraniteLightRgb = dark ? Granite.DarkLight : Granite.LightLight,
            GraniteDarkRgb = dark ? Granite.DarkDark : Granite.LightDark,
            LightLook = dark ? 0u : 1u, Spin = spin ? 1u : 0u, ReduceMotion = reduceMotion ? 1u : 0u,
            Backdrop = surround is null ? Frame.CubeBackdropGranite : Frame.CubeBackdropNone, SurroundRgb = surround ?? 0,
        };
        if (_cube != IntPtr.Zero) { Frame.origami_cube_set_style(_cube, ref _style); Pump(); }
    }

    /// <summary>Fades to the next photograph; the scene on show changes at once.</summary>
    public void NextScene()
    {
        if (_cube == IntPtr.Zero) return;
        Frame.origami_cube_next_scene(_cube);
        Pump();
    }

    /// <summary>The photograph on show, an index into the core's scenes; -1 for none.</summary>
    public int CurrentScene => _cube == IntPtr.Zero ? -1 : Frame.origami_cube_current_scene(_cube);

    /// <summary>Self-test hook: a left click on the cube's window, with or without Shift.</summary>
    public void ClickForTest(bool shift) { if (_hwnd != IntPtr.Zero) Win32.SendMessageW(_hwnd, WM_LBUTTONDOWN, new IntPtr(shift ? MK_SHIFT : 0), IntPtr.Zero); }

    public void SetMachine(IntPtr machine)
    {
        if (_cube != IntPtr.Zero) { Frame.origami_cube_set_machine(_cube, machine, 0); Pump(); }
    }

    public void Show(bool show)
    {
        _visible = show;
        Place();
    }

    /// <summary>The cube as drawn now, tight BGRX (a self-test call: it draws again and waits for the GPU).</summary>
    public unsafe byte[]? Readback(out int w, out int h)
    {
        w = h = 0;
        if (_cube == IntPtr.Zero) return null;
        Frame.origami_cube_readback(_cube, null, 0, out var uw, out var uh, out var stride);
        var buf = new byte[stride * uh];
        int st;
        fixed (byte* p = buf) st = Frame.origami_cube_readback(_cube, p, (nuint)buf.Length, out uw, out uh, out stride);
        if (st != Frame.Ok) { LastError = "cube_readback: " + Frame.LastError(); return null; }
        w = (int)uw; h = (int)uh;
        return buf;
    }

    public void Close()
    {
        _visible = false; // a layout pass after this must not make the window again
        _timer?.Stop(); _timer = null;
        if (_cube != IntPtr.Zero) { Frame.origami_cube_detach(_cube); _cube = IntPtr.Zero; }
        if (_hwnd != IntPtr.Zero) { s_hosts.Remove(_hwnd); Win32.DestroyWindow(_hwnd); _hwnd = IntPtr.Zero; }
    }

    // ---- placement --------------------------------------------------------------------------

    void Place()
    {
        var root = XamlRoot;
        var show = _visible && root != null && ActualWidth >= 8 && ActualHeight >= 8;
        if (!show)
        {
            if (_hwnd != IntPtr.Zero) { Win32.ShowWindow(_hwnd, 0); if (_cube != IntPtr.Zero) Frame.origami_cube_set_visible(_cube, 0); _timer?.Stop(); }
            return;
        }
        var s = root!.RasterizationScale;
        var o = TransformToVisual(null).TransformPoint(new Windows.Foundation.Point(0, 0));
        int x = (int)Math.Round(o.X * s), y = (int)Math.Round(o.Y * s), w = (int)Math.Round(ActualWidth * s), h = (int)Math.Round(ActualHeight * s);
        if (!EnsureWindow()) return;
        var again = (x, y, w, h, s);
        var wasHidden = _last == default || !Win32.IsWindowVisible(_hwnd);
        if (again == _last && !wasHidden) return;
        _last = again;
        Win32.SetWindowPos(_hwnd, IntPtr.Zero, x, y, w, h, 0x0010 | 0x0040); // NOACTIVATE | SHOWWINDOW
        if (_cube == IntPtr.Zero)
        {
            var surface = new Frame.Surface { Kind = Frame.SurfaceWin32Hwnd, Window = _hwnd };
            surface.Size = (uint)System.Runtime.InteropServices.Marshal.SizeOf<Frame.Surface>();
            Status = Frame.origami_cube_attach(ref surface, (uint)w, (uint)h, s, out _cube);
            if (Status != Frame.Ok) { LastError = $"cube_attach {Status}: {Frame.LastError()}"; _cube = IntPtr.Zero; return; }
            LastError = null;
            if (_style.Size != 0) Frame.origami_cube_set_style(_cube, ref _style);
        }
        else
        {
            Frame.origami_cube_resize(_cube, (uint)w, (uint)h, s);
            Frame.origami_cube_set_visible(_cube, 1);
        }
        Pump();
    }

    bool EnsureWindow()
    {
        if (_hwnd != IntPtr.Zero) return true;
        var parent = WindowHandle();
        if (parent == IntPtr.Zero) return false;
        if (!s_registered)
        {
            s_proc = Proc;
            var wc = new Win32.WNDCLASSEX
            {
                cbSize = (uint)System.Runtime.InteropServices.Marshal.SizeOf<Win32.WNDCLASSEX>(),
                lpfnWndProc = s_proc,
                hInstance = Win32.GetModuleHandleW(null),
                hCursor = Win32.LoadCursorW(IntPtr.Zero, new IntPtr(32512)),
                hbrBackground = Win32.GetStockObject(4),
                lpszClassName = ClassName,
            };
            if (Win32.RegisterClassExW(ref wc) == 0) { LastError = "RegisterClassEx failed"; return false; }
            s_registered = true;
        }
        _hwnd = Win32.CreateWindowExW(0, ClassName, null, 0x40000000 | 0x04000000 | 0x02000000, 0, 0, 16, 16, parent, IntPtr.Zero, Win32.GetModuleHandleW(null), IntPtr.Zero);
        if (_hwnd == IntPtr.Zero) { LastError = "CreateWindowEx failed"; return false; }
        s_hosts[_hwnd] = this;
        return true;
    }

    // ---- drawing ---------------------------------------------------------------------------

    void Pump()
    {
        if (_cube == IntPtr.Zero) return;
        if (Frame.origami_cube_present(_cube, out _, out var moving) != Frame.Ok) { LastError = "cube_present: " + Frame.LastError(); return; }
        Moving = moving != 0;
        if (moving != 0)
        {
            _timer ??= CreateTimer();
            _timer.Start();
        }
        else _timer?.Stop();
    }

    DispatcherQueueTimer CreateTimer()
    {
        var t = DispatcherQueue.CreateTimer();
        t.Interval = TimeSpan.FromMilliseconds(16);
        t.IsRepeating = true;
        t.Tick += (_, _) => Pump();
        return t;
    }

    static IntPtr Proc(IntPtr hwnd, uint msg, IntPtr w, IntPtr l)
    {
        if (msg == WM_ERASEBKGND) return new IntPtr(1);
        if (msg == WM_LBUTTONDOWN && (w.ToInt64() & MK_SHIFT) != 0 && s_hosts.TryGetValue(hwnd, out var host)) host.ShiftClicked?.Invoke();
        return Win32.DefWindowProcW(hwnd, msg, w, l);
    }
}
