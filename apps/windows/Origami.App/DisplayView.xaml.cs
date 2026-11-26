using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

/// <summary>
/// The machine screen: a child window the core renders into, or the D3D12 fallback where it cannot.
/// Input is handled in the child's window procedure (a native child takes it from XAML).
/// </summary>
public sealed partial class DisplayView : UserControl
{
    const uint WM_SETFOCUS = 0x7, WM_KILLFOCUS = 0x8, WM_ERASEBKGND = 0x14, WM_KEYDOWN = 0x100, WM_KEYUP = 0x101,
               WM_SYSKEYDOWN = 0x104, WM_SYSKEYUP = 0x105, WM_MOUSEMOVE = 0x200, WM_LBUTTONDOWN = 0x201,
               WM_LBUTTONUP = 0x202, WM_RBUTTONDOWN = 0x204, WM_RBUTTONUP = 0x205, WM_MBUTTONDOWN = 0x207,
               WM_MBUTTONUP = 0x208, WM_MOUSEWHEEL = 0x20A, WM_CAPTURECHANGED = 0x215;
    const string ClassName = "OrigamiDisplay";
    static Win32.WndProc? s_proc; // the class keeps the delegate for the process lifetime
    static bool s_registered;
    static readonly Dictionary<IntPtr, DisplayView> Views = new();

    ICoreClient? _core;
    string _key = "";
    IFrames? _frames;
    IntPtr _hwnd, _view, _op;
    int _headW, _headH;
    DispatcherQueue? _ui;
    Thread? _waiter;
    volatile bool _stop;
    int _pumpQueued;
    byte[] _pixels = Array.Empty<byte>();
    Frame.Run[] _runs = Array.Empty<Frame.Run>();
    ulong _lastSeq;
    DispatcherQueueTimer? _retry; // pumps again once a present error's hold has passed
    int _failures; // consecutive failed pumps; the retry timer gives up after a few so a dead view is not polled forever
    long _holdUntil; // after a present error, skip pumps for a second instead of failing every wake

    readonly KeyboardState _keys = new();
    readonly PointerController _ptr;
    readonly HashSet<int> _consumed = new();
    bool _grabbed;
    int _swallowButton = -1, _buttonsDown, _wheel;
    int _centerScreenX, _centerScreenY;

    /// <summary>Device pixels per guest pixel as last laid out, for the self-test.</summary>
    public double LastFactor => _headW > 0 ? ScreenRect().W / (double)_headW : 0;

    public Func<IntPtr> WindowHandle { get; set; } = () => IntPtr.Zero;
    /// <summary>Gets (VK, ctrl, shift, alt) for a key pressed while not captured; true if the app took it.</summary>
    public Func<int, bool, bool, bool, bool>? Shortcut { get; set; }
    public Action? PointerModeChanged { get; set; }

    /// <summary>Whether the picture's child window is on screen; it draws over all XAML, so a dialog needs it hidden.</summary>
    public bool WindowVisible => _hwnd != IntPtr.Zero && Win32.IsWindowVisible(_hwnd);
    public bool Attached => _hwnd != IntPtr.Zero && (_view != IntPtr.Zero || _op != IntPtr.Zero);
    public bool Captured => _ptr.Captured;
    public int KeysDown => _keys.DownCount;
    public string? LastError { get; private set; }
    /// <summary>"core view" or "d3d12 copy fallback" once attached.</summary>
    public string Renderer { get; private set; } = "none";
    public int CoreViewStatus { get; private set; }
    public long Wakes, Presents;

    public PointerMode Mode
    {
        get => _ptr.Mode;
        set
        {
            if (value == _ptr.Mode) return;
            _ptr.Mode = value;
            if (_ptr.Captured) { _ptr.ForceRelease(); EndCapture(); }
            PointerModeChanged?.Invoke();
        }
    }

    public DisplayView()
    {
        InitializeComponent();
        _ptr = new PointerController(_keys);
        Surround.SizeChanged += (_, _) => { Place(); _ = PaintGranite(); };
        LayoutUpdated += (_, _) => Place();
        Loaded += (_, _) => { _root = XamlRoot; _root.Changed += OnRootChanged; _ = PaintGranite(); };
        Unloaded += (_, _) => { if (_root != null) _root.Changed -= OnRootChanged; _root = null; };
    }

