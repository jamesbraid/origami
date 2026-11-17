using Origami.Logic;

// A tiny runner: each test is a named lambda, a failure is an exception, the exit code is the
// number of failures. No package feed needed, so it runs the same on every CI host.
var failures = 0;
void Test(string name, Action body)
{
    try { body(); Console.WriteLine($"PASS {name}"); }
    catch (Exception e) { failures++; Console.WriteLine($"FAIL {name}: {e.Message}"); }
}
void Eq<T>(T want, T got, string what = "")
{
    if (!EqualityComparer<T>.Default.Equals(want, got)) throw new Exception($"{what} want {want} got {got}");
}

Test("keymap: main block, extended and unknown", () =>
{
    Eq(30, KeyMap.FromScanCode(0x1E, false), "A");
    Eq(28, KeyMap.FromScanCode(0x1C, false), "Enter");
    Eq(96, KeyMap.FromScanCode(0x1C, true), "KP enter");
    Eq(103, KeyMap.FromScanCode(0x48, true), "Up");
    Eq(72, KeyMap.FromScanCode(0x48, false), "KP8");
    Eq(100, KeyMap.FromScanCode(0x38, true), "RAlt");
    Eq(0, KeyMap.FromScanCode(0x7F, false), "unknown");
    Eq(0, KeyMap.FromScanCode(0x20, true), "unknown extended");
});

Test("keymap: Pause and NumLock share scan 0x45 and differ by the extended flag", () =>
{
    Eq(Array.IndexOf(GuestKeys.Names, "Pause"), GuestKeys.Ordinal(KeyMap.FromScanCode(0x45, false)), "Pause");
    Eq(Array.IndexOf(GuestKeys.Names, "NumLock"), GuestKeys.Ordinal(KeyMap.FromScanCode(0x45, true)), "NumLock");
});

Test("keymap: hinv spells out", () =>
{
    Eq('h', KeyMap.Ascii(KeyMap.FromScanCode(0x23, false)) ?? '?');
    Eq('i', KeyMap.Ascii(KeyMap.FromScanCode(0x17, false)) ?? '?');
    Eq('n', KeyMap.Ascii(KeyMap.FromScanCode(0x31, false)) ?? '?');
    Eq('v', KeyMap.Ascii(KeyMap.FromScanCode(0x2F, false)) ?? '?');
});

Test("keyboard: repeat is not a new press; release all lifts each key once", () =>
{
    var k = new KeyboardState();
    Eq(true, k.Press(30)); Eq(false, k.Press(30)); k.Press(29);
    var up = k.ReleaseAll();
    Eq(2, up.Count); Eq(0, k.DownCount);
    Eq(true, up.All(e => e.Kind == InputKind.Key && !e.Down));
});

Test("pointer: follow maps panel to head pixels and clamps", () =>
{
    var p = PointerController.ToHead(640, 512, 1280, 1024, 1280, 1024);
    Eq((640, 512), p!.Value);
    var half = PointerController.ToHead(100, 100, 640, 512, 1280, 1024);
    Eq((200, 200), half!.Value);
    Eq(true, PointerController.ToHead(-1, 5, 640, 512, 1280, 1024) is null);
    Eq(true, PointerController.ToHead(640, 5, 640, 512, 1280, 1024) is null);
});

Test("pointer: follow sends absolute, capture mode ignores moves until the click", () =>
{
    var c = new PointerController(new KeyboardState()) { Mode = PointerMode.Follow };
    Eq(InputEvent.At(10, 20), c.OnMove(10, 20, 1280, 1024, 1280, 1024, 0, 0)!.Value);
    c.Mode = PointerMode.Capture;
    Eq(true, c.OnMove(10, 20, 1280, 1024, 1280, 1024, 0, 0) is null);
    Eq(true, c.OnButton(0, true, out var grab) is null); Eq(true, grab); Eq(true, c.Captured);
    Eq(InputEvent.Move(3, -2), c.OnMove(103, 98, 1280, 1024, 1280, 1024, 100, 100)!.Value);
    Eq(true, c.OnMove(100, 100, 1280, 1024, 1280, 1024, 100, 100) is null);
    Eq(InputEvent.Button(0, true), c.OnButton(0, true, out _)!.Value);
});

