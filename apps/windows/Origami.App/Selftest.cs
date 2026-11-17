#if ORIGAMI_FAKE
using System.Text;
using System.Text.Json;
using Origami.Logic;

namespace Origami.App;

/// <summary>
/// `Origami.exe --selftest OUT.json --seed-fake [--expect-renderer NAME]`: drives the app's commands and real
/// SendInput against the core's fake engine and exits with the number of failed steps.
/// </summary>
public static class Selftest
{
    sealed record Step(string Name, bool Ok, string Detail);
    static readonly List<Step> Steps = new();
    static string _out = "";
    static bool _done;

    public static void Fail(string why)
    {
        if (_out == "" || _done) return;
        Add(new Step("fatal", false, why));
        Finish();
    }

    // Each step is also appended to OUT.partial at once, so a crash still leaves the steps that ran.
    static void Add(Step s)
    {
        Steps.Add(s);
        try { File.AppendAllText(_out + ".partial", (s.Ok ? "ok   " : "FAIL ") + s.Name + " | " + s.Detail + Environment.NewLine); } catch { }
    }
    static void Check(string name, bool ok, string detail = "") => Add(new Step(name, ok, detail));
    static void Note(string name, string detail) => Add(new Step(name, true, detail));

    static void Finish()
    {
        _done = true;
        var failed = Steps.Count(s => !s.Ok);
        var json = JsonSerializer.Serialize(new { failed, steps = Steps }, new JsonSerializerOptions { WriteIndented = true });
        File.WriteAllText(_out, json);
        Environment.Exit(failed);
    }

    static async Task<bool> Until(Func<bool> cond, int ms)
    {
        var t = Environment.TickCount64 + ms;
        while (Environment.TickCount64 < t) { if (cond()) return true; await Task.Delay(20); }
        return cond();
    }

    public static async Task RunAsync(MainWindow w, UniffiCoreClient core, string outFile)
    {
        _out = outFile;
        var dir = Path.GetDirectoryName(Path.GetFullPath(outFile))!;
        // Hard stop so a hung step cannot hold the runner.
        _ = Task.Run(async () => { await Task.Delay(120_000); w.DispatcherQueue.TryEnqueue(() => Fail("watchdog: 120 s")); });
        try { await Scenario(w, core, dir); }
        catch (Exception e) { Check("exception", false, e.ToString()); }
        Finish();
    }

    static CancellationTokenSource StartGuest(UniffiCoreClient core, string key, out Task task)
    {
        var cts = new CancellationTokenSource();
        task = Task.Run(async () => { while (!cts.IsCancellationRequested) { try { core.FakeAdvanceFrame(key); } catch { } await Task.Delay(33); } });
        return cts;
    }