    XamlRoot? _root;
    int _graniteW, _graniteH, _graniteGen;
    double _graniteScale;

    /// <summary>Self-test: the surround's granite is drawn one device pixel per bitmap pixel and covers the pane.</summary>
    public bool GraniteIsDevicePixels =>
        XamlRoot is { } r && Surround.Background is Microsoft.UI.Xaml.Media.ImageBrush
        { Transform: Microsoft.UI.Xaml.Media.ScaleTransform t, ImageSource: Microsoft.UI.Xaml.Media.Imaging.WriteableBitmap bmp }
        && Math.Abs(t.ScaleX * r.RasterizationScale - 1) < 1e-9 && bmp.PixelWidth >= Math.Ceiling(Surround.ActualWidth * r.RasterizationScale);

    void OnRootChanged(XamlRoot sender, XamlRootChangedEventArgs e) { _ = PaintGranite(); Place(); }

    /// <summary>The surround is granite, neutral dark. The bitmap is device pixels: it grows with the pane and is redone when the display scale changes.</summary>
    async Task PaintGranite()
    {
        if (XamlRoot is not { } root) return;
        var s = root.RasterizationScale;
        int w = (int)Math.Ceiling(Surround.ActualWidth * s), h = (int)Math.Ceiling(Surround.ActualHeight * s);
        if (s != _graniteScale) { _graniteScale = s; _graniteW = _graniteH = 0; }
        if (w <= _graniteW && h <= _graniteH) return;
        _graniteW = Math.Max(w, _graniteW); _graniteH = Math.Max(h, _graniteH);
        var gen = ++_graniteGen; // a newer, larger fill replaces one still being made
        if (await GraniteBrush.Make(_graniteW, _graniteH, s, Granite.DarkLight, Granite.DarkDark) is { } b && gen == _graniteGen) Surround.Background = b;
    }

    // ---- attach / teardown -----------------------------------------------------------------

    /// <summary>--force-fallback keeps the D3D12 presenter under test where the core's own renderer works; the self-test clears it for the core-view rows.</summary>
    public bool ForceFallback { get; set; } = Array.IndexOf(Environment.GetCommandLineArgs(), "--force-fallback") >= 0;

    /// <summary>Returns false while the machine has no picture yet; call again on the next display event.</summary>
    public bool Attach(ICoreClient core, string key)
    {
        if (Attached && _key == key) return true;
        Detach();
        _core = core; _key = key; _ui = DispatcherQueue;
        var d = core.Display(key, 0);
        if (!d.Active) { LastError = "no active display yet"; return false; }
        _headW = d.Width; _headH = d.Height;
        EnsureWindow();
        if (_hwnd == IntPtr.Zero) return false;
        Views[_hwnd] = this;
        _frames = core.OpenFrames(key);
        _retry = DispatcherQueue.CreateTimer();
        _retry.Interval = TimeSpan.FromMilliseconds(1000);
        _retry.IsRepeating = false;
        _retry.Tick += (_, _) => Pump();
        _hidden = false; _forcePlace = true;
        Place();

        var scale = XamlRoot?.RasterizationScale ?? 1.0;
        Win32.GetClientRect(_hwnd, out var rc);
        var s = new Frame.Surface { Kind = Frame.SurfaceWin32Hwnd, Window = _hwnd };
        s.Size = (uint)System.Runtime.InteropServices.Marshal.SizeOf<Frame.Surface>();
        CoreViewStatus = ForceFallback ? Frame.ErrNoGpu : Frame.origami_view_attach(_frames.MachineHandle, 0, ref s, (uint)Math.Max(1, rc.Right), (uint)Math.Max(1, rc.Bottom), scale, out _view);
        if (CoreViewStatus == Frame.Ok) { Renderer = "core view"; Frame.origami_view_set_scale_mode(_view, Frame.ScaleFit); }
        else if (CoreViewStatus != Frame.ErrNoGpu)
        {
            LastError = $"view_attach {CoreViewStatus}: {Frame.LastError()}";
            _view = IntPtr.Zero; Detach(); return false;
        }
        else
        {
            var why = Frame.LastError();
            _view = IntPtr.Zero;
            _op = Presenter.op_create(_hwnd, (uint)_headW, (uint)_headH, 0);
            if (_op == IntPtr.Zero) { LastError = $"core view {CoreViewStatus} ({why}); fallback: {Presenter.LastError()}"; Detach(); return false; }
            Renderer = "d3d12 copy fallback";
            LastError = $"core view status {CoreViewStatus}: {why}";
        }
        _pixels = new byte[_headW * _headH * 4];
        _runs = new Frame.Run[_headH];
        _stop = false;
        var wake = _frames.WakeHandle;
        _waiter = new Thread(() =>
        {
            while (!_stop)
            {
                if (Win32.WaitForSingleObject(wake, 200) != 0) continue; // WAIT_OBJECT_0 only
                Interlocked.Increment(ref Wakes);
                if (Interlocked.Exchange(ref _pumpQueued, 1) == 0 && !_ui!.TryEnqueue(Pump)) _pumpQueued = 0;
            }
        }) { IsBackground = true, Name = "frame-wake" };
        _waiter.Start();
        Pump(); // the wake event is auto-reset: a frame published before we waited never signalled it
        return true;
    }