Test("pointer: ctrl+alt frees the pointer and no key stays down", () =>
{
    var keys = new KeyboardState();
    var c = new PointerController(keys) { Mode = PointerMode.Capture };
    c.OnButton(0, true, out _);
    keys.Press(KeyMap.LeftCtrl); Eq(true, c.OnKeyState(out _) is null);
    keys.Press(KeyMap.LeftAlt);
    var up = c.OnKeyState(out var rel);
    Eq(true, rel); Eq(false, c.Captured); Eq(2, up!.Count); Eq(0, keys.DownCount);
});

Test("pointer: focus loss lifts keys and frees the pointer", () =>
{
    var keys = new KeyboardState();
    var c = new PointerController(keys) { Mode = PointerMode.Capture };
    c.OnButton(0, true, out _); keys.Press(30);
    var up = c.OnFocusLost();
    Eq(1, up.Count); Eq(false, c.Captured);
});

Test("settings: round trip, per machine override, damaged file is a first run", () =>
{
    var dir = Path.Combine(Path.GetTempPath(), "origami-test-" + Guid.NewGuid().ToString("N"));
    var store = new SettingsStore(Path.Combine(dir, "settings.json"));
    Eq(PointerMode.Follow, store.Load().DefaultPointerMode);
    var s = new AppSettings { LibraryRoot = @"C:\machines" };
    s.PointerModeByMachine["o2"] = PointerMode.Capture;
    store.Save(s);
    var back = store.Load();
    Eq(@"C:\machines", back.LibraryRoot); Eq(PointerMode.Capture, back.ModeFor("o2")); Eq(PointerMode.Follow, back.ModeFor("x"));
    File.WriteAllText(Path.Combine(dir, "settings.json"), "{ nope");
    Eq("", store.Load().LibraryRoot);
    Directory.Delete(dir, true);
});

Test("pill: words and tones", () =>
{
    Eq("Running", Pill.Text(MachineState.Running, false));
    Eq("Console only", Pill.Text(MachineState.Running, true));
    Eq("Saving", Pill.Text(MachineState.Stopping, false));
    Eq("red", Pill.Tone(MachineState.Exited));
    Eq("Left running", Pill.Text(MachineState.LeftRunning)); Eq("amber", Pill.Tone(MachineState.LeftRunning));
});

Test("guest keys: ordinals follow the core's key order", () =>
{
    Eq(104, GuestKeys.Names.Length, "key count");
    Eq(0, GuestKeys.Ordinal(30), "A");
    Eq(25, GuestKeys.Ordinal(44), "Z");
    Eq(26, GuestKeys.Ordinal(11), "Digit0");
    Eq(Array.IndexOf(GuestKeys.Names, "Up"), GuestKeys.Ordinal(103), "Up");
    Eq(Array.IndexOf(GuestKeys.Names, "ControlRight"), GuestKeys.Ordinal(97), "RCtrl");
    Eq(Array.IndexOf(GuestKeys.Names, "Numpad7"), GuestKeys.Ordinal(71), "keypad 7");
    Eq(Array.IndexOf(GuestKeys.Names, "NumpadEnter"), GuestKeys.Ordinal(96), "keypad enter");
    Eq(-1, GuestKeys.Ordinal(120), "no key");
    for (int c = 1; c < 128; c++)
        if (GuestKeys.Ordinal(c) is var o && o >= GuestKeys.Names.Length) throw new Exception($"evdev {c} out of range");
});