    static async Task Scenario(MainWindow w, UniffiCoreClient core, string dir)
    {
        var log = new List<InputEvent>();
        core.InputLog = log;
        await Task.Delay(500);
        // A CI step runs in a console window that owns the foreground; input needs ours.
        if (w.AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter op) op.IsAlwaysOnTop = true;
        w.Activate();
        Win.Foreground(w.Hwnd);
        await Task.Delay(500);
        Check("window is the foreground window", Win.IsForeground(w.Hwnd), $"fg=0x{Win.ForegroundHandle().ToInt64():x} ours=0x{w.Hwnd.ToInt64():x}");
        Check("library lists two machines", w.LibraryCount == 2, $"count={w.LibraryCount} engines={core.EngineIds}");
        var coreCommands = core.Commands();
        var missing = coreCommands.Where(c => !w.MenuShortcuts.ContainsKey(c.Id) || w.MenuShortcuts[c.Id] != c.WindowsAccelerator).Select(c => c.Id).ToList();
        Check("menus carry every core command with its Windows shortcut", coreCommands.Count > 10 && missing.Count == 0,
            $"{coreCommands.Count} core commands, {w.Commands.Count} in the menus, mismatched: {string.Join(",", missing)}");
        var key = w.SelectedKey!;
        Check("a machine is selected", !string.IsNullOrEmpty(key), key);

        // --- new machine: the catalogue, a create, and the core's refusal of a bad name ---
        var offers = core.Catalogue();
        Check("new machine: the catalogue offers machines", offers.Count > 0 && NewMachine.DefaultIndex(offers) < offers.Count, string.Join(",", offers.Select(NewMachine.Label)));
        var made = await core.CreateMachineAsync("Selftest created", offers[NewMachine.DefaultIndex(offers)].Id);
        w.Reload();
        Check("new machine: create returns a key and the library lists the machine", made.Length > 0 && w.LibraryCount == 3 && core.Machines().Any(m => m.Key == made && m.Name == "Selftest created"), $"key={made} count={w.LibraryCount}");
        string refused = "";
        try { await core.CreateMachineAsync("a/b", offers[0].Id); } catch (Exception e) { refused = e.Message; }
        Check("new machine: a name with a slash is refused with a reason", refused.Length > 0 && w.LibraryCount == 3, refused);
        key = w.SelectedKey!;

        // --- start ---
        w.Invoke("machine.start");
        Check("start reaches Running", await Until(() => core.Status(key).State == MachineState.Running, 15_000), core.Status(key).Reason);
        Check("pill says Running", await Until(() => w.PillTextForTest == "Running", 3000), w.PillTextForTest);

        // --- display ---
        var disp = w.DisplayViewForTest;
        var attached = await Until(() => disp.Attached, 8000);
        Check("display attached", attached, disp.LastError ?? "");
        if (!attached) return;
        Note("renderer", $"{disp.Renderer} (core view status {disp.CoreViewStatus}; {disp.LastError})");
        if (App.ArgValue("--expect-renderer") is { } want)
            Check("renderer is the one this runner should use", disp.Renderer == want, $"got '{disp.Renderer}', want '{want}'");

        // a fake guest that draws 30 frames a second
        var guestStop = StartGuest(core, key, out var guest);
        var moving = await Until(() => disp.Presents >= 10, 8000);
        Check("frames present while the guest draws", moving, $"wakes={disp.Wakes} presents={disp.Presents}");
        guestStop.Cancel(); await guest;
        await Until(() => false, 400); // let the last wake drain
        var seq = core.Display(key, 0).Seq;
        await Until(() => disp.FallbackCounters() is not { } c || c.LastSeq == seq, 3000);

        // first picture
        var ppmPath = Path.Combine(dir, "engine-screendump.ppm");
        core.Screendump(key, 0, ppmPath);
        var (pw, ph, engine) = Shot.ReadPpm(ppmPath);
        Check("the engine's screendump has the head's size", pw == disp.HeadWidth && ph == disp.HeadHeight, $"{pw}x{ph} vs {disp.HeadWidth}x{disp.HeadHeight}");
        var pixels = disp.Verify(out var cv);
        if (pixels == null) Note("first-picture readback", "not available: the core's renderer owns the surface (no readback hook)");
        else
        {
            long vsEngine = 0, vsExpect = 0;
            for (int y = 0; y < ph; y++)
                for (int x = 0; x < pw; x++)
                {
                    int o = (y * pw + x) * 4;
                    if (pixels[o] != engine[o] || pixels[o + 1] != engine[o + 1] || pixels[o + 2] != engine[o + 2]) vsEngine++;
                    // the fake engine draws [seq, y, x] as B, G, R
                    if (pixels[o] != (byte)seq || pixels[o + 1] != (byte)y || pixels[o + 2] != (byte)x) vsExpect++;
                }
            Check("first-picture: GPU readback equals the engine's screendump (0 mismatching pixels)", vsEngine == 0, $"mismatch={vsEngine} of {pw * ph} ({pw}x{ph}, seq {seq})");
            Check("first-picture: readback equals the pattern computed independently", vsExpect == 0, $"mismatch={vsExpect}");
            Check("presenter's own check: back buffer equals its upload buffer", cv.LastMismatch == 0, $"mismatch={cv.LastMismatch}");
            var avgUs = cv.Presents == 0 ? 0 : cv.PresentUsTotal / cv.Presents;
            Note("copy path accounting", $"CPU copied {cv.CpuBytesCopiedPicture} picture bytes; present avg={avgUs} us max={cv.PresentUsMax} us over {cv.Presents}, warp={cv.IsWarp} (not enforced on WARP)");
            await Shot.Readback(pixels, pw, ph, pw * 4, Path.Combine(dir, "picture.png"));
        }
        try { await Shot.Xaml((Microsoft.UI.Xaml.UIElement)w.Content, Path.Combine(dir, "xaml.png")); Note("xaml render saved", ""); }
        catch (Exception e) { Check("xaml render saved", false, e.Message); }

        // --- idle: nothing wakes, nothing presents ---
        var (w0, p0) = (disp.Wakes, disp.Presents);
        var n0 = disp.FallbackCounters()?.Presents;
        await Task.Delay(1500);
        Check("idle: no wakes and no presents while the picture is unchanged",
            disp.Wakes == w0 && disp.Presents == p0 && disp.FallbackCounters()?.Presents == n0,
            $"wakes {w0} -> {disp.Wakes}, presents {p0} -> {disp.Presents}, native {n0} -> {disp.FallbackCounters()?.Presents}");

        File.WriteAllText(Path.Combine(dir, "ready.txt"), "ready");
        if (!await Until(() => File.Exists(Path.Combine(dir, "done.txt")), 15_000)) Note("desktop screenshot", "the workflow did not signal done");

        core.FakeAdvanceFrame(key);
        Check("a new frame wakes the viewer again", await Until(() => disp.Wakes > w0, 3000), $"wakes={disp.Wakes}");

        // --- console ---
        core.ConsoleSend(key, Encoding.ASCII.GetBytes("hinv\r"));
        var echoed = await Until(() => Encoding.ASCII.GetString(core.ConsoleRead(key, 0, out _)).Contains("hinv"), 3000);
        Check("console: bytes sent come back", echoed);

        // --- keyboard, real scancodes through the OS ---
        Win.Foreground(w.Hwnd);
        disp.FocusDisplay();
        await Task.Delay(300);
        var (cx, cy, cw, ch) = disp.ScreenRect();
        Win.MouseAbs(cx + cw / 2, cy + ch / 2);
        await Task.Delay(150);
        Check("the screen window has keyboard focus", disp.HasFocus);
        lock (log) log.Clear();
        foreach (var sc in new[] { 0x23, 0x17, 0x31, 0x2F, 0x1C }) { Win.Scan(sc, false, true); Win.Scan(sc, false, false); await Task.Delay(30); }
        await Until(() => { lock (log) return log.Count(e => e.Kind == InputKind.Key) >= 10; }, 3000);
        int[] keys; lock (log) keys = log.Where(e => e.Kind == InputKind.Key && e.Down).Select(e => e.A).ToArray();
        Check("keyboard: scancodes for hinv and Enter become the right key positions in the app", keys.SequenceEqual(new[] { 35, 23, 49, 47, 28 }), string.Join(",", keys));
        var atEngine = core.FakeInput(key).Where(e => e.StartsWith("Key")).ToArray();
        var wantKeys = new[] { "H", "I", "N", "V", "Enter" };
        Check("keyboard: the engine itself received hinv and Enter, down then up, in order",
            atEngine.Length == 10 && wantKeys.Select((n, i) => atEngine[2 * i].Contains(n + ",") && atEngine[2 * i].Contains("true") && atEngine[2 * i + 1].Contains("false")).All(b => b),
            string.Join(" ", atEngine.Take(4)));
        foreach (var sc in new[] { 0x23, 0x17, 0x31, 0x2F }) Win.Scan(sc, false, true); // h i n v held
        await Task.Delay(200);
        var held = core.ReleaseAllKeys(key);
        Check("keyboard: the core itself counts the four held keys", held == 4, $"core released {held}");
        foreach (var sc in new[] { 0x23, 0x17, 0x31, 0x2F }) Win.Scan(sc, false, false);
        await Task.Delay(100);

        // an app shortcut over the screen must not reach the guest
        bool shown = w.LibraryShown;
        lock (log) log.Clear();
        Win.Scan(0x43, false, true); Win.Scan(0x43, false, false); // F9: view.library
        await Task.Delay(300);
        bool leaked; lock (log) leaked = log.Any(e => e.Kind == InputKind.Key);
        Check("shortcut: F9 toggles the library and does not reach the guest", w.LibraryShown != shown && !leaked, $"library {shown} -> {w.LibraryShown}, leaked={leaked}");
        w.Invoke("view.library");
        await Task.Delay(300);

        // --- follow-the-mouse ---
        Check("pointer mode starts as Follow", disp.Mode == PointerMode.Follow);
        (cx, cy, cw, ch) = disp.ScreenRect();
        lock (log) log.Clear();
        Win.MouseAbs(cx + cw / 4, cy + ch / 2);
        var wantX = disp.HeadWidth / 4; var wantY = disp.HeadHeight / 2;
        bool near = await Until(() => { lock (log) return log.Any(e => e.Kind == InputKind.PointerAt && Math.Abs(e.A - wantX) <= 2 && Math.Abs(e.B - wantY) <= 2); }, 2000);
        string last; lock (log) last = log.LastOrDefault(e => e.Kind == InputKind.PointerAt).ToString();
        Check("follow-mouse: the host pointer maps to the right head pixel (+-2)", near, $"want ({wantX},{wantY}) last {last}");

        // --- capture ---
        w.Invoke("machine.pointer-mode");
        await Task.Delay(200);
        Check("pointer mode toggled to Capture", disp.Mode == PointerMode.Capture);
        var guest2 = StartGuest(core, key, out var guest2Task); // events keep arriving while captured
        (cx, cy, cw, ch) = disp.ScreenRect();
        Win.MouseAbs(cx + cw / 2, cy + ch / 2);
        await Task.Delay(100);
        lock (log) log.Clear();
        Win.MouseButton(true); Win.MouseButton(false);
        Check("capture: a click captures", await Until(() => disp.Captured, 2000));
        Check("capture: the host cursor is hidden and confined to the screen window", Win.CursorHidden() && Win.ClipIsSmall(), $"hidden={Win.CursorHidden()} clipped={Win.ClipIsSmall()}");
        await Task.Delay(700);
        Check("capture: it survives the guest drawing frames", disp.Captured);
        bool clickSent; lock (log) clickSent = log.Any(e => e.Kind == InputKind.Button);
        Check("capture: the capturing click is not sent to the guest", !clickSent);
        await Task.Delay(200);
        Win.MouseRel(20, 10);
        bool moved = await Until(() => { lock (log) return log.Any(e => e.Kind == InputKind.PointerMove && e.A > 0 && e.B > 0); }, 2000);
        Check("capture: relative motion reaches the core", moved);
        Win.Scan(0x1E, false, true); // a key down, then the chord
        await Task.Delay(80);
        Win.Scan(0x1D, false, true); Win.Scan(0x38, false, true);
        Check("capture: Ctrl+Alt releases the pointer", await Until(() => !disp.Captured, 2000));
        Win.Scan(0x38, false, false); Win.Scan(0x1D, false, false); Win.Scan(0x1E, false, false);
        await Task.Delay(150);
        guest2.Cancel(); await guest2Task;
        Check("capture: the child window no longer owns the mouse", Win.CaptureOwner() == IntPtr.Zero, $"owner=0x{Win.CaptureOwner().ToInt64():x}");
        Check("capture: the host cursor is shown and free again", !Win.CursorHidden() && !Win.ClipIsSmall(), $"hidden={Win.CursorHidden()} clipped={Win.ClipIsSmall()}");
        var stuck = core.ReleaseAllKeys(key);
        Check("capture: the core holds no key after the chord", stuck == 0, $"released={stuck}");

        Check("quit: a running machine is named from live state", w.UpNames.Count == 1, string.Join(",", w.UpNames));

        // --- diagnostics ---
        w.ClearSelectionForTest();
        Check("diagnostics: nothing is selected", await Until(() => w.SelectedKey == null, 2000), w.SelectedKey ?? "");
        w.Invoke("window.diagnostics");
        Check("diagnostics: with nothing selected the command says to select a machine", w.StatusForTest == "Select a machine first.", w.StatusForTest);
        w.SelectForTest(key);
        await Until(() => w.SelectedKey == key, 2000);
        w.Invoke("window.diagnostics");
        var diag = await Until(() => w.DiagnosticsForTest(key) != null, 3000) ? w.DiagnosticsForTest(key) : null;
        Check("diagnostics: the window opens", diag != null);
        if (diag != null)
        {
            Check("diagnostics: the report fills in", await Until(() => diag.ReportText.Contains("wake-ups"), 3000));
            var rt = diag.ReportText;
            Check("diagnostics: the report shows the engine, state, heads and screen counts", rt.Contains("fake") && rt.Contains("Running") && rt.Contains("64x48") && rt.Contains("wake-ups"), rt.Replace("\r", "").Replace("\n", " | "));
            diag.Copy();
            string clip = "";
            try { clip = await Windows.ApplicationModel.DataTransfer.Clipboard.GetContent().GetTextAsync(); } catch (Exception e) { clip = "clipboard: " + e.Message; }
            Check("diagnostics: Copy Report puts the report on the clipboard", clip == diag.ReportText, clip.Length > 80 ? clip[..80] : clip);
            diag.Close();
        }

        // --- pause and resume ---
        w.Invoke("machine.pause");
        Check("pause: the machine is Paused", await Until(() => core.Status(key).State == MachineState.Paused, 3000), core.Status(key).Reason);
        Check("pause: the pill says Paused", await Until(() => w.PillTextForTest == "Paused", 2000), w.PillTextForTest);
        w.FocusLibrary();
        await Task.Delay(300);
        Win.Scan(0x1D, false, true); Win.Scan(0x19, false, true); Win.Scan(0x19, false, false); Win.Scan(0x1D, false, false); // Ctrl+P
        Check("resume: Ctrl+P from XAML focus resumes a paused machine", await Until(() => core.Status(key).State == MachineState.Running, 3000), core.Status(key).State.ToString());

        // --- focus loss lifts a held key ---
        disp.FocusDisplay();
        await Task.Delay(300);
        Win.Scan(0x1E, false, true); // 'a' held
        await Task.Delay(200);
        w.FocusLibrary();
        await Task.Delay(300);
        Win.Scan(0x1E, false, false);
        Check("focus loss: the held key is lifted in the app and in the core", disp.KeysDown == 0 && core.ReleaseAllKeys(key) == 0, $"app holds {disp.KeysDown}");

        // --- stopping while captured and holding a key must leave nothing behind ---
        disp.FocusDisplay();
        (cx, cy, cw, ch) = disp.ScreenRect();
        Win.MouseAbs(cx + cw / 2, cy + ch / 2);
        await Task.Delay(100);
        Win.MouseButton(true); Win.MouseButton(false);
        Check("detach while captured: captured first", await Until(() => disp.Captured, 2000));
        Win.Scan(0x1E, false, true);
        await Task.Delay(150);
        w.Invoke("machine.stop");
        Check("detach while captured: the display detaches", await Until(() => !disp.Attached, 8000));
        Win.Scan(0x1E, false, false);
        Check("detach while captured: cursor shown, unclipped, mouse released, no key held",
            !Win.CursorHidden() && !Win.ClipIsSmall() && Win.CaptureOwner() == IntPtr.Zero && disp.KeysDown == 0,
            $"hidden={Win.CursorHidden()} clipped={Win.ClipIsSmall()} owner=0x{Win.CaptureOwner().ToInt64():x} keys={disp.KeysDown}");

        // --- stop ---
        Check("power off reaches Stopped", await Until(() => core.Status(key).State == MachineState.Stopped, 8000), core.Status(key).Reason);
        Check("pill says Stopped", await Until(() => w.PillTextForTest == "Stopped", 3000), w.PillTextForTest);
        Check("display detached after stop", await Until(() => !disp.Attached, 3000));

        // --- stopping a machine that has already ended is not an error ---
        string again = "ok";
        try { await core.PowerOffAsync(key); await core.ForceQuitAsync(key); } catch (Exception e) { again = e.GetType().Name + ": " + e.Message; }
        Check("stop: Power Off and Force Quit on an ended machine do not fail", again == "ok", again);
        Check("quit: no machine is up, so quitting would not prompt", w.UpNames.Count == 0, string.Join(",", w.UpNames));
    }
}
#endif
