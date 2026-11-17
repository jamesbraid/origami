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

Console.WriteLine(failures == 0 ? "all passed" : $"{failures} failed");
return failures;