Test("accelerators: parse the core's strings", () =>
{
    Eq(true, Accelerator.TryParse("Ctrl+Shift+R", out var a)); Eq(new Accelerator(true, true, false, "R"), a);
    Eq((int)'R', a.VirtualKey, "R"); Eq(true, a.TakenOverGuest);
    Eq(true, Accelerator.TryParse("Ctrl+N", out var n)); Eq(false, n.TakenOverGuest, "bare Ctrl letter stays with the guest");
    Eq(true, Accelerator.TryParse("F9", out var f)); Eq(0x78, f.VirtualKey, "F9"); Eq(true, f.TakenOverGuest);
    Eq(false, Accelerator.TryParse("Cmd+N", out _), "Cmd is not a Windows modifier");
    Eq(false, Accelerator.TryParse("", out _)); Eq(false, Accelerator.TryParse(null, out _));
    Eq(true, Accelerator.TryParse("Alt+F4", out var q)); Eq(true, q.Alt);
});

Test("egg: hinv typed in order, any case, restarting on a wrong letter", () =>
{
    bool Type(WordTyped w, string s) { var hit = false; foreach (var c in s) hit |= w.Feed(c); return hit; }
    Eq(true, Type(new WordTyped("hinv"), "hinv"), "plain");
    Eq(true, Type(new WordTyped("hinv"), "HiNv"), "mixed case");
    Eq(true, Type(new WordTyped("hinv"), "xxhhinv"), "a repeated first letter still counts");
    Eq(false, Type(new WordTyped("hinv"), "hinx"), "wrong last letter");
    Eq(false, Type(new WordTyped("hinv"), "hnv"), "missing letter");
    var w = new WordTyped("hinv");
    Eq(true, Type(w, "hinv"), "first");
    Eq(false, Type(w, "inv"), "does not match again without the h");
    Eq(true, Type(w, "hinv"), "matches again");
});

Test("pill: a machine that ended on its own says why", () =>
{
    Eq("Stopped unexpectedly. qemu exited 1", Pill.StoppedNote(new MachineStatus(MachineState.Exited, "Stopped unexpectedly", "qemu exited 1")));
    Eq("Stopped unexpectedly. the runner went away", Pill.StoppedNote(new MachineStatus(MachineState.Exited, "the runner went away", "")), "reason when there is no error");
    Eq("", Pill.StoppedNote(new MachineStatus(MachineState.Stopped, "Stopped", "old error")), "nothing unless it exited");
});

Test("commands: menu by id prefix", () =>
{
    Eq("Machine", new CommandInfo("machine.start", "Start", null).Menu);
    Eq("Window", new CommandInfo("window.console", "Console", null).Menu);
    Eq("File", new CommandInfo("app.quit", "Quit", null).Menu);
});

Test("new machine: labels, default model and when Create is offered", () =>
{
    var a = new Offering("indy-r4400", "qemu", "Indy", "ip24", 1, 1);
    var b = new Offering("octane2-vpro", "qemu", "Octane2", "ip30", 1, 2);
    Eq("indy-r4400 (1 CPU)", NewMachine.Label(a), "singular");
    Eq("octane2-vpro (2 CPUs)", NewMachine.Label(b), "plural");
    Eq(1, NewMachine.DefaultIndex(new[] { a, b }), "octane2-vpro first choice");
    Eq(0, NewMachine.DefaultIndex(new[] { a }), "else the first");
    Eq(false, NewMachine.CanCreate("   ", 0), "blank name");
    Eq(false, NewMachine.CanCreate("x", -1), "no model");
    Eq(true, NewMachine.CanCreate(" x ", 0), "name and model");
});

Test("diagnostics report: lists engine, state, heads, console and screen counts", () =>
{
    var d = new MachineDiag("Octane2", "qemu", MachineState.Running, "The machine is running.", null,
        new[] { new HeadDiag(0, 1280, 1024, true, 42) }, true, null, 1234);
    var r = DiagnosticsReport.Format(d, 17, 16);
    foreach (var want in new[] { "Octane2", "qemu", "Running", "The machine is running.", "Last error:   none", "#0  1280x1024  picture yes  sequence 42", "connected, 1234 bytes", "17 wake-ups, 16 presents" })
        if (!r.Contains(want)) throw new Exception("missing: " + want + "\n" + r);
    var dead = d with { State = MachineState.Exited, LastError = "runner died", ConsoleConnected = false, ConsoleDownReason = "closed", Heads = Array.Empty<HeadDiag>() };
    var r2 = DiagnosticsReport.Format(dead, 0, 0);
    foreach (var want in new[] { "Stopped unexpectedly", "runner died", "none", "not connected (closed)" })
        if (!r2.Contains(want)) throw new Exception("missing: " + want + "\n" + r2);
});

