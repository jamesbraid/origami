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

        // --- two machines of one product: a real click on each row selects that row ---
        var keys2 = core.Machines().Select(x => x.Key).ToList();
        var okRows = keys2.Count == 2;
        foreach (var target in keys2.Concat(keys2.Take(1)).ToList()) // a, b, a: never the same row twice running, which is a double click
        {
            if (w.RowCenter(target) is not { } rc) { okRows = false; break; }
            Win.MouseAbs(rc.X, rc.Y); await Task.Delay(120); Win.MouseButton(true); Win.MouseButton(false);
            okRows &= await Until(() => w.SelectedKey == target, 2000);
        }
        Check("library: clicking each of two same-product rows selects that row, in both orders", okRows, $"selected {w.SelectedKey}");

        // --- new machine: the catalogue, a create, and the core's refusal of a bad name ---
        var offers = core.Catalogue();
        Check("new machine: the catalogue offers machines", offers.Count > 0 && NewMachine.DefaultIndex(offers) < offers.Count, string.Join(",", offers.Select(NewMachine.Label)));
        var made = await core.CreateMachineAsync("Selftest created", offers[NewMachine.DefaultIndex(offers)].Id);
        w.Reload();
        Check("new machine: create returns a key and the library lists the machine", made.Length > 0 && w.LibraryCount == 3 && core.Machines().Any(m => m.Key == made && m.Name == "Selftest created"), $"key={made} count={w.LibraryCount}");
        string refused = "";
        try { await core.CreateMachineAsync("a/b", offers[0].Id); } catch (Exception e) { refused = e.Message; }
        Check("new machine: a name with a slash is refused with a reason", refused.Length > 0 && w.LibraryCount == 3, refused);

        // --- engine choice: a named engine is used, an unknown one is refused ---
        var eng = offers[0].Engine;
        var viaEngine = await core.CreateMachineAsync("Selftest engine", offers[0].Id, eng);
        string noEngine = "";
        try { await core.CreateMachineAsync("Selftest nowhere", offers[0].Id, "no-such-engine"); } catch (Exception e) { noEngine = e.Message; }
        Check("engine: a create names its engine; an unknown engine is refused", viaEngine.Length > 0 && noEngine.Length > 0 && NewMachine.EnginesFor(offers, offers[0].Id).Contains(eng), $"engine={eng} refused='{noEngine}'");

        // --- settings: read, edit, save, and the core's refusal while running ---
        var st0 = await core.SettingsAsync(made);
        var otherMem = st0.MemoryAllowed.FirstOrDefault(m => m != st0.MemoryMiB);
        await core.ApplyChangeAsync(made, new MachineChange.SetMemory(otherMem));
        var st1 = await core.SettingsAsync(made);
        Check("settings: a memory edit is written to the machine file", otherMem != 0 && st1.MemoryMiB == otherMem && st0.MemoryMiB != otherMem, $"{st0.MemoryMiB} -> {st1.MemoryMiB} of {string.Join(",", st0.MemoryAllowed)}");
        await core.ApplyChangeAsync(made, new MachineChange.AddForward(new Forward("ssh", "tcp", 2222, 22)));
        var st2 = await core.SettingsAsync(made);
        Check("settings: a port forward is added", st2.Forwards.Any(f => f.Name == "ssh" && f.HostPort == 2222 && f.GuestPort == 22), string.Join(",", st2.Forwards));
        await core.ApplyChangeAsync(made, new MachineChange.RemoveForward("ssh"));
        await core.ApplyChangeAsync(made, new MachineChange.AddShare(new Share("shared", dir)));
        var st3 = await core.SettingsAsync(made);
        Check("settings: a forward is removed and a folder shared", st3.Forwards.Count == 0 && st3.Shares.Any(x => x.Name == "shared"), string.Join(",", st3.Shares));
        await core.ApplyChangeAsync(made, new MachineChange.RemoveShare("shared"));
        await core.ApplyChangeAsync(made, new MachineChange.SetNetwork("none"));
        var st4 = await core.SettingsAsync(made);
        Check("settings: a share is removed and the network mode set", st4.Shares.Count == 0 && st4.NetworkMode == "none", st4.NetworkMode);

        // --- tabs ---
        var rows = core.Machines().Select(x => x.Key).ToList();
        Check("tabs: the bar is shown even with one machine open", w.TabCount == 1 && w.TabBarVisible, $"tabs {w.TabCount}, bar {w.TabBarVisible}");
        var first = w.SelectedKey!;
        var other = rows.First(k => k != first);
        w.OpenInNewTab(other);
        Check("tabs: Open in New Tab adds a tab beside the first machine's", await Until(() => w.TabCount == 2 && w.ActiveTabKey == other, 2500), $"tabs {w.TabCount}, active {w.ActiveTabKey}");
        w.SelectForTabs(first); await Until(() => w.ActiveTabKey == first, 2000);
        w.OpenInNewTab(first);
        Check("tabs: opening an open machine does not add another tab", w.TabCount == 2, $"tabs {w.TabCount}");
        await Until(() => w.TabCenter(other) != null, 3000); // the tabs were rebuilt by the select above: wait for their layout
        if (w.TabCenter(other) is { } tc)
        {
            Win.MouseAbs(tc.X, tc.Y); await Task.Delay(120); Win.MouseButton(true); Win.MouseButton(false);
            Check("tabs: clicking a tab selects its machine", await Until(() => w.SelectedKey == other && w.ActiveTabKey == other, 2500), $"selected {w.SelectedKey}");
        }
        else Check("tabs: a tab can be clicked", false, "no tab found");
        await w.CloseTabAsync(other, ask: false);
        Check("tabs: closing a tab leaves the other one active and the bar shown", w.TabCount == 1 && w.ActiveTabKey == first && w.TabBarVisible, $"tabs {w.TabCount}, active {w.ActiveTabKey}");
        // --- the row: a real double click opens a tab, and its context menu ---
        var rows2 = core.Machines().Select(x => x.Key).ToList();
        var first2 = w.SelectedKey!;
        var other2 = rows2.First(k => k != first2);
        if (w.RowScreenPoint(other2) is { } rcx)
        {
            Win.MouseAbs(rcx.X, rcx.Y); await Task.Delay(120);
            Win.MouseButton(true); Win.MouseButton(false); await Task.Delay(90); Win.MouseButton(true); Win.MouseButton(false); // a real double click
            Check("rows: a double click opens a tab and keeps the first machine's", await Until(() => w.TabCount == 2 && w.ActiveTabKey == other2, 2500), $"tabs {w.TabCount}, active {w.ActiveTabKey}");
        }
        else Check("rows: a library row can be clicked", false, "no row found");
        Check("rows: the context menu offers Open in New Tab and Show in Explorer", w.RowMenuTexts(first2).SequenceEqual(new[] { "Open in New Tab", "Show in Explorer" }), string.Join(",", w.RowMenuTexts(first2)));
        w.SelectForTabs(first2); await Until(() => w.SelectedKey == first2, 2000);
        await w.CloseTabAsync(other2, ask: false);
        Check("cards: Memory, Nodes and Topology show on the stopped machine's card", w.CardsText.Contains("Memory") && w.CardsText.Contains("Node") && w.CardsText.Contains("Topology"), w.CardsText);
        key = w.SelectedKey!;

        // --- start ---
        w.Invoke("machine.start");
        Check("start reaches Running", await Until(() => core.Status(key).State == MachineState.Running, 15_000), core.Status(key).Reason);
        Check("pill says Running", await Until(() => w.PillTextForTest == "Running", 3000), w.PillTextForTest);
        string busy = "";
        try { await core.ApplyChangeAsync(key, new MachineChange.SetMemory(st0.MemoryAllowed[0])); } catch (Exception e) { busy = e.GetType().Name; }
        Check("settings: an edit to a running machine is refused as Busy", busy.Contains("Busy"), busy);

        // --- display ---
        var disp = w.DisplayViewForTest;
        var attached = await Until(() => disp.Attached, 8000);
        Check("display attached", attached, disp.LastError ?? "");
        if (!attached) return;
        Check("granite: the surround is drawn at one device pixel per bitmap pixel", await Until(() => disp.GraniteIsDevicePixels, 5000));
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

        // --- the console as a terminal: colours, typing, selection, copy, paste, scrollback, redraw only what changed ---
        w.Invoke("window.console");
        Check("console: the command opens a window for the machine", await Until(() => w.ConsoleForTest(key) != null, 4000));
        var con = w.ConsoleForTest(key);
        if (con is not null)
        {
            core.FakeSay(key, "\u001b[31mred\u001b[0m plain\r\n");
            Check("terminal: guest output reaches the grid", await Until(() => con.TextForTest.Contains("red plain"), 3000), con.TextForTest.Replace("\n", " | "));
            Check("terminal: coloured text is drawn in colour", await Until(() => con.ColourCells > 0, 2000), $"{con.ColourCells} coloured cells in the last row drawn");
            con.Type("hinv"); con.PressEnter();
            Check("terminal: typed keys reach the guest (the fake engine echoes them)", await Until(() => con.TextForTest.Contains("hinv"), 3000), con.TextForTest.Replace("\n", " | "));
            var lines = con.TextForTest.Split('\n');
            int row = Array.FindIndex(lines, l => l.Contains("red plain"));
            con.SelectAll(row, 0, row, 8);
            Check("terminal: a selection reads back as its text", con.SelectedText == "red plain", $"'{con.SelectedText}'");
            con.Copy();
            string clipText = "";
            try { clipText = await Windows.ApplicationModel.DataTransfer.Clipboard.GetContent().GetTextAsync(); } catch (Exception e) { clipText = "clipboard: " + e.Message; }
            Check("terminal: Copy puts the selection on the clipboard", clipText == "red plain", clipText);
            var pkg = new Windows.ApplicationModel.DataTransfer.DataPackage(); pkg.SetText("pasted line\r\n");
            Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(pkg);
            await con.PasteAsync();
            Check("terminal: Paste sends the clipboard text", await Until(() => con.TextForTest.Contains("pasted line"), 3000), con.TextForTest.Replace("\n", " | "));
            await Task.Delay(400);
            var drawn0 = con.RowsDrawn;
            await Task.Delay(600);
            Check("terminal: an idle console redraws nothing", con.RowsDrawn == drawn0, $"rows drawn {drawn0} -> {con.RowsDrawn}");
            core.FakeSay(key, "one more line\r\n");
            await Until(() => con.TextForTest.Contains("one more line"), 3000);
            await Task.Delay(300);
            Check("terminal: new output redraws changed rows only", con.RowsDrawn - drawn0 > 0 && con.RowsDrawn - drawn0 < con.Rows, $"drew {con.RowsDrawn - drawn0} of {con.Rows} rows");
            for (int n = 0; n < 120; n++) core.FakeSay(key, $"line {n}\r\n");
            await Until(() => con.TextForTest.Contains("line 119"), 5000); // all of it, or new lines keep moving the offset
            Check("terminal: output beyond the screen goes to scrollback", con.ScrollbackCount > 0, $"scrollback {con.ScrollbackCount}");
            con.ScrollBack(5);
            Check("terminal: scrolling back moves the view", con.ScrollOffset == 5, $"offset {con.ScrollOffset}");
            con.Type("x");
            Check("terminal: typing returns to the live end", con.ScrollOffset == 0, $"offset {con.ScrollOffset}");
            core.FakeSay(key, "\r\nxy");
            await Until(() => con.TextForTest.TrimEnd().EndsWith("xy"), 3000);
            await Task.Delay(300);
            int col0 = con.CursorCol; con.PressBackspace();
            Check("terminal: Backspace goes through the core as ^H and moves the echoed cursor left", await Until(() => con.CursorCol == col0 - 1, 3000), $"column {col0} -> {con.CursorCol}");
        }

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

        // --- About, and the starting screen's live cube ---
        IdentityChecks.OutDir = dir;
        foreach (var (name, ok, detail) in await IdentityChecks.About(w, Until)) Check(name, ok, detail);
        w.PreviewStarting();
        var cubeStart = w.StartCubeForTest;
        Check("starting: the cube attached", await Until(() => cubeStart.Attached, 10_000), cubeStart.LastError ?? $"status {cubeStart.Status}");
        await Task.Delay(800);
        var s1 = cubeStart.Readback(out var sw1, out var sh1);
        var turns = s1 != null && await IdentityChecks.Turns(cubeStart, s1);
        Check("starting: the cube draws on granite and turns by itself", s1 != null && turns && IdentityChecks.Colours(s1) > 50, s1 == null ? cubeStart.LastError ?? "no readback" : $"{sw1}x{sh1}, {IdentityChecks.Colours(s1)} colours, moving={cubeStart.Moving}");
        if (s1 != null) await Shot.Readback(s1, sw1, sh1, sw1 * 4, Path.Combine(dir, "starting-cube.png"));
        w.Restore();
        Check("starting: the cube is detached once the pane is gone", await Until(() => !cubeStart.Attached, 3000));

        // --- stop ---
        Check("power off reaches Stopped", await Until(() => core.Status(key).State == MachineState.Stopped, 8000), core.Status(key).Reason);
        Check("pill says Stopped", await Until(() => w.PillTextForTest == "Stopped", 3000), w.PillTextForTest);
        Check("display detached after stop", await Until(() => !disp.Attached, 3000));

        // --- stopping a machine that has already ended is not an error ---
        string again = "ok";
        try { await core.PowerOffAsync(key); await core.ForceQuitAsync(key); } catch (Exception e) { again = e.GetType().Name + ": " + e.Message; }
        Check("stop: Power Off and Force Quit on an ended machine do not fail", again == "ok", again);
        Check("quit: no machine is up, so quitting would not prompt", w.UpNames.Count == 0, string.Join(",", w.UpNames));

        // --- a machine with no display opens to its console ---
        var blind = await core.CreateMachineAsync("Serial only (fake)", "fakeblind.1");
        w.Reload();
        w.SelectForTest(blind);
        await Until(() => w.SelectedKey == blind, 2000);
        w.Invoke("machine.start");
        Check("headless: the machine starts", await Until(() => core.Status(blind).State == MachineState.Running, 15_000), core.Status(blind).Reason);
        Check("headless: the pill says Console only", await Until(() => w.PillTextForTest == "Console only", 3000), w.PillTextForTest);
        Check("headless: the console is the machine's view, with no display attached", await Until(() => w.ConsolePaneForTest != null, 3000) && !w.DisplayViewForTest.Attached && !w.DisplayVisibleForTest);
        if (w.ConsolePaneForTest is { } pane)
        {
            core.FakeSay(blind, "serial only\r\n");
            Check("headless: guest output reaches the pane", await Until(() => pane.TextForTest.Contains("serial only"), 3000), pane.TextForTest.Replace("\n", " | "));
        }
        w.Invoke("machine.stop");
        Check("headless: stopping closes the pane", await Until(() => core.Status(blind).State == MachineState.Stopped && w.ConsolePaneForTest == null, 8000), core.Status(blind).Reason);
    }
}
#endif