    public void Detach()
    {
        // Nothing the guest thinks is held may outlive the view; teardown below must run regardless.
        if (Attached) { try { LoseFocus(); } catch (Exception e) { LastError = "release on detach: " + e.Message; } }
        if (_ptr.Captured) { _ptr.ForceRelease(); EndCapture(); }
        _stop = true;
        _retry?.Stop(); _retry = null;
        _waiter?.Join(1000); _waiter = null;
        // The view goes before the window it draws into.
        if (_view != IntPtr.Zero) { Frame.origami_view_detach(_view); _view = IntPtr.Zero; }
        if (_op != IntPtr.Zero) { Presenter.op_destroy(_op); _op = IntPtr.Zero; }
        _frames?.Dispose(); _frames = null;
        if (_hwnd != IntPtr.Zero)
        {
            Views.Remove(_hwnd);
            Hide();
        }
        Renderer = "none";
    }

    public void DestroyWindow()
    {
        Detach();
        if (_hwnd != IntPtr.Zero) { Win32.DestroyWindow(_hwnd); _hwnd = IntPtr.Zero; }
    }

    void EnsureWindow()
    {
        if (_hwnd != IntPtr.Zero) return;
        var parent = WindowHandle();
        if (parent == IntPtr.Zero) { LastError = "no parent window"; return; }
        if (!s_registered)
        {
            s_proc = Proc;
            var wc = new Win32.WNDCLASSEX
            {
                cbSize = (uint)System.Runtime.InteropServices.Marshal.SizeOf<Win32.WNDCLASSEX>(),
                lpfnWndProc = s_proc,
                hInstance = Win32.GetModuleHandleW(null),
                hCursor = Win32.LoadCursorW(IntPtr.Zero, new IntPtr(32512)), // IDC_ARROW
                hbrBackground = Win32.GetStockObject(4), // BLACK_BRUSH
                lpszClassName = ClassName,
            };
            if (Win32.RegisterClassExW(ref wc) == 0) { LastError = "RegisterClassEx failed"; return; }
            s_registered = true;
        }
        // WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN, hidden until placed
        _hwnd = Win32.CreateWindowExW(0, ClassName, null, 0x40000000 | 0x04000000 | 0x02000000, 0, 0, 16, 16, parent, IntPtr.Zero, Win32.GetModuleHandleW(null), IntPtr.Zero);
        if (_hwnd == IntPtr.Zero) LastError = "CreateWindowEx failed";
    }

    // ---- layout ----------------------------------------------------------------------------

    /// <summary>The picture's rectangle on screen, physical pixels.</summary>
    public (int X, int Y, int W, int H) ScreenRect()
    {
        Win32.GetClientRect(_hwnd, out var rc);
        var p = new Win32.POINT();
        Win32.ClientToScreen(_hwnd, ref p);
        return (p.X, p.Y, rc.Right, rc.Bottom);
    }