Test("settings: editable only when stopped, add rules, share name", () =>
{
    Eq(true, MachineSettingsRules.Editable(MachineState.Stopped), "stopped");
    Eq(true, MachineSettingsRules.Editable(MachineState.Exited), "exited");
    Eq(false, MachineSettingsRules.Editable(MachineState.Running), "running");
    Eq(false, MachineSettingsRules.Editable(MachineState.Stopping), "stopping");
    Eq(true, MachineSettingsRules.CanAddForward("ssh", 2222, 22), "valid forward");
    Eq(false, MachineSettingsRules.CanAddForward(" ", 2222, 22), "no name");
    Eq(false, MachineSettingsRules.CanAddForward("x", 0, 22), "port 0");
    Eq(false, MachineSettingsRules.CanAddForward("x", 1, 65536), "port too big");
    Eq("src", MachineSettingsRules.ShareNameFor(@"C:\work\src\"), "trailing slash");
    Eq("share", MachineSettingsRules.ShareNameFor(@"C:\"), "drive root");
});

Test("new machine: one model per id, engines that offer it", () =>
{
    var a = new Offering("x", "qemu", "P", "t", 1, 1);
    var b = new Offering("x", "iris", "P", "t", 1, 1);
    var c = new Offering("y", "qemu", "P", "t", 1, 1);
    var all = new[] { a, b, c };
    Eq(2, NewMachine.Models(all).Count, "models");
    Eq("qemu,iris", string.Join(",", NewMachine.EnginesFor(all, "x")), "engines of x");
    Eq("qemu", string.Join(",", NewMachine.EnginesFor(all, "y")), "engines of y");
});

Test("tabs: select replaces the active tab, open adds, close picks a neighbour", () =>
{
    var s = new TabSet();
    s.Select("a"); Eq(1, s.Keys.Count); Eq("a", s.Active);
    s.Select("b"); Eq(1, s.Keys.Count, "selecting another machine reuses the active tab"); Eq("b", s.Active);
    s.OpenNew("a"); Eq(2, s.Keys.Count); Eq("a", s.Active);
    s.OpenNew("c"); s.Select("b"); Eq("b", s.Active, "selecting a machine that has a tab goes to it"); Eq(3, s.Keys.Count);
    Eq("a", s.Close("b"), "closing the middle tab selects its left neighbour");
    Eq("a", s.Active);
    s.Select("d"); Eq(true, s.Keys.SequenceEqual(new[] { "d", "c" }), "d replaced the active a");
    Eq("d", s.Close("c") ?? "none", "closing an inactive tab keeps the active one");
    Eq(true, s.Close("d") is null, "closing the last tab leaves none");
    // A double click: the first click selects (replacing the active tab), the second opens a new tab and keeps the old one.
    var d = new TabSet(); d.Select("p"); d.Select("q"); d.OpenNew("q");
    Eq(true, d.Keys.SequenceEqual(new[] { "p", "q" }), "the replacement is undone, q gets a tab beside p"); Eq("q", d.Active);
    var e = new TabSet(); e.Select("a"); e.OpenNew("b"); e.Select("c"); e.OpenNew("c");
    Eq(true, e.Keys.SequenceEqual(new[] { "a", "b", "c" }), "an open right after a select that replaced b keeps b and adds c");
    var f = new TabSet(); f.Select("a"); f.Select("a"); f.OpenNew("a"); Eq(true, f.Keys.SequenceEqual(new[] { "a" }), "reopening the active machine does not duplicate it");
    s.OpenNew("x"); s.OpenNew("y"); s.Retain(new[] { "y" }); Eq(true, s.Keys.SequenceEqual(new[] { "y" }), "tabs for removed machines go");
});

Test("terminal palette: ansi, cube, greys, true colour, default", () =>
{
    Eq(0xCD3131u, TermPalette.Indexed(1)); Eq(0xFFFFFFu, TermPalette.Indexed(15));
    Eq(0x000000u, TermPalette.Indexed(16), "cube origin"); Eq(0xFFFFFFu, TermPalette.Indexed(231), "cube end"); Eq(0x5F0000u, TermPalette.Indexed(52), "cube red 1");
    Eq(0x080808u, TermPalette.Indexed(232)); Eq(0xEEEEEEu, TermPalette.Indexed(255));
    Eq(0x112233u, TermPalette.Rgb(TermColour.Rgb(0x11, 0x22, 0x33), 0));
    Eq(0xABCDEFu, TermPalette.Rgb(TermColour.Default, 0xABCDEF));
});

Test("terminal selection: order, text, wide characters, trailing blanks", () =>
{
    TermCell C(string s, bool cont = false, bool wide = false) => new(s, TermColour.Default, TermColour.Default, false, false, false, false, false, wide, cont);
    var rows = new Dictionary<int, TermCell[]>
    {
        [0] = new[] { C("h"), C("i"), C(""), C("") },
        [1] = new[] { C("\u4e16", wide: true), C("", cont: true), C("x"), C("") },
    };
    var sel = new TermSelection();
    Eq(false, sel.Active);
    sel.Begin(1, 2); sel.Extend(0, 0); // dragged backwards
    Eq("hi\n\u4e16x", sel.Text(r => rows[r], 4), "the wide character once, blanks trimmed");
    Eq(true, sel.Contains(0, 1) && sel.Contains(1, 2) && !sel.Contains(1, 3));
    sel.Clear(); Eq("", sel.Text(r => rows[r], 4));
});

Test("terminal keys: virtual keys that are not text", () =>
{
    Eq(TermKeyKind.Enter, TermKeys.FromVirtualKey(0x0D)!.Value.Kind);
    Eq(TermKeyKind.Up, TermKeys.FromVirtualKey(0x26)!.Value.Kind);
    Eq(12, TermKeys.FromVirtualKey(0x7B)!.Value.Number, "F12");
    Eq(true, TermKeys.FromVirtualKey(0x41) is null, "a letter is text");
});

Test("stills: topology first, product second, colour from the manifest", () =>
{
    var c = StillCatalog.Parse("""{"topologies":{"octane2":"octane2","tezro-ip53":"tezro-tower"},"products":{"tezro":"tezro-tower","o2":"o2"},"stills":{"octane2":{"name":"Octane2","colour":"#2e354e"},"tezro-tower":{"name":"Tezro","colour":"#4c629c"},"o2":{"name":"O2","colour":"#3d5a6d"}}}""");
    Eq(0x2E354Eu, c.Resolve("octane2", "octane2")!.Rgb);
    Eq("tezro-tower", c.Resolve("tezro", "tezro-ip53")!.Id);
    Eq("tezro-tower", c.Resolve("tezro", null)!.Id, "product fallback");
    Eq("o2", c.Resolve("o2", "unknown-topology")!.Id, "unknown topology falls back to the product");
    Eq(true, c.Resolve("fake", null) is null, "no still for an unknown machine");
});

Test("granite: two tones, repeated, pixel-doubled", () =>
{
    // 2x2 tile: light on the diagonal.
    var tile = new byte[16];
    foreach (var i in new[] { 0, 3 }) { tile[i * 4] = tile[i * 4 + 1] = tile[i * 4 + 2] = 255; tile[i * 4 + 3] = 255; }
    var o = Granite.Fill(tile, 2, 2, 8, 4, 2, 0xE2E4E8, 0xD6D8DD);
    Eq(0xE8, (int)o[0], "pixel 0 is the light tone (blue byte)");
    Eq(0xDD, (int)o[(0 * 8 + 2) * 4], "x=2 is the next tile pixel, dark");
    Eq(0xE8, (int)o[(0 * 8 + 1) * 4], "doubled: x=1 repeats x=0");
    Eq(0xE8, (int)o[(0 * 8 + 4) * 4], "the tile repeats after 2 px of 2 px: x=4 is light again");
    Eq(0xDD, (int)o[(2 * 8 + 0) * 4], "y=2 starts the tile's second row, dark first");
});

Test("view scale: fit in whole steps while that is at least 1 DIP, fractional below", () =>
{
    // 1280x1024 head. 200%: fit 2.148, whole step 2 is exactly 1 DIP.
    Eq(2.0, ViewScale.Factor(ViewMode.FitToWindow, 1280, 1024, 3000, 2200, 2.0), "200% steps to 2");
    // 150%: fit 1.855, whole step 1 is only 0.67 DIP, so the fill is fractional (1.24 DIP per guest pixel).
    Eq(true, Math.Abs(ViewScale.Factor(ViewMode.FitToWindow, 1280, 1024, 2400, 1900, 1.5) - 1900.0 / 1024) < 1e-9, "150% fractional");
    Eq(3.0, ViewScale.Factor(ViewMode.FitToWindow, 1280, 1024, 4000, 3200, 1.5), "150% steps to 3 once that is 2 DIPs");
    // 125%: fit 1.5625 is fractional; at 2.34 the whole step 2 is 1.6 DIPs.
    Eq(true, Math.Abs(ViewScale.Factor(ViewMode.FitToWindow, 1280, 1024, 2000, 1600, 1.25) - 1.5625) < 1e-9, "125% fractional");
    Eq(2.0, ViewScale.Factor(ViewMode.FitToWindow, 1280, 1024, 3000, 2500, 1.25), "125% steps to 2");
    Eq(3.0, ViewScale.Factor(ViewMode.FitToWindow, 640, 480, 2200, 1700, 1.0), "100% whole steps");
    Eq(true, Math.Abs(ViewScale.Factor(ViewMode.FitToWindow, 1280, 1024, 800, 700, 1.0) - 0.625) < 1e-9, "100% fractional below 1 DIP");
});

Test("view scale: 1x and 2x are DIPs per guest pixel and fall back to fit; actual size is a device pixel", () =>
{
    Eq(2.0, ViewScale.Factor(ViewMode.Scale1x, 1280, 1024, 3000, 2200, 2.0), "1x at 200% is 2 device px");
    Eq(2.0, ViewScale.Factor(ViewMode.Scale2x, 1280, 1024, 3000, 2200, 2.0), "2x does not fit at 200%: fit, whole 2");
    Eq(1.5, ViewScale.Factor(ViewMode.Scale1x, 1280, 1024, 2400, 1900, 1.5), "1x at 150% is 1.5 device px");
    Eq(true, Math.Abs(ViewScale.Factor(ViewMode.Scale2x, 1280, 1024, 2400, 1900, 1.5) - 1900.0 / 1024) < 1e-9, "2x at 150% falls back to fit");
    Eq(2.0, ViewScale.Factor(ViewMode.Scale2x, 640, 480, 2200, 1700, 1.0), "2x at 100%");
    Eq(1.0, ViewScale.Factor(ViewMode.ActualSize, 1280, 1024, 3000, 2200, 2.0), "actual size is one device pixel");
});

Test("accelerators: the four view shortcuts reach the app, bare Ctrl digits stay with the guest", () =>
{
    foreach (var s in new[] { "Ctrl+9", "Ctrl+0", "Ctrl+Alt+1", "Ctrl+Alt+2" })
    { Eq(true, Accelerator.TryParse(s, out var a), s); Eq(true, a.TakenOverGuest, s + " is taken"); }
    foreach (var s in new[] { "Ctrl+1", "Ctrl+2", "Ctrl+Alt+9", "Ctrl+Alt+3", "Alt+1" })
    { Accelerator.TryParse(s, out var a); Eq(false, a.TakenOverGuest, s + " stays with the guest"); }
});

Console.WriteLine(failures == 0 ? "all passed" : $"{failures} failed");
return failures;