    bool _hidden, _forcePlace;
    (int, int, int, int, double) _lastPlace;
    /// <summary>The child window is over XAML, so anything that takes the pane (preferences) hides it.</summary>
    public void Hide()
    {
        _hidden = true; _forcePlace = true;
        if (_view != IntPtr.Zero) Frame.origami_view_set_visible(_view, 0); // a hidden view draws nothing
        if (_hwnd != IntPtr.Zero) Win32.ShowWindow(_hwnd, 0);
    }

    public void Show()
    {
        if (!_hidden || !Attached) return;
        _hidden = false; Place();
        if (_view != IntPtr.Zero) { Frame.origami_view_set_visible(_view, 1); Pump(); }
    }

    void Place()
    {
        if (_hidden || _hwnd == IntPtr.Zero || _headW <= 0 || XamlRoot is null) return;
        var s = XamlRoot.RasterizationScale;
        double aw = Surround.ActualWidth - ViewScale.PanePadding, ah = Surround.ActualHeight - ViewScale.PanePadding;
        if (aw < 16 || ah < 16) return;
        var k = ViewScale.Factor(_headW, _headH, aw * s, ah * s);
        double pw = Math.Floor(_headW * k) / s, ph = Math.Floor(_headH * k) / s; // DIPs
        var origin = Surround.TransformToVisual(null).TransformPoint(new Windows.Foundation.Point(0, 0));
        double x = origin.X + (Surround.ActualWidth - pw) / 2, y = origin.Y + (Surround.ActualHeight - ph) / 2;
        Bezel.Width = pw + 4; Bezel.Height = ph + 4;
        Bezel.Margin = new Thickness(x - origin.X - 2, y - origin.Y - 2, 0, 0);
        int px = (int)Math.Round(x * s), py = (int)Math.Round(y * s), pwp = (int)Math.Round(pw * s), php = (int)Math.Round(ph * s);
        if ((px, py, pwp, php, s) == _lastPlace && !_forcePlace) return;
        _lastPlace = (px, py, pwp, php, s); _forcePlace = false;
        // HWND_TOP, SWP_NOACTIVATE | SWP_SHOWWINDOW
        Win32.SetWindowPos(_hwnd, IntPtr.Zero, px, py, pwp, php, 0x0010 | 0x0040);
        if (_view != IntPtr.Zero) { Frame.origami_view_resize(_view, (uint)pwp, (uint)php); Frame.origami_view_set_scale(_view, s); }
    }

    // ---- presenting ------------------------------------------------------------------------

    void Pump()
    {
        Interlocked.Exchange(ref _pumpQueued, 0);
        if (_frames is null) return;
        if (Environment.TickCount64 < _holdUntil) { if (_failures <= 10) _retry?.Start(); return; }
        if (_view != IntPtr.Zero)
        {
            for (int i = 0; i < 3; i++)
            {
                if (Frame.origami_view_present(_view, out var drawn) != Frame.Ok) { LastError = "view_present: " + Frame.LastError(); _holdUntil = Environment.TickCount64 + 1000; if (++_failures <= 10) _retry?.Start(); return; }
                _failures = 0;
                if (drawn == Frame.DrawnPresented) Presents++;
                if (drawn != Frame.DrawnAgain) break;
                if (i == 2) _ui?.TryEnqueue(Pump); // still asking for another call
            }
            return;
        }
        if (_op == IntPtr.Zero) return;
        bool any = false;
        unsafe
        {
            for (int guard = 0; guard < 8; guard++)
            {
                var o = new Frame.FrameOut { Size = (uint)sizeof(Frame.FrameOut) };
                int st;
                fixed (byte* px = _pixels) fixed (Frame.Run* rn = _runs)
                    st = Frame.origami_frame_take(_frames.MachineHandle, 0, px, (nuint)_pixels.Length, rn, (nuint)_runs.Length, ref o);
                if (st == Frame.ErrBufferTooSmall)
                {
                    _pixels = new byte[Math.Max((long)o.BytesUsed, _pixels.Length)];
                    _runs = new Frame.Run[Math.Max(o.RunCount, (uint)_runs.Length)];
                    continue;
                }
                if (st != Frame.Ok) { LastError = "frame_take: " + Frame.LastError(); _holdUntil = Environment.TickCount64 + 1000; if (++_failures <= 10) _retry?.Start(); break; }
                if (o.Kind != Frame.FrameRows) break;
                if (o.Resized != 0 && (o.Width != _headW || o.Height != _headH))
                {
                    // The head changed size: rebuild the presenter for the new picture.
                    Presenter.op_destroy(_op);
                    _headW = (int)o.Width; _headH = (int)o.Height;
                    _op = Presenter.op_create(_hwnd, o.Width, o.Height, 0);
                    Place();
                    if (_op == IntPtr.Zero) { LastError = "recreate: " + Presenter.LastError(); return; }
                } // this frame holds the whole new picture: fall through and apply it
                fixed (byte* px = _pixels) fixed (Frame.Run* rn = _runs)
                    if (Presenter.op_apply_rows(_op, px, (nuint)o.BytesUsed, rn, o.RunCount) != 0) LastError = "op_apply_rows rejected a frame";
                _lastSeq = o.Seq;
                any = true;
            }
        }
        if (any && Presenter.op_present(_op, _lastSeq, 0) == 1) Presents++;
    }

    /// <summary>Check path (fallback only): present with a readback and return the BGRX picture.</summary>
    public unsafe byte[]? Verify(out PresentCounters c)
    {
        c = default;
        if (_op == IntPtr.Zero) return null;
        Presenter.op_present(_op, _lastSeq, 1);
        Presenter.op_counters(_op, out c);
        var buf = new byte[_headW * _headH * 4];
        fixed (byte* p = buf) Presenter.op_screendump(_op, p, (nuint)buf.Length);
        return buf;
    }

    public PresentCounters? FallbackCounters()
    {
        if (_op == IntPtr.Zero) return null;
        Presenter.op_counters(_op, out var c);
        return c;
    }

    /// <summary>The core's display counters: how many lines were imported in place and how many bytes the CPU copied.</summary>
    public Frame.FrameCounters? CoreFrameCounters()
    {
        if (_frames is null) return null;
        var c = new Frame.FrameCounters { Size = (uint)System.Runtime.InteropServices.Marshal.SizeOf<Frame.FrameCounters>() };
        return Frame.origami_frame_stats(_frames.MachineHandle, ref c) == Frame.Ok ? c : null;
    }

    /// <summary>What the core's view last drew, tight BGRX, via the core's readback (a self-test call: it draws again and waits for the GPU).</summary>
    public unsafe byte[]? CoreReadback(out int w, out int h, bool headSize = false)
    {
        w = h = 0;
        if (_view == IntPtr.Zero) return null;
        if (headSize)
        {
            // Draw at the head's own size so the picture is not scaled; a child clipped by its parent keeps its full client size.
            Win32.SetWindowPos(_hwnd, IntPtr.Zero, 0, 0, _headW, _headH, 0x0010 | 0x0002); // NOACTIVATE | NOMOVE
            Frame.origami_view_resize(_view, (uint)_headW, (uint)_headH); Frame.origami_view_set_scale(_view, 1.0);
            for (int i = 0; i < 3; i++) if (Frame.origami_view_present(_view, out var dr) != Frame.Ok || dr != Frame.DrawnAgain) break;
        }
        Frame.origami_view_readback(_view, null, 0, out var uw, out var uh, out var stride);
        var buf = new byte[stride * uh];
        int st;
        fixed (byte* p = buf) st = Frame.origami_view_readback(_view, p, (nuint)buf.Length, out uw, out uh, out stride);
        if (st != Frame.Ok) { LastError = "view_readback: " + Frame.LastError(); return null; }
        w = (int)uw; h = (int)uh;
        if (headSize) { _forcePlace = true; Place(); }
        return buf; // tight: stride is w * 4
    }

    public Frame.ViewCounters? CoreViewCounters()
    {
        if (_view == IntPtr.Zero) return null;
        var c = new Frame.ViewCounters { Size = (uint)System.Runtime.InteropServices.Marshal.SizeOf<Frame.ViewCounters>() };
        return Frame.origami_view_stats(_view, ref c) == Frame.Ok ? c : null;
    }

#if ORIGAMI_FAKE
    /// <summary>Self-test: what the last present drew. The copy fallback has no renderer to ask: null.</summary>
    public Frame.LastDraw? LastDrawn()
    {
        if (_view != IntPtr.Zero)
        {
            var d = new Frame.LastDraw { Size = (uint)System.Runtime.InteropServices.Marshal.SizeOf<Frame.LastDraw>() };
            return Frame.origami_view_get_last_draw(_view, ref d) == Frame.Ok ? d : null;
        }
        return null; // the copy fallback has no renderer to ask, so it has no measurement to give
    }

    /// <summary>Self-test: the pane's top-left on screen, physical pixels (the pane is the surround less its padding).</summary>
    public (int X, int Y) PaneOrigin()
    {
        var s = DeviceScale;
        var o = Surround.TransformToVisual(null).TransformPoint(new Windows.Foundation.Point(0, 0));
        var p = new Win32.POINT();
        Win32.ClientToScreen(WindowHandle(), ref p);
        return ((int)Math.Round(p.X + (o.X + ViewScale.PanePadding / 2.0) * s), (int)Math.Round(p.Y + (o.Y + ViewScale.PanePadding / 2.0) * s));
    }
#endif

    /// <summary>The pane the picture lays out in, in DIPs.</summary>
    public (double W, double H) PaneSize => (Surround.ActualWidth, Surround.ActualHeight);
    public double DeviceScale => XamlRoot?.RasterizationScale ?? 1.0;
    public int HeadWidth => _headW;
    public int HeadHeight => _headH;
    public void FocusDisplay() { if (_hwnd != IntPtr.Zero) Win32.SetFocus(_hwnd); }
    public bool HasFocus => _hwnd != IntPtr.Zero && Win32.GetFocus() == _hwnd;

    // ---- the child window's procedure ------------------------------------------------------

    static IntPtr Proc(IntPtr hwnd, uint msg, IntPtr w, IntPtr l)
    {
        if (msg == WM_ERASEBKGND) return new IntPtr(1);
        if (Views.TryGetValue(hwnd, out var v))
        {
            try { if (v.Message(hwnd, msg, w, l, out var r)) return r; }
            catch (Exception e) { v.LastError = "input: " + e.Message; }
        }
        return Win32.DefWindowProcW(hwnd, msg, w, l);
    }

    bool Message(IntPtr hwnd, uint msg, IntPtr w, IntPtr l, out IntPtr result)
    {
        result = IntPtr.Zero;
        if (_core is null) return false;
        switch (msg)
        {
            case WM_SYSKEYDOWN when w.ToInt64() == 0x73: return false; // Alt+F4 still closes the window
            case WM_KEYDOWN: case WM_SYSKEYDOWN: Key(w, l, true); return true;
            case WM_KEYUP: case WM_SYSKEYUP: Key(w, l, false); return true;
            case WM_SETFOCUS: return false;
            case WM_CAPTURECHANGED when l != hwnd: LiftButtons(); return false;
            case WM_KILLFOCUS: LoseFocus(); return false;
            case WM_MOUSEMOVE: Move((short)(l.ToInt64() & 0xFFFF), (short)((l.ToInt64() >> 16) & 0xFFFF)); return true;
            case WM_LBUTTONDOWN: Button(hwnd, 0, true); return true;
            case WM_LBUTTONUP: Button(hwnd, 0, false); return true;
            case WM_MBUTTONDOWN: Button(hwnd, 1, true); return true;
            case WM_MBUTTONUP: Button(hwnd, 1, false); return true;
            case WM_RBUTTONDOWN: Button(hwnd, 2, true); return true;
            case WM_RBUTTONUP: Button(hwnd, 2, false); return true;
            case WM_MOUSEWHEEL:
                {
                    _wheel += (short)((w.ToInt64() >> 16) & 0xFFFF);
                    var notches = _wheel / 120;
                    _wheel -= notches * 120; // high-resolution wheels report parts of a notch
                    if (notches != 0) _core.SendInput(_key, InputEvent.Wheel(notches));
                    return true;
                }
        }
        return false;
    }

    void Key(IntPtr w, IntPtr l, bool down)
    {
        long lp = l.ToInt64();
        int scan = (int)((lp >> 16) & 0xFF);
        bool ext = ((lp >> 24) & 1) != 0;
        int code = KeyMap.FromScanCode(scan, ext);
        if (code == 0) return;
        if (!down && _consumed.Remove(code)) return;
        bool repeat = ((lp >> 30) & 1) != 0;
        if (down && repeat && (_consumed.Contains(code) || !_keys.IsDown(code))) return;
        if (down && !_ptr.Captured && Shortcut is { } sc)
        {
            bool ctrl = Win32.GetKeyState(0x11) < 0, shift = Win32.GetKeyState(0x10) < 0, alt = Win32.GetKeyState(0x12) < 0;
            var vk = (int)w.ToInt64();
            // Every shortcut the app takes needs Ctrl or is a function key; the app decides which ones the guest may not have.
            if ((ctrl || (vk >= 0x70 && vk <= 0x87)) && sc(vk, ctrl, shift, alt)) { _consumed.Add(code); return; }
        }
        if (down) { if (!_keys.Press(code)) return; }
        else if (!_keys.Release(code)) return;
        _core!.SendInput(_key, InputEvent.Key(code, down));
        var up = _ptr.OnKeyState(out var released);
        if (released)
        {
            if (up != null) foreach (var u in up) _core.SendInput(_key, u);
            _core.ReleaseAllKeys(_key);
            EndCapture();
        }
    }

    void Move(int x, int y)
    {
        Win32.GetClientRect(_hwnd, out var rc);
        InputEvent? ev;
        if (_ptr.Captured)
        {
            ev = _ptr.OnMove(x, y, 0, 0, 0, 0, rc.Right / 2, rc.Bottom / 2); // the warp target is this integer centre
            if (ev != null) Win32.SetCursorPos(_centerScreenX, _centerScreenY);
        }
        else ev = _ptr.OnMove(x, y, rc.Right, rc.Bottom, _headW, _headH, 0, 0);
        if (ev != null) _core!.SendInput(_key, ev.Value);
    }

    void Button(IntPtr hwnd, int b, bool down)
    {
        if (down) Win32.SetFocus(hwnd);
        // Keep the mouse while a button is down so its release cannot be missed outside the window.
        if (!down) { _buttonsDown &= ~(1 << b); if (_buttonsDown == 0) Win32.ReleaseCapture(); }
        if (!down && b == _swallowButton) { _swallowButton = -1; return; } // the release of the click that captured
        var ev = _ptr.OnButton(b, down, out var grab);
        if (grab) { _swallowButton = b; BeginCapture(); }
        if (ev != null) _core!.SendInput(_key, ev.Value);
        if (down) { if (_buttonsDown == 0) Win32.SetCapture(hwnd); _buttonsDown |= 1 << b; }
    }

    void LiftButtons()
    {
        for (int b = 0; b < 3; b++)
            if ((_buttonsDown & (1 << b)) != 0) _core?.SendInput(_key, InputEvent.Button(b, false));
        _buttonsDown = 0;
    }

    public void LoseFocus()
    {
        if (_core is null || _frames is null) return; // detached: the view already released everything
        _consumed.Clear(); _swallowButton = -1;
        LiftButtons();
        foreach (var u in _ptr.OnFocusLost()) _core.SendInput(_key, u);
        _core.ReleaseAllKeys(_key);
        EndCapture();
    }

    // ---- capture ---------------------------------------------------------------------------

    void BeginCapture()
    {
        Win32.GetClientRect(_hwnd, out var rc);
        var c = new Win32.POINT { X = rc.Right / 2, Y = rc.Bottom / 2 };
        var tl = new Win32.POINT(); var br = new Win32.POINT { X = rc.Right, Y = rc.Bottom };
        Win32.ClientToScreen(_hwnd, ref c); Win32.ClientToScreen(_hwnd, ref tl); Win32.ClientToScreen(_hwnd, ref br);
        _centerScreenX = c.X; _centerScreenY = c.Y;
        var r = new Win32.RECT { Left = tl.X, Top = tl.Y, Right = br.X, Bottom = br.Y };
        Win32.ClipCursor(ref r);
        Win32.ShowCursor(false); // counted: EndCapture undoes exactly this one
        _grabbed = true;
        Win32.SetCursorPos(_centerScreenX, _centerScreenY);
        PointerModeChanged?.Invoke();
    }

    void EndCapture()
    {
        if (_ptr.Captured) return;
        if (_grabbed) { Win32.ClipCursor(IntPtr.Zero); Win32.ShowCursor(true); _grabbed = false; }
        PointerModeChanged?.Invoke();
    }
}
