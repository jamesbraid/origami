#if ORIGAMI_FAKE
using System.Text;
using System.Text.Json;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Origami.Logic;
using OStep = uniffi.origami_ffi.OrigamiInstallStep;
using OrigamiUiString = uniffi.origami_ffi.OrigamiUiString;

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

    // A ContentDialog lives in a popup, so search the open popups; an id of null matches the first of the type.
    static T? FindOpen<T>(MainWindow w, string? id) where T : FrameworkElement
    {
        T? In(DependencyObject o)
        {
            if (o is T t && (id == null || Microsoft.UI.Xaml.Automation.AutomationProperties.GetAutomationId(t) == id)) return t;
            for (var i = 0; i < VisualTreeHelper.GetChildrenCount(o); i++) if (In(VisualTreeHelper.GetChild(o, i)) is { } hit) return hit;
            return null;
        }
        foreach (var p in VisualTreeHelper.GetOpenPopupsForXamlRoot(w.XamlRootForTest)) if (p.Child is { } c && In(c) is { } hit) return hit;
        return null;
    }

    static T? FindWhere<T>(MainWindow w, Func<T, bool> match) where T : FrameworkElement
    {
        T? In(DependencyObject o)
        {
            if (o is T t && match(t)) return t;
            for (var i = 0; i < VisualTreeHelper.GetChildrenCount(o); i++) if (In(VisualTreeHelper.GetChild(o, i)) is { } hit) return hit;
            return null;
        }
        foreach (var p in VisualTreeHelper.GetOpenPopupsForXamlRoot(w.XamlRootForTest)) if (p.Child is { } c && In(c) is { } hit) return hit;
        return null;
    }

    /// <summary>Every element of type T in the open popups.</summary>
    static List<T> AllOpen<T>(MainWindow w) where T : FrameworkElement
    {
        var found = new List<T>();
        void In(DependencyObject o)
        {
            if (o is T t) found.Add(t);
            for (var i = 0; i < VisualTreeHelper.GetChildrenCount(o); i++) In(VisualTreeHelper.GetChild(o, i));
        }
        foreach (var p in VisualTreeHelper.GetOpenPopupsForXamlRoot(w.XamlRootForTest)) if (p.Child is { } c) In(c);
        return found;
    }

    /// <summary>Text the open dialog cuts short with an ellipsis, and buttons narrower than their content.</summary>
    static List<string> CutShort(MainWindow w)
    {
        var cut = AllOpen<TextBlock>(w).Where(t => t.ActualWidth > 0 && t.IsTextTrimmed).Select(t => t.Text).ToList();
        cut.AddRange(AllOpen<Button>(w).Where(b => b.ActualWidth > 0 && b.Content is FrameworkElement c && c.DesiredSize.Width > b.ActualWidth - b.Padding.Left - b.Padding.Right + 1)
            .Select(b => "button " + Microsoft.UI.Xaml.Automation.AutomationProperties.GetAutomationId(b)));
        return cut;
    }

    /// <summary>The open dialog's button with this label, such as the primary "Create".</summary>
    static Button? FindButton(MainWindow w, string label) => FindWhere<Button>(w, b => b.Content as string == label);

    /// <summary>A click, as the automation tree would give it.</summary>
    static void Press(Button? b) { if (b != null) new Microsoft.UI.Xaml.Automation.Peers.ButtonAutomationPeer(b).Invoke(); }

    /// <summary>A picture of the window for the design review; a failure to take one never fails the run.</summary>
    static async Task Capture(MainWindow w, string dir, string name)
    {
        try { await Task.Delay(400); await Shot.Window(w.Hwnd, Path.Combine(dir, "shot-" + name + ".png")); Note("picture " + name, "saved"); }
        catch (Exception e) { Note("picture " + name, "not taken: " + e.Message); }
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
        _ = Task.Run(async () => { await Task.Delay(600_000); w.DispatcherQueue.TryEnqueue(() => Fail("watchdog: 600 s")); });
        try { await Scenario(w, core, dir); }
        catch (Exception e) { Check("exception", false, e.ToString()); }
        Finish();
    }

    [System.Runtime.InteropServices.DllImport("shell32", CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
    static extern uint ExtractIconEx(string file, int index, IntPtr[]? large, IntPtr[]? small, uint n);

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
        var missing = coreCommands.Where(c => !w.Shortcuts.ContainsKey(c.Id) || w.Shortcuts[c.Id] != c.WindowsAccelerator).Select(c => c.Id).ToList();
        Check("shortcuts: the window carries every core command with its Windows shortcut", coreCommands.Count > 10 && missing.Count == 0,
            $"{coreCommands.Count} core commands, {w.Commands.Count} in the window, mismatched: {string.Join(",", missing)}");
        var key = w.SelectedKey!;
        Check("a machine is selected", !string.IsNullOrEmpty(key), key);

        // --- the top bar, the command bar and the library rows of the 0.6.2 design ---
        Check("title bar: it holds only the app icon and the title", w.TitleBarTitleForTest == Ui.T(OrigamiUiString.Origami) && !w.TitleBarHasContentForTest, $"title '{w.TitleBarTitleForTest}', content {w.TitleBarHasContentForTest}");
        var primary = w.PrimaryCommandNamesForTest;
        var more = w.SecondaryCommandNamesForTest;
        Check("command bar: Start, Power off, Console, Capture mouse and Settings are on the bar; the rest is under More",
            primary.SequenceEqual(new[] { "StartButton", "PowerHost", "ConsoleButton", "PointerButton", "SettingsButton" })
            && more.SequenceEqual(new[] { "CpuButton", "DiagnosticsButton", "ScreenshotButton", "InstallButton", "ShortcutsButton", "PreferencesButton", "AboutButton" }), string.Join(",", primary) + " | " + string.Join(",", more));
        Check("command bar: CPU load and Cancel start carry the core's words", w.Commands.First(c => c.Id == "view.cpu-load").Title == "CPU load" && w.Commands.First(c => c.Id == "machine.cancel-start").Title == "Cancel start");
        Check("command bar: Cancel start is on only while the machine starts", core.CommandEnabled("machine.cancel-start", MachineState.Starting) && !core.CommandEnabled("machine.cancel-start", MachineState.Running) && !core.CommandEnabled("machine.cancel-start", MachineState.Stopped));
        Check("command bar: Capture mouse and Keyboard shortcuts carry the core's words",
            w.Commands.First(c => c.Id == "machine.capture-mouse").Title == "Capture mouse" && w.Commands.First(c => c.Id == "view.shortcuts").Title == Ui.T(OrigamiUiString.KeyboardShortcuts));
        Check("rows: no capsule in any row, and a stopped row says its model", w.RowCapsulesForTest == 0 && core.Machines().All(m => w.RowLineForTest(m.Key) == core.Row(m.Key).Line && !w.RowHasDotForTest(m.Key)),
            string.Join(",", core.Machines().Select(m => w.RowLineForTest(m.Key))));
        Check("rows: the tooltip and label are the core's description", core.Machines().All(m => w.RowDescriptionForTest(m.Key) == core.Row(m.Key).Description && core.Row(m.Key).Description.StartsWith(m.Name)), w.RowDescriptionForTest(key) ?? "none");
        Check("page: the header names the machine and its state, the status bar is hidden while stopped",
            w.PillTextForTest == "Stopped" && !w.StatusBarShownForTest, $"{w.PillTextForTest}, status bar {w.StatusBarShownForTest}");
        await Capture(w, dir, "main-stopped");

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
        Check("new machine: the catalogue offers machines", offers.Count > 0 && core.DefaultOffering(offers) < offers.Count, string.Join(",", offers.Select(NewMachine.Label)));
        var made = await core.CreateMachineAsync("Selftest created", offers[core.DefaultOffering(offers)].Id);
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
        var shareRow = st3.Shares.First(x => x.Name == "shared");
        Check("shares: the row carries the core's mount commands", shareRow.MountLines is { Count: 2 } ml && ml[0] == "mkdir -p /mnt/shared" && ml[1] == "mount 10.98.0.4:/shared /mnt/shared", string.Join("|", shareRow.MountLines ?? new List<string>()));
        Check("shares: a name is suggested from the folder and a bad one is refused", core.SuggestShareName(@"C:\work\My Code\") == "My-Code" && core.CheckShareName("a/b") != null && core.CheckShareName("src") == null, core.CheckShareName("a/b") ?? "no refusal");
        await core.ApplyChangeAsync(made, new MachineChange.RemoveShare("shared"));
        await core.ApplyChangeAsync(made, new MachineChange.SetNetwork("none"));
        var st4 = await core.SettingsAsync(made);
        Check("settings: a share is removed and the network mode set", st4.Shares.Count == 0 && st4.Network.Mode == "none", st4.Network.Mode);

        // --- settings as pages: the eight the other apps have, the console line on Startup & firmware, Bridged on Network ---
        var selected = w.SelectedKey;
        w.SelectForTest(made);
        async Task<bool> GoPage(string page)
        {
            if (!await Until(() => MachineSettingsDialog.Current is not null, 3000)) return false;
            var nav = MachineSettingsDialog.Current!;
            nav.SelectedItem = nav.MenuItems.OfType<NavigationViewItem>().First(i => (string)i.Tag == page);
            return await Until(() => FindOpen<FrameworkElement>(w, "settings.error") != null, 3000);
        }
        w.OpenSettingsForTest(null);
        await Until(() => MachineSettingsDialog.Current is not null, 3000);
        var pageNames = MachineSettingsDialog.Current?.MenuItems.OfType<NavigationViewItem>().Select(i => (string)i.Content).ToList() ?? new List<string>();
        Check("settings: eight pages in the order the other apps list them", pageNames.SequenceEqual(MachineSettingsDialog.Pages().Select(p => p.Title)) && pageNames.Count == 8, string.Join(",", pageNames));
        await Capture(w, dir, "settings-general");
        await GoPage("startup");
        ComboBox? line = null;
        await Until(() => (line = FindOpen<ComboBox>(w, "settings.console")) != null, 3000);
        Check("console line: Settings lists the machine's lines with the default marked", line != null && line.Items.Count == 2 && line.SelectedIndex == 0 && (string)line.Items[0] == "ttyS0 (default)", line == null ? "no combo" : string.Join(",", line.Items));
        Check("console line: the page says where startup messages appear, in the core's words", FindOpen<TextBlock>(w, "settings.startup")?.Text is "Screen" or "Console", FindOpen<TextBlock>(w, "settings.startup")?.Text ?? "none");
        if (line != null)
        {
            // The page is rebuilt after an edit; the combo just set is stale until a new one replaces it.
            var stale = line;
            stale.SelectedIndex = 1;
            var chosen = await Until(() => core.SettingsAsync(made).Result.ConsolePort == "ttyS1", 3000);
            line = null;
            await Until(() => (line = FindOpen<ComboBox>(w, "settings.console")) is { SelectedIndex: 1 } c && !ReferenceEquals(c, stale), 3000);
            var fresh = line is { SelectedIndex: 1 } && !ReferenceEquals(line, stale);
            Check("console line: picking the second line is written and the page shows it", chosen && fresh, $"file {core.SettingsAsync(made).Result.ConsolePort}, rebuilt combo at {line?.SelectedIndex.ToString() ?? "none"}");
            if (fresh) line!.SelectedIndex = 0;
            var back = await Until(() => core.SettingsAsync(made).Result.ConsolePort == "ttyS0", 3000);
            Check("console line: picking the default line puts the choice back", fresh && back, $"file {core.SettingsAsync(made).Result.ConsolePort}");
        }
        // The Settings dialog cuts no text short on the pages with the most on them.
        var cutSettings = new List<string>();
        foreach (var page in new[] { "general", "startup", "network" }) { await GoPage(page); await Task.Delay(300); cutSettings.AddRange(CutShort(w)); }
        Check("layout: no text in Settings is cut short", cutSettings.Count == 0, string.Join(" | ", cutSettings));
        // --- network: Bridged opens its fields first, applies with its device; a moved subnet returns the lease advice ---
        var modes = core.NetworkModes().ToList();
        var bridgedAt = modes.FindIndex(c => c.Label == "Bridged");
        await GoPage("network");
        RadioButtons? netMode = null;
        await Until(() => (netMode = FindOpen<RadioButtons>(w, "settings.network")) != null, 3000);
        Check("network: the modes are radio buttons with the core's captions and no Apply for them", netMode != null && netMode.Items.Count == modes.Count && FindOpen<Button>(w, "settings.network.apply") == null && modes.All(m => m.Detail.Length > 0));
        if (netMode != null) netMode.SelectedIndex = bridgedAt;
        TextBox? device = null;
        await Until(() => (device = FindOpen<TextBox>(w, "settings.bridge.device")) != null, 3000);
        var beforeApply = core.SettingsAsync(made).Result.Network.Mode;
        Check("network: choosing Bridged opens its fields and saves nothing yet", device != null && beforeApply == "none", $"fields={(device != null)} mode={beforeApply}");
        Check("network: Bridged has no Apply button of its own", FindOpen<Button>(w, "settings.bridge.apply") == null);
        // The fields apply as one edit when focus leaves them.
        if (device != null && FindOpen<TextBox>(w, "settings.bridge.share") is { } shareBox) { device.Focus(FocusState.Programmatic); device.Text = "tap0"; shareBox.Focus(FocusState.Programmatic); }
        var bridgedOn = await Until(() => core.SettingsAsync(made).Result.Network is { Mode: "tap" or "vmnet" } n && n.Device == "tap0", 3000);
        var netNow = core.SettingsAsync(made).Result.Network;
        Check("network: Bridged is applied with its device and the Ethernet address is the machine's", bridgedOn && netNow.Mac is { } m0 && m0.StartsWith("08:00:69:") && netNow.MacNote.Contains("ID"), $"{netNow.Mode} {netNow.Device} {netNow.Mac} {netNow.MacNote}");
        await Capture(w, dir, "settings-network");
        Check("network: the Ethernet address shows as a value with the core's caption", await Until(() => FindOpen<TextBlock>(w, "settings.mac")?.Text == netNow.Mac, 3000), FindOpen<TextBlock>(w, "settings.mac")?.Text ?? "none");
        FindOpen<ContentDialog>(w, null)?.Hide();
        await Until(() => FindOpen<ContentDialog>(w, null) == null, 2000);
        await core.ApplyChangeAsync(made, new MachineChange.SetNetwork("user"));
        var movedLines = await core.ApplyChangeAsync(made, new MachineChange.SetNetwork("user", "", null, "10.42.0.0/24"));
        Check("network: a moved subnet returns the lease advice with a command to copy", core.SettingsAsync(made).Result.Network.Subnet == "10.42.0.0/24" && MachineSettingsRules.Notes(movedLines).Any(n => n.Copy?.StartsWith("rm /var/adm") == true), string.Join("|", movedLines.Select(l => l.Text)));
        string publicSubnet = "";
        try { await core.ApplyChangeAsync(made, new MachineChange.SetNetwork("user", "", null, "8.8.8.0/24")); } catch (Exception e) { publicSubnet = e.Message; }
        Check("network: a public subnet is refused with the core's sentence", publicSubnet.Length > 0 && core.SettingsAsync(made).Result.Network.Subnet == "10.42.0.0/24", publicSubnet);
        if (selected != null) w.SelectForTest(selected);

        // --- disks: the real New Machine dialog with an empty disk, an image copied in and one used in place; the badge; the CD ---
        var irix = Path.Combine(Environment.GetEnvironmentVariable("ORIGAMI_FIXTURES") ?? "", "disk-irix-6.5.30.img");
        Check("disks: the IRIX fixture is found", File.Exists(irix), irix);
        string DiskKey(string n) => core.Machines().FirstOrDefault(m => m.Name == n)?.Key ?? "";
        // The disk choice is a list of radio buttons in the core's order; pick the one whose title the core gives to this choice.
        void PickDisk(uniffi.origami_ffi.OrigamiDiskChoice choice)
        {
            var rb = FindOpen<RadioButtons>(w, "new.disk")!;
            var title = uniffi.origami_ffi.OrigamiFfiMethods.OrigamiDiskOptions(true).First(o => o.Choice == choice).Title;
            rb.SelectedIndex = rb.Items.OfType<StackPanel>().ToList().FindIndex(i => i.Children.OfType<StackPanel>().First().Children.OfType<TextBlock>().First().Text == title);
        }
        async Task<(bool Made, string Note, string Badge)> NewWithDisk(string machineName, uniffi.origami_ffi.OrigamiDiskChoice kind, string? file, bool wantCopy)
        {
            TextBox? nameBox = null;
            // The command is ignored while an earlier dialog is still closing, so ask again.
            for (var tries = 0; tries < 4 && nameBox == null; tries++)
            {
                w.Invoke("machine.new");
                await Until(() => (nameBox = FindOpen<TextBox>(w, "new.name")) != null, 2000);
            }
            if (nameBox == null) return (false, "no dialog", "");
            PickDisk(kind);
            if (kind == uniffi.origami_ffi.OrigamiDiskChoice.Install) FindOpen<NumberBox>(w, "new.size")!.Value = 1;
            if (file != null)
            {
                PathPicker.Answers.Enqueue(file);
                Press(FindOpen<Button>(w, "disk.choose"));
                await Until(() => FindOpen<TextBlock>(w, "disk.note")?.Visibility == Visibility.Visible, 5000);
                if (!wantCopy) FindOpen<ComboBox>(w, "disk.place")!.SelectedIndex = 1;
            }
            var noteText = FindOpen<TextBlock>(w, "disk.note")?.Text ?? "";
            var badgeText = FindOpen<TextBlock>(w, "disk.badge")?.Text ?? "";
            nameBox.Text = machineName;
            await Until(() => FindButton(w, "Create") is { IsEnabled: true }, 3000);
            Press(FindButton(w, "Create"));
            var listed = await Until(() => core.Machines().Any(m => m.Name == machineName), 60_000); // a 1 GB raw disk is slow to make on NTFS
            await Until(() => FindOpen<ContentDialog>(w, null) == null, 10_000);
            return (listed, noteText, badgeText);
        }
        var (emptyMade, _, _) = await NewWithDisk("Disk empty", uniffi.origami_ffi.OrigamiDiskChoice.Install, null, true);
        var emptyDrives = emptyMade ? (await core.SettingsAsync(DiskKey("Disk empty"))).Drives : new List<Drive>();
        Check("disks: an empty disk is made with the machine at target 1", emptyDrives.Count == 1 && emptyDrives[0].Target == 1, string.Join(",", emptyDrives.Select(d => d.Image)));
        var (copyMade, copyNote, copyBadge) = await NewWithDisk("Disk copy", uniffi.origami_ffi.OrigamiDiskChoice.Image, irix, true);
        var copyDrives = copyMade ? (await core.SettingsAsync(DiskKey("Disk copy"))).Drives : new List<Drive>();
        Check("disks: a copied image is kept in the machine's folder",
            copyDrives.Count == 1 && copyDrives[0].Image == "drives/disk-irix-6.5.30.img" && File.Exists(Path.Combine(core.LibraryRoot, "Disk copy", "drives", "disk-irix-6.5.30.img")), string.Join(",", copyDrives.Select(d => d.Image)));
        Check("disks: copying says the original is safe", copyNote.Contains("keeps your original safe"), copyNote);
        Check("disks: the sheet names the OS on the disk", copyBadge == "IRIX 6.5.30", copyBadge);
        core.Relist();
        Check("disks: the library row's label names the OS on the disk and the row has no badge", await Until(() => { core.Relist(); return w.RowDescriptionForTest(DiskKey("Disk copy"))?.EndsWith("IRIX 6.5.30") == true; }, 15_000), w.RowDescriptionForTest(DiskKey("Disk copy")) ?? "none");
        var (placeMade, placeNote, _) = await NewWithDisk("Disk place", uniffi.origami_ffi.OrigamiDiskChoice.Image, irix, false);
        var placeDrives = placeMade ? (await core.SettingsAsync(DiskKey("Disk place"))).Drives : new List<Drive>();
        Check("disks: an image used in place stays where it is",
            placeDrives.Count == 1 && placeDrives[0].Image.EndsWith("disk-irix-6.5.30.img") && !File.Exists(Path.Combine(core.LibraryRoot, "Disk place", "drives", "disk-irix-6.5.30.img")), string.Join(",", placeDrives.Select(d => d.Image)));
        Check("disks: an image used in place warns that IRIX writes into it", placeNote.Contains("IRIX will write into this file"), placeNote);
        // A ready-made disk, when the catalogue has one, is the first choice and carries the Recommended tag; Preferences can list the downloads.
        var withReady = uniffi.origami_ffi.OrigamiFfiMethods.OrigamiDiskOptions(true);
        Check("disks: the ready-made choice comes first with the core's tag and the downloads can be listed",
            withReady[0].Choice == uniffi.origami_ffi.OrigamiDiskChoice.Ready && withReady[0].Recommended && core.CachedDisks() is not null, withReady[0].Title);
        // The CD: attached in Settings while stopped, then swapped and ejected on the running machine, whose file keeps the first.
        var cdKey = await core.CreateMachineAsync("Disk cd", offers[core.DefaultOffering(offers)].Id);
        var (cdOne, cdTwo) = (Path.Combine(dir, "one.iso"), Path.Combine(dir, "two.iso"));
        foreach (var cdFile in new[] { cdOne, cdTwo }) File.WriteAllBytes(cdFile, new byte[2048]);
        w.Reload();
        w.SelectForTest(cdKey);
        async Task<bool> OpenSettings(string button)
        {
            for (var tries = 0; tries < 4; tries++)
            {
                w.OpenSettingsForTest("drives");
                if (await Until(() => FindOpen<Button>(w, button) != null, 2000)) return true;
            }
            return false;
        }
        var settingsOpen = await OpenSettings("settings.insertcd");
        var insertEnabled = await Until(() => FindOpen<Button>(w, "settings.insertcd") is { IsEnabled: true }, 3000);
        Check("disks: Settings offers a CD-ROM when the machine has none", settingsOpen && insertEnabled, $"opened {settingsOpen}, enabled {insertEnabled}");
        FindOpen<ContentDialog>(w, null)?.Hide();
        await Until(() => FindOpen<ContentDialog>(w, null) == null, 2000);
        var cdTarget = (await core.SettingsAsync(cdKey)).CdTarget ?? 4;
        await core.ApplyChangeAsync(cdKey, new MachineChange.AttachCd(cdOne, cdTarget));
        Check("disks: a CD image is attached while stopped", (await core.SettingsAsync(cdKey)).Drives.Any(d => d.Kind == "cdrom"));
        var cdDrive = (await core.SettingsAsync(cdKey)).Drives.FirstOrDefault(d => d.Kind == "cdrom")?.Name ?? "cdrom4";
        var cdRunning = false;
        for (var tries = 0; tries < 3 && !cdRunning; tries++)
        {
            w.SelectForTest(cdKey);
            w.Invoke("machine.start");
            cdRunning = await Until(() => core.Status(cdKey).State == MachineState.Running, 15_000);
        }
        Check("disks: the machine with the CD starts", cdRunning, $"{core.Status(cdKey).State}: {core.Status(cdKey).Reason}");
        PathPicker.Answers.Enqueue(cdTwo);
        var opened = await OpenSettings("settings.cd.change." + cdDrive);
        Press(FindOpen<Button>(w, "settings.cd.change." + cdDrive));
        Check("disks: the CD is swapped while the machine runs", await Until(() => core.FakeMedia(cdKey).SequenceEqual(new[] { $"{cdDrive} {cdTwo}" }), 3000),
            $"settings opened {opened}, machine {core.Status(cdKey).State}, media {string.Join("|", core.FakeMedia(cdKey))}");
        await Until(() => FindOpen<Button>(w, "settings.cd.eject." + cdDrive) != null, 3000);
        Press(FindOpen<Button>(w, "settings.cd.eject." + cdDrive));
        Check("disks: the CD is ejected from Settings while the machine runs", await Until(() => core.FakeMedia(cdKey).LastOrDefault() == $"{cdDrive} eject", 3000), string.Join("|", core.FakeMedia(cdKey)));
        FindOpen<ContentDialog>(w, null)?.Hide();
        await Until(() => FindOpen<ContentDialog>(w, null) == null, 2000);
        Check("disks: swapping the CD leaves the machine's file alone", (await core.SettingsAsync(cdKey)).Drives.First(d => d.Kind == "cdrom").Image.EndsWith("one.iso"));
        w.Invoke("machine.stop", true);
        await Until(() => core.Status(cdKey).State == MachineState.Stopped, 8000);
        if (selected != null) w.SelectForTest(selected);

        // --- Install IRIX: the assistant start to finish on a machine with no disk, through its real window ---
        var instKey = await core.CreateMachineAsync("Install test", "origin200.1");
        var netBefore = (await core.SettingsAsync(instKey)).Network.Mode;
        Check("install: the fake machines run IRIX", core.Machines().First(m => m.Key == instKey).RunsIrix);
        w.Reload();
        w.SelectForTest(instKey);
        await Until(() => w.SelectedKey == instKey, 2000);
        InstallWindow? iw = null;
        for (var tries = 0; tries < 4 && iw == null; tries++)
        {
            w.Invoke("machine.install-irix");
            await Until(() => (iw = w.InstallForTest(instKey)) != null, 2000);
        }
        Check("install: Machine > Install IRIX opens the assistant", iw != null);
        if (iw != null)
        {
            async Task<bool> OnStep(OStep s) => await Until(() => iw.StepForTest == s, 8000);
            async Task Go()
            {
                await Until(() => iw.Find<Button>("install.continue") is { IsEnabled: true }, 8000);
                Press(iw.Find<Button>("install.continue"));
            }
            await OnStep(OStep.Release);
            Check("install: step 1 offers 6.5.30 and Standard by default",
                await Until(() => iw.Find<RadioButton>("install.release.6.5.30")?.IsChecked == true && iw.Find<RadioButton>("install.profile.standard")?.IsChecked == true, 5000));
            await Go();
            await OnStep(OStep.Media);
            var emptyMedia = Path.Combine(dir, "empty-media");
            Directory.CreateDirectory(emptyMedia);
            PathPicker.Answers.Enqueue(emptyMedia);
            await Until(() => iw.Find<RadioButton>("install.media.folder") != null, 5000);
            iw.Find<RadioButton>("install.media.folder")!.IsChecked = true;
            await Until(() => iw.Find<Button>("install.media.choose") is { Visibility: Visibility.Visible }, 3000);
            Press(iw.Find<Button>("install.media.choose"));
            Check("install: an empty media folder is refused in the core's words",
                await Until(() => iw.Find<TextBlock>("install.media.verdict")?.Text == "That folder has no 6.5.30 or 6.5-base folder.", 5000), iw.Find<TextBlock>("install.media.verdict")?.Text ?? "none");
            Check("install: Continue waits while the folder is incomplete and says why", iw.Find<Button>("install.continue") is { IsEnabled: false } && iw.ReasonForTest == "Choose a folder that has every disc.", iw.ReasonForTest);
            Check("install: the pages list numbered steps in the core's words", iw.StepNamesForTest.Count >= 6 && iw.StepNamesForTest[0] == "1. Release" && iw.StepNamesForTest[1] == "2. Discs", string.Join(",", iw.StepNamesForTest));
            iw.Find<RadioButton>("install.media.download")!.IsChecked = true;
            await Go();
            await OnStep(OStep.Software);
            await Until(() => iw.Find<CheckBox>("install.addon.mipspro") != null, 8000);
            iw.Find<CheckBox>("install.addon.mipspro")!.IsChecked = true;
            await Go();
            if (core.Open(instKey).BootConsole() == uniffi.origami_ffi.OrigamiBootConsole.Serial)
            {
                await OnStep(OStep.Method);
                Check("install: a serial machine has a Method step with Automatic chosen", await Until(() => iw.Find<RadioButton>("install.method.automatic")?.IsChecked == true, 5000));
                iw.Find<RadioButton>("install.method.guided")!.IsChecked = true;
                await Go();
            }
            await OnStep(OStep.Disk);
            Check("install: a machine with no disk offers one at the default size",
                await Until(() => iw.Find<NumberBox>("install.disk.size")?.Value == core.Labels().DefaultGb, 8000));
            await Go();
            await OnStep(OStep.Check);
            Check("install: Continue made the disk", (await core.SettingsAsync(instKey)).Drives.Any(d => d.Kind == "disk"));
            await Until(() => iw.Find<Button>("install.check.run") != null, 5000);
            Press(iw.Find<Button>("install.check.run"));
            Check("install: the check passes", await Until(() => iw.Find<TextBlock>("install.check.status")?.Text == "Every disc checked out.", 30_000), iw.Find<TextBlock>("install.check.status")?.Text ?? "none");
            await Go();
            // The guide takes the pane: the assistant steps aside, the library gives way to the steps, and the footer names the install files.
            Check("install: the guide takes the pane from the library and the assistant steps aside", await Until(() => w.GuideShownForTest && !iw.AppWindow.IsVisible, 15_000), $"guide {w.GuideShownForTest}, window {iw.AppWindow.IsVisible}");
            Check("install: the footer says what the install files are, with no address", await Until(() => w.GuideFooterForTest.StartsWith("Install files ready: IRIX 6.5.30, Standard + MIPSpro"), 15_000), w.GuideFooterForTest);
            var guideSteps = core.Raw.InstallGuide(instKey).Steps;
            Check("install: the header counts the guide's steps in the core's words", w.GuideHeaderForTest == $"Install IRIX, step 1 of {guideSteps.Length}", w.GuideHeaderForTest);
            Check("install: the machine is running", await Until(() => core.Status(instKey).State == MachineState.Running, 15_000), core.Status(instKey).Reason);
            await Capture(w, dir, "install-guide");
            Check("install: the machine's row says Installing", await Until(() => w.PillTextForTest == "Installing", 5000), w.PillTextForTest);
            Press(w.GuideButtonForTest("guide.cancel"));
            Check("install: Cancel while the install runs asks the core's question with a Back button",
                await Until(() => iw.OpenDialog is { Title: "Stop the machine?", CloseButtonText: "Back" }, 5000), iw.OpenDialog?.Title as string ?? "none");
            iw.OpenDialog?.Hide();
            Check("install: Back keeps the assistant open with its guide",
                await Until(() => iw.OpenDialog == null, 5000) && w.InstallForTest(instKey) == iw && w.GuideFooterForTest.StartsWith("Install files ready"), w.GuideFooterForTest);
            var serialMachine = core.Open(instKey).BootConsole() == uniffi.origami_ffi.OrigamiBootConsole.Serial;
            var firstStep = guideSteps[0];
            var typeIt = w.GuideButtonForTest("guide.typeit.0");
            Check("install: the current step shows what to watch for, and Type it only on a serial machine",
                firstStep.Watch is { Length: > 0 } && w.GuideTypedForTest.Contains(firstStep.Watch) && (typeIt != null) == (serialMachine && firstStep.Typed.Length > 0 && firstStep.Send != null && core.Status(instKey).State == MachineState.Running),
                $"serial {serialMachine}, send {firstStep.Send != null}, shown '{string.Join("|", w.GuideTypedForTest)}'");
            if (typeIt != null)
            {
                Press(typeIt);
                var typedText = firstStep.Send!.TrimEnd('\r', '\n');
                Check("install: Type it reaches the guest console",
                    await Until(() => Encoding.UTF8.GetString(core.ConsoleRead(instKey, 0, out _)).Contains(typedText), 5000), typedText);
            }
            w.Invoke("machine.stop", true);
            Check("install: the machine stops", await Until(() => core.Status(instKey).State == MachineState.Stopped, 10_000), core.Status(instKey).Reason);
            await Until(() => w.GuideButtonForTest("guide.finish") is { IsEnabled: true }, 8000);
            Press(w.GuideButtonForTest("guide.finish"));
            Check("install: Finish closes the assistant", await Until(() => w.InstallForTest(instKey) == null, 8000));
            Check("install: Finish says IRIX is installed", await Until(() => FindOpen<ContentDialog>(w, null) != null, 5000));
            FindOpen<ContentDialog>(w, null)?.Hide();
            await Until(() => FindOpen<ContentDialog>(w, null) == null, 2000);
            var settingsShown = await OpenSettings("settings.insertcd");
            Check("install: the network is back as it was", settingsShown && (await core.SettingsAsync(instKey)).Network.Mode == netBefore, $"{netBefore} then {(await core.SettingsAsync(instKey)).Network.Mode}");
            FindOpen<ContentDialog>(w, null)?.Hide();
            await Until(() => FindOpen<ContentDialog>(w, null) == null, 2000);
        }
        // Automatic: the fake engine has no console runner, so Install fails with the core's sentence and Continue stays off.
        var autoKey = await core.CreateMachineAsync("Install auto", "origin200.1");
        w.Reload();
        w.SelectForTest(autoKey);
        InstallWindow? aw = null;
        for (var tries = 0; tries < 4 && aw == null; tries++)
        {
            w.Invoke("machine.install-irix");
            await Until(() => (aw = w.InstallForTest(autoKey)) != null, 2000);
        }
        if (aw != null)
        {
            async Task Walk(InstallWindow win, params OStep[] steps)
            {
                foreach (var stepWant in steps)
                    // A press while the window is busy (making the disk, starting the machine) is refused, so press again.
                    for (var tries = 0; tries < 6 && win.StepForTest != stepWant; tries++)
                    {
                        await Until(() => win.Find<Button>("install.continue") is { IsEnabled: true }, 20_000);
                        try { Press(win.Find<Button>("install.continue")); } catch (System.Runtime.InteropServices.COMException) { await Task.Delay(500); }
                        await Until(() => win.StepForTest == stepWant, 4000);
                    }
            }
            await Walk(aw, OStep.Media, OStep.Software, OStep.Method, OStep.Disk, OStep.Check);
            await Until(() => aw.Find<Button>("install.check.run") != null, 5000);
            Press(aw.Find<Button>("install.check.run"));
            await Until(() => aw.Find<TextBlock>("install.check.status")?.Text == "Every disc checked out.", 30_000);
            await Walk(aw, OStep.Packages);
            Check("install: Automatic offers Install in the pane and keeps Finish off", await Until(() => w.GuideButtonForTest("guide.install") is { IsEnabled: true }, 8000) && w.GuideButtonForTest("guide.finish") == null);
            Press(w.GuideButtonForTest("guide.install"));
            Check("install: a failed Install shows the core's sentence and Finish stays off",
                await Until(() => w.GuideFailureForTest.Length > 0, 20_000) && w.GuideButtonForTest("guide.finish") == null, w.GuideFailureForTest);
            await aw.StopAndCloseAsync();
        }
        // A screen console having no Method step is covered by the core's flow test: fake machines have no graphics pipe.
        if (selected != null) w.SelectForTest(selected);
        // --- New machine: its groups, its disk list in the core's order, and its errors under their fields ---
        for (var tries = 0; tries < 4 && FindOpen<TextBox>(w, "new.name") == null; tries++)
        {
            w.Invoke("machine.new");
            await Until(() => FindOpen<TextBox>(w, "new.name") != null, 2000);
        }
        var diskChoice = FindOpen<RadioButtons>(w, "new.disk");
        if (diskChoice != null)
        {
            await Task.Delay(300);
            await Capture(w, dir, "new-machine");
            var cutNew = CutShort(w);
            Check("layout: no text in New machine is cut short", cutNew.Count == 0, string.Join(" | ", cutNew));
            var fields = new[] { "new.engine", "new.model", "new.name" }
                .Select(id => FindOpen<FrameworkElement>(w, id)).Select(e => e switch { ComboBox c => (string)c.Header, TextBox t => (string)t.Header, _ => "" })
                .Append(AllOpen<Expander>(w).Select(e => e.Header as string ?? "").FirstOrDefault() ?? "").ToList();
            Check("layout: New machine lists Engine, Model, Name and Advanced, with headers above their controls", fields.SequenceEqual(new[] { Labels.NewMachine.Engine, Labels.NewMachine.Model, Labels.NewMachine.Name, Labels.NewMachine.Advanced }), string.Join(",", fields));
            var groups = AllOpen<TextBlock>(w).Select(t => t.Text).Where(t => t is "Machine" or "Disk").ToList();
            Check("layout: New machine has a Machine group and a Disk group", groups.SequenceEqual(new[] { Ui.T(OrigamiUiString.Machine), Ui.T(OrigamiUiString.Disk) }), string.Join(",", groups));
            var titles = diskChoice.Items.OfType<StackPanel>().Select(i => i.Children.OfType<StackPanel>().First().Children.OfType<TextBlock>().First().Text).ToList();
            var coreTitles = uniffi.origami_ffi.OrigamiFfiMethods.OrigamiDiskOptions(diskChoice.Items.Count == 4).Select(o => o.Title).ToList();
            Check("disk: the choices are the core's, in its order, the first selected", titles.SequenceEqual(coreTitles.Where(t => titles.Contains(t))) && diskChoice.SelectedIndex == 0, string.Join(",", titles));
            Check("disk: Install from discs is offered on an IRIX machine", titles.Contains(uniffi.origami_ffi.OrigamiFfiMethods.OrigamiDiskOptions(false).First(o => o.Choice == uniffi.origami_ffi.OrigamiDiskChoice.Install).Title));
            // A name the core refuses: its sentence stays on the dialog, under the name, where it can be read.
            FindOpen<TextBox>(w, "new.name")!.Text = "a/b";
            await Until(() => FindButton(w, "Create") is { IsEnabled: true }, 3000);
            Press(FindButton(w, "Create"));
            TextBlock? nameError = null;
            await Until(() => (nameError = FindOpen<TextBlock>(w, "new.name.error")) is { Visibility: Visibility.Visible, Text.Length: > 0 }, 5000);
            var inView = nameError is { ActualHeight: > 0 } && nameError.TransformToVisual(null).TransformPoint(new Windows.Foundation.Point(0, nameError.ActualHeight)).Y <= w.XamlRootForTest.Size.Height;
            Check("new machine: the refusal shows under the name and is not cut off", nameError is { Text.Length: > 0, IsTextTrimmed: false } && inView && FindOpen<ContentDialog>(w, null) != null, nameError?.Text ?? "no text");
            await Capture(w, dir, "new-machine-error");
            Check("new machine: the refusal says no path or flag", nameError is not null && !nameError.Text.Contains('/') && !nameError.Text.Contains('\\') && !nameError.Text.Contains("--"), nameError?.Text ?? "none");
        }
        else Check("new machine: the dialog opens", false, "no dialog");
        FindOpen<ContentDialog>(w, null)?.Hide();
        await Until(() => FindOpen<ContentDialog>(w, null) == null, 2000);
        // Drop: a disk image on the library opens New machine with it chosen; the core names each kind and its caption.
        var irixDisk = Path.Combine(Environment.GetEnvironmentVariable("ORIGAMI_FIXTURES") ?? "", "disk-irix-6.5.30.img");
        Check("drop: the core tells a CD image from a disk image and words what each does",
            uniffi.origami_ffi.OrigamiFfiMethods.OrigamiClassifyDrop(Path.Combine(dir, "one.iso")) == uniffi.origami_ffi.OrigamiDropKind.CdImage
            && uniffi.origami_ffi.OrigamiFfiMethods.OrigamiClassifyDrop(irixDisk) == uniffi.origami_ffi.OrigamiDropKind.DiskImage
            && uniffi.origami_ffi.OrigamiFfiMethods.OrigamiDropCaption(uniffi.origami_ffi.OrigamiDropKind.CdImage) == "Change CD"
            && uniffi.origami_ffi.OrigamiFfiMethods.OrigamiDropCaption(uniffi.origami_ffi.OrigamiDropKind.DiskImage) == "New machine from this disk");
        _ = w.DropFileAsync(irixDisk, uniffi.origami_ffi.OrigamiDropKind.DiskImage);
        Check("drop: a disk image dropped on the library opens New machine with it chosen",
            await Until(() => FindOpen<TextBlock>(w, "disk.file")?.Text.StartsWith("disk-irix-6.5.30.img") == true, 10_000), FindOpen<TextBlock>(w, "disk.file")?.Text ?? "none");
        FindOpen<ContentDialog>(w, null)?.Hide();
        await Until(() => FindOpen<ContentDialog>(w, null) == null, 2000);
        if (selected != null) w.SelectForTest(selected);

        // --- tabs ---
        var rows = core.Machines().Select(x => x.Key).ToList();
        Check("tabs: the strip is hidden while one machine is open", w.TabCount == 1 && !w.TabBarVisible, $"tabs {w.TabCount}, bar {w.TabBarVisible}");
        var first = w.SelectedKey!;
        var other = rows.First(k => k != first);
        w.OpenInNewTab(other);
        Check("tabs: Open in New Tab adds a tab beside the first machine's and shows the strip", await Until(() => w.TabCount == 2 && w.ActiveTabKey == other && w.TabBarVisible, 2500), $"tabs {w.TabCount}, active {w.ActiveTabKey}");
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
        Check("tabs: closing a tab leaves the other one active and hides the strip again", w.TabCount == 1 && w.ActiveTabKey == first && !w.TabBarVisible, $"tabs {w.TabCount}, active {w.ActiveTabKey}");
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
        Check("rows: the context menu offers Open in New Tab, Show in Explorer and Delete…", w.RowMenuTexts(first2).SequenceEqual(new[] { "Open in new tab", "Show in Explorer", "Delete…" }), string.Join(",", w.RowMenuTexts(first2)));
        w.SelectForTabs(first2); await Until(() => w.SelectedKey == first2, 2000);
        await w.CloseTabAsync(other2, ask: false);
        Check("cards: Memory and Model show on the stopped machine's card, Nodes only with several", w.CardsText.Contains("Memory") && w.CardsText.Contains("Model") && !w.CardsText.Contains("Node") && !w.CardsText.Contains("Topology"), w.CardsText);
        key = w.SelectedKey!;
        // --- the app icon: embedded in the exe, and copied beside it for the window ---
        Check("icon: the exe carries the Origami icon", ExtractIconEx(Environment.ProcessPath!, -1, null, null, 0) > 0, $"{ExtractIconEx(Environment.ProcessPath!, -1, null, null, 0)} icon group(s)");
        Check("icon: Origami.ico sits beside the exe for the window", File.Exists(Path.Combine(AppContext.BaseDirectory, "Origami.ico")));

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
        var tol = 2.0 / Math.Min(disp.HeadWidth, disp.HeadHeight); // a couple of device pixels of rounding, as a factor
        bool Capped() { var (sz, wa) = (w.AppWindow.Size, w.WorkArea); return sz.Width >= wa.W - 2 || sz.Height >= wa.H - 2; }
        // The picture is at t device px per guest px, unless the window hit the work area (smaller) or its minimum size (larger).
        bool ScaleIs(double t)
        {
            var f = disp.LastFactor; var sz = w.AppWindow.Size;
            bool atMin = sz.Width <= Math.Round(MainWindow.MinWidthDips * disp.DeviceScale) + 2 && sz.Height <= Math.Round(MainWindow.MinHeightDips * disp.DeviceScale) + 2;
            return atMin ? f >= t - tol : Capped() ? f <= t + tol : Math.Abs(f - t) <= tol;
        }
        Check("window: inside the work area once the picture has sized it", await Until(() => w.InsideWorkArea, 3000));
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

        // --- picture size: the menu sizes the window once, then the picture fills it ---
        var dip = disp.DeviceScale; // device pixels per DIP
        foreach (var (id, target) in new[] { ("view.actual-size", 1.0), ("view.scale1x", dip), ("view.scale2x", 2 * dip) })
        {
            w.Invoke(id);
            Check($"view: {id} sizes the window so the picture is {target:F2} device px per guest px", await Until(() => ScaleIs(target), 2000), $"factor={disp.LastFactor:F3} window={w.AppWindow.Size.Width}x{w.AppWindow.Size.Height}");
        }
        var sz0 = w.AppWindow.Size;
        var resized = new Windows.Graphics.RectInt32(w.AppWindow.Position.X, w.AppWindow.Position.Y, sz0.Width - 150, sz0.Height - 100);
        w.AppWindow.MoveAndResize(resized);
        await Task.Delay(500);
        var sz1 = w.AppWindow.Size;
        var (rx, ry, rw, rh) = disp.ScreenRect();
        var (pw1, ph1) = disp.PaneSize;
        var padPx = ViewScale.PanePadding * dip;
        Check("view: the app does not undo a resize the user makes", Math.Abs(sz1.Width - resized.Width) <= 2 && Math.Abs(sz1.Height - resized.Height) <= 2, $"{resized.Width}x{resized.Height} -> {sz1.Width}x{sz1.Height}");
        Check("view: after a resize the picture fills the pane, one axis to the padding", Math.Abs(rw - (pw1 * dip - padPx)) <= 2 || Math.Abs(rh - (ph1 * dip - padPx)) <= 2, $"picture {rw}x{rh} pane {pw1 * dip:F0}x{ph1 * dip:F0} px, padding {padPx:F0}");
        w.Invoke("view.fit");
        Check("view: Fit to Window leaves the window as it is", await Until(() => w.StatusForTest.Contains("Fit"), 2000) && w.AppWindow.Size.Width == sz1.Width && w.AppWindow.Size.Height == sz1.Height, w.StatusForTest);

        // --- toolbar: the console opens from its button's command ---
        w.Invoke("window.console");
        Check("console: the toolbar command opens the machine's console window", await Until(() => w.ConsolesOpen == 1, 3000), $"open: {w.ConsolesOpen}");
        w.CloseConsoles(); // the window would sit over the screen and take the clicks below

        // --- toolbar: every button has a tooltip, and pressing one never moves or resizes the window ---
        Check("toolbar: the tab bar shows the selected machine's tab, not an empty strip", w.TabCount >= 1, $"tabs {w.TabCount}");
        Check("sidebar: the heading is Machines", w.LibraryHeadingForTest == Labels.SidebarHeading, w.LibraryHeadingForTest);
        Check("toolbar: every button has a tooltip", w.ToolbarWithoutTipForTest.Length == 0, string.Join(",", w.ToolbarWithoutTipForTest));
        string Frame() => $"{w.AppWindow.Position.X},{w.AppWindow.Position.Y} {w.AppWindow.Size.Width}x{w.AppWindow.Size.Height}";
        var frame0 = Frame();
        foreach (var name in new[] { "CpuButton", "CpuButton", "PointerButton", "PointerButton", "ConsoleButton" })
        {
            w.PressToolbarForTest(name);
            await Task.Delay(400);
            Check($"toolbar: pressing {name} keeps the window frame", Frame() == frame0, $"{frame0} -> {Frame()}");
        }
        w.CloseConsoles();

        // --- the page while the machine runs: the status bar, the prompts, Settings above the picture, Delete ---
        Check("status bar: while it runs it shows the picture's size and the pointer, in the core's words", await Until(() => w.StatusBarTextForTest == "64 × 48|Follows mouse", 4000), w.StatusBarTextForTest);
        Check("status bar: it never shows the machine's state or a note", !w.StatusBarTextForTest.Contains("Running") && !w.StatusBarTextForTest.Contains("display"), w.StatusBarTextForTest);
        await Capture(w, dir, "main-running");
        var runName = core.Machines().First(m => m.Key == key).Name;
        async Task<ContentDialog?> Prompt(string command)
        {
            w.Invoke(command);
            await Until(() => w.OpenDialogForTest != null, 4000);
            return w.OpenDialogForTest;
        }
        async Task Dismiss()
        {
            w.OpenDialogForTest?.Hide();
            await Until(() => w.OpenDialogForTest == null, 4000);
        }
        var offDialog = await Prompt("machine.stop");
        Check("prompts: Power off asks first, in the core's words, and Cancel is the default",
            offDialog is { DefaultButton: ContentDialogButton.Close, CloseButtonText: "Cancel", PrimaryButtonText: "Power off" } && (string)offDialog.Title == $"Power off {runName}?", offDialog?.Title as string ?? "no dialog");
        await Dismiss();
        Check("prompts: cancelling the prompt leaves the machine running", core.Status(key).State == MachineState.Running, core.Status(key).State.ToString());
        var resetDialog = await Prompt("machine.reset");
        Check("prompts: Reset asks first and Cancel is the default", resetDialog is { DefaultButton: ContentDialogButton.Close } && (string)resetDialog.Title == $"Reset {runName}?", resetDialog?.Title as string ?? "no dialog");
        await Dismiss();
        var forceDialog = await Prompt("machine.force-quit");
        Check("prompts: Force quit asks first and Cancel is the default", forceDialog is { DefaultButton: ContentDialogButton.Close } && (string)forceDialog.Title == $"Force quit {runName}?", forceDialog?.Title as string ?? "no dialog");
        await Dismiss();
        Win.RequestClose(w.Hwnd);
        await Until(() => w.OpenDialogForTest != null, 4000);
        var quitDialog = w.OpenDialogForTest;
        Check("prompts: quitting with a machine running asks first, and Keep running is the default",
            quitDialog is { DefaultButton: ContentDialogButton.Primary, PrimaryButtonText: "Keep running", SecondaryButtonText: "Power off", CloseButtonText: "Cancel" } && ((string)quitDialog.Title).StartsWith("Quit Origami while "), quitDialog?.Title as string ?? "no dialog");
        await Dismiss();
        Check("prompts: cancelling the quit prompt keeps the window and the machine", w.Hwnd != IntPtr.Zero && core.Status(key).State == MachineState.Running);
        // Settings is a dialog over the running picture: the picture's own window is put away while it is up, and comes back after.
        var pictureBefore = disp.WindowVisible;
        w.OpenSettingsForTest("drives");
        await Until(() => MachineSettingsDialog.Current is not null, 4000);
        await Task.Delay(300);
        Check("settings: the guest picture is not drawn over Settings while the machine runs", pictureBefore && !disp.WindowVisible, $"before {pictureBefore}, while open {disp.WindowVisible}");
        Check("settings: a running machine's hardware pages say so with a Power off button",
            FindOpen<Button>(w, "settings.locked.poweroff") is { ActualWidth: > 0 } && FindOpen<TextBlock>(w, "settings.locked")?.Text == Ui.T(OrigamiUiString.PowerOffToChangeHardwareSettings), FindOpen<TextBlock>(w, "settings.locked")?.Text ?? "none");
        await Dismiss();
        Check("settings: the picture comes back when Settings closes", await Until(() => disp.WindowVisible, 4000));
        // Delete asks with the core's words; a running machine cannot be deleted. (The core has no recorder for the Recycle Bin in the fake build, so nothing is deleted here.)
        w.Invoke("machine.remove");
        await Task.Delay(300);
        Check("delete: a running machine cannot be deleted", w.OpenDialogForTest == null && core.Machines().Any(m => m.Key == key));
        var stoppedOther = core.Machines().First(m => m.Key != key && m.State == MachineState.Stopped);
        _ = w.RemoveAsync(stoppedOther.Key);
        await Until(() => w.OpenDialogForTest != null, 4000);
        var delDialog = w.OpenDialogForTest;
        Check("delete: a stopped machine asks first, in the core's Windows words, with Cancel as the default",
            delDialog is { DefaultButton: ContentDialogButton.Close, CloseButtonText: "Cancel", PrimaryButtonText: "Delete" } && (string)delDialog.Title == $"Delete {stoppedOther.Name}?" && delDialog.Content is TextBlock { Text: var body } && body.Contains("Recycle Bin"), delDialog?.Title as string ?? "no dialog");
        Check("delete: the alert asked the core's plan first and nothing moved", core.FakeTrashed().Length == 0);
        await Dismiss();
        Check("delete: cancelling keeps the machine", core.Machines().Any(m => m.Key == stoppedOther.Key) && core.FakeTrashed().Length == 0);

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

        // the view shortcuts, as real keys over the screen: the app takes them and the guest never sees the digit
        var viewKeys = new (string Name, string Id, int[] Scans, int Digit, double? Target)[]
        {
            ("Ctrl+Alt+2", "view.scale2x", new[] { 0x1D, 0x38, 0x03 }, 3, 2 * dip),
            ("Ctrl+Alt+1", "view.scale1x", new[] { 0x1D, 0x38, 0x02 }, 2, dip),
            ("Ctrl+0", "view.actual-size", new[] { 0x1D, 0x0B }, 11, 1.0),
            ("Ctrl+9", "view.fit", new[] { 0x1D, 0x0A }, 10, null),
        };
        foreach (var (name, id, scans, digit, target) in viewKeys)
        {
            lock (log) log.Clear();
            foreach (var sc in scans) Win.Scan(sc, false, true);
            foreach (var sc in scans.Reverse()) Win.Scan(sc, false, false);
            var took = await Until(() => w.LastViewCommand == id && (target is not { } t || ScaleIs(t)), 2000);
            await Task.Delay(100);
            bool digitLeaked; lock (log) digitLeaked = log.Any(e => e.Kind == InputKind.Key && e.A == digit);
            Check($"shortcut: {name} over the screen runs its view command and the digit does not reach the guest", took && !digitLeaked, $"status={w.StatusForTest} factor={disp.LastFactor:F3} leaked={digitLeaked}");
        }

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
        Win.Scan(0x02, false, true); Win.Scan(0x02, false, false); // '1' with the chord still held: the view shortcut, not a guest key
        Check("capture: Ctrl+Alt+1 after the release is the view shortcut", await Until(() => w.LastViewCommand == "view.scale1x", 2000), w.LastViewCommand ?? "none");
        w.Invoke("view.fit");
        Win.Scan(0x38, false, false); Win.Scan(0x1D, false, false); Win.Scan(0x1E, false, false);
        await Task.Delay(150);
        guest2.Cancel(); await guest2Task;
        Check("capture: the child window no longer owns the mouse", Win.CaptureOwner() == IntPtr.Zero, $"owner=0x{Win.CaptureOwner().ToInt64():x}");
        Check("capture: the host cursor is shown and free again", !Win.CursorHidden() && !Win.ClipIsSmall(), $"hidden={Win.CursorHidden()} clipped={Win.ClipIsSmall()}");
        var stuck = core.ReleaseAllKeys(key);
        Check("capture: the core holds no key after the chord", stuck == 0, $"released={stuck}");

        // --- screenshot: a PNG in the Pictures folder, named for the machine and the time ---
        var picDir = Path.Combine(dir, "pictures");
        Screenshots.PicturesDir = picDir;
        w.Invoke("machine.screenshot");
        var shotOk = await Until(() => w.LastScreenshot != null && File.Exists(w.LastScreenshot), 8000);
        Check("screenshot: a PNG appears in the Pictures folder", shotOk, w.LastScreenshot ?? w.StatusForTest);
        if (shotOk)
        {
            var png = File.ReadAllBytes(w.LastScreenshot!);
            var sig = png.Length > 24 && png[0] == 0x89 && png[1] == (byte)'P' && png[2] == (byte)'N' && png[3] == (byte)'G';
            int shotW = sig ? png[16] << 24 | png[17] << 16 | png[18] << 8 | png[19] : 0, shotH = sig ? png[20] << 24 | png[21] << 16 | png[22] << 8 | png[23] : 0;
            Check("screenshot: it is a PNG of the head's size", sig && shotW == 64 && shotH == 48, $"{shotW}x{shotH}");
            Check("screenshot: named \"<machine> <date> at <time>.png\"", System.Text.RegularExpressions.Regex.IsMatch(Path.GetFileName(w.LastScreenshot!), @"^.+ \d{4}-\d{2}-\d{2} at \d{2}\.\d{2}\.\d{2}( \d+)?\.png$"), Path.GetFileName(w.LastScreenshot!));
            Check("screenshot: the status bar offers Show in Explorer", w.ShowInExplorerVisible);
        }

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
            Check("diagnostics: the report fills in", await Until(() => diag.ReportText.Contains("Frame updates"), 3000));
            var rt = diag.ReportText;
            Check("diagnostics: the report shows the engine, state, heads and screen counts", rt.Contains("fake") && rt.Contains("Running") && rt.Contains("64 x 48") && rt.Contains("Frame updates"), rt.Replace("\r", "").Replace("\n", " | "));
            diag.Copy();
            string clip = "";
            try { clip = await Windows.ApplicationModel.DataTransfer.Clipboard.GetContent().GetTextAsync(); } catch (Exception e) { clip = "clipboard: " + e.Message; }
            Check("diagnostics: Copy report puts the report on the clipboard", clip == diag.ReportText, clip.Length > 80 ? clip[..80] : clip);
            diag.Close();
        }

        // --- pause and resume ---
        w.Invoke("machine.pause");
        Check("pause: the machine is Paused", await Until(() => core.Status(key).State == MachineState.Paused, 3000), core.Status(key).Reason);
        Check("pause: the pill says Paused", await Until(() => w.PillTextForTest == "Paused", 2000), w.PillTextForTest);
        Check("pause: the toolbar button reads Resume", await Until(() => w.PauseLabelForTest == "Resume", 2000), w.PauseLabelForTest);
        w.FocusLibrary();
        await Task.Delay(300);
        Win.Scan(0x1D, false, true); Win.Scan(0x19, false, true); Win.Scan(0x19, false, false); Win.Scan(0x1D, false, false); // Ctrl+P
        Check("resume: Ctrl+P from XAML focus resumes a paused machine", await Until(() => core.Status(key).State == MachineState.Running, 3000), core.Status(key).State.ToString());
        Check("resume: the toolbar button reads Pause again", await Until(() => w.PauseLabelForTest == "Pause", 2000), w.PauseLabelForTest);

        // --- focus loss lifts a held key ---
        disp.FocusDisplay();
        await Task.Delay(300);
        Win.Scan(0x1E, false, true); // 'a' held
        await Task.Delay(200);
        w.FocusLibrary();
        await Task.Delay(300);
        Win.Scan(0x1E, false, false);
        Check("focus loss: the held key is lifted in the app and in the core", disp.KeysDown == 0 && core.ReleaseAllKeys(key) == 0, $"app holds {disp.KeysDown}");

        // --- the emulated CPU load panel: hidden means no polling; the fake engine reports 30/10 and 60/5 ---
        var polls0 = w.CpuPolls;
        await Task.Delay(1200);
        Check("cpu: hidden, the panel does no polling", !w.CpuPanelShown && w.CpuPolls == polls0, $"shown {w.CpuPanelShown}, polls {polls0} -> {w.CpuPolls}");
        w.SetCpuPanel(true);
        Check("cpu: one bar per CPU, cut into user, sys and spare", await Until(() => w.CpuText.Contains("CPU 0 host time: 30% user, 10% sys, 60% spare") && w.CpuText.Contains("CPU 1 host time: 60% user, 5% sys, 35% spare"), 4000), w.CpuText);
        Check("cpu: the title says the load is measured on the host", w.CpuTitleText == "Emulated CPU load, measured on the host, not the guest", w.CpuTitleText);
        var p1 = w.CpuPolls;
        await Task.Delay(2200);
        Check("cpu: shown, it polls at about 2 Hz", w.CpuPolls - p1 >= 3 && w.CpuPolls - p1 <= 7, $"{w.CpuPolls - p1} polls in 2.2 s");
        w.SetCpuPanel(false);
        await Task.Delay(300);
        var p2 = w.CpuPolls;
        await Task.Delay(1200);
        Check("cpu: hiding it stops the polling", w.CpuPolls == p2, $"polls {p2} -> {w.CpuPolls}");

        // --- stopping while captured and holding a key must leave nothing behind ---
        disp.FocusDisplay();
        (cx, cy, cw, ch) = disp.ScreenRect();
        Win.MouseAbs(cx + cw / 2, cy + ch / 2);
        await Task.Delay(100);
        Win.MouseButton(true); Win.MouseButton(false);
        Check("detach while captured: captured first", await Until(() => disp.Captured, 2000));
        Win.Scan(0x1E, false, true);
        await Task.Delay(150);
        w.Invoke("machine.stop", true);
        Check("detach while captured: the display detaches", await Until(() => !disp.Attached, 8000));
        Win.Scan(0x1E, false, false);
        Check("detach while captured: cursor shown, unclipped, mouse released, no key held",
            !Win.CursorHidden() && !Win.ClipIsSmall() && Win.CaptureOwner() == IntPtr.Zero && disp.KeysDown == 0,
            $"hidden={Win.CursorHidden()} clipped={Win.ClipIsSmall()} owner=0x{Win.CaptureOwner().ToInt64():x} keys={disp.KeysDown}");

        // --- About, and the starting screen's live cube ---
        IdentityChecks.OutDir = dir;
        foreach (var (name, ok, detail) in await IdentityChecks.About(w, Until)) Check(name, ok, detail);
        foreach (var (name, ok, detail) in await IdentityChecks.Credits(w, Until)) Check(name, ok, detail);
        w.PreviewStarting();
        var cubeStart = w.StartCubeForTest;
        Check("starting: the cube attached", await Until(() => cubeStart.Attached, 10_000), cubeStart.LastError ?? $"status {cubeStart.Status}");
        await Task.Delay(800);
        var s1 = cubeStart.Readback(out var sw1, out var sh1);
        var turns = s1 != null && await IdentityChecks.Turns(cubeStart, s1);
        Check("starting: the cube draws and turns by itself", s1 != null && turns && IdentityChecks.Colours(s1) > 50, s1 == null ? cubeStart.LastError ?? "no readback" : $"{sw1}x{sh1}, {IdentityChecks.Colours(s1)} colours, moving={cubeStart.Moving}");
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
        Check("stop: Power off and Force quit on an ended machine do not fail", again == "ok", again);
        Check("quit: no machine is up, so quitting would not prompt", w.UpNames.Count == 0, string.Join(",", w.UpNames));
        // A machine made for the purpose, once everything that needs the others has run.
        var doomed = await core.CreateMachineAsync("Delete me", offers[core.DefaultOffering(offers)].Id);
        w.Reload();
        var doomedName = "Delete me";
        await w.RemoveAsync(doomed, confirmed: true);
        Check("delete: confirming moves the folder to the core's recorded trash and says so in a toast",
            core.FakeTrashed().Length == 1 && core.FakeTrashed()[0].Contains(doomedName) && core.Machines().All(m => m.Key != doomed) && w.StatusForTest == $"{doomedName} moved to the Recycle Bin",
            string.Join("|", core.FakeTrashed()) + " / " + w.StatusForTest);

        // --- a machine with no display opens to its console ---
        var blind = await core.CreateMachineAsync("Serial only (fake)", "fakeblind.1");
        w.Reload();
        w.SelectForTest(blind);
        await Until(() => w.SelectedKey == blind, 2000);
        w.Invoke("machine.start");
        Check("headless: the machine starts", await Until(() => core.Status(blind).State == MachineState.Running, 15_000), core.Status(blind).Reason);
        Check("headless: the state says Running, not a word of its own", await Until(() => w.PillTextForTest == "Running", 3000), w.PillTextForTest);
        Check("headless: the console is the machine's view, with no display attached", await Until(() => w.ConsolePaneForTest != null, 3000) && !w.DisplayViewForTest.Attached && !w.DisplayVisibleForTest);
        if (w.ConsolePaneForTest is { } pane)
        {
            core.FakeSay(blind, "serial only\r\n");
            Check("headless: guest output reaches the pane", await Until(() => pane.TextForTest.Contains("serial only"), 3000), pane.TextForTest.Replace("\n", " | "));
        }
        w.Invoke("machine.stop", true);
        Check("headless: stopping closes the pane", await Until(() => core.Status(blind).State == MachineState.Stopped && w.ConsolePaneForTest == null, 8000), core.Status(blind).Reason);

        // --- the stopped machine's pane is tinted with its chassis colour, fading toward the bottom ---
        Check("tint: the summary pane is tinted, strongest at the top", await Until(() => w.TintForTest is not null && w.TintTopAlphaForTest == PaneTint.TopAlpha && w.TintBottomAlphaForTest == PaneTint.BottomAlpha, 3000),
            $"tint {w.TintForTest}, alpha {w.TintTopAlphaForTest} -> {w.TintBottomAlphaForTest}");

        // --- nothing chosen: the live cube turns under "Choose a machine" ---
        w.ClearSelectionForTest();
        var cubeEmpty = w.EmptyCubeForTest;
        Check("empty: the view asks to choose a machine", await Until(() => w.EmptyTitleForTest == "Choose a machine", 3000), w.EmptyTitleForTest);
        Check("empty: the cube attached", await Until(() => cubeEmpty.Attached, 10_000), cubeEmpty.LastError ?? $"status {cubeEmpty.Status}");
        await Task.Delay(800);
        var e1 = cubeEmpty.Readback(out var ew, out var eh);
        var eTurns = e1 != null && await IdentityChecks.Turns(cubeEmpty, e1);
        Check("empty: the cube draws and turns by itself", e1 != null && eTurns && IdentityChecks.Colours(e1) > 50, e1 == null ? cubeEmpty.LastError ?? "no readback" : $"{ew}x{eh}, {IdentityChecks.Colours(e1)} colours");
        if (e1 != null) await Shot.Readback(e1, ew, eh, ew * 4, Path.Combine(dir, "empty-cube.png"));
        w.SelectForTest(key);
        Check("empty: the cube is detached once a machine is chosen", await Until(() => !cubeEmpty.Attached, 3000));

        // --- saving another library folder re-lists at once, with no restart ---
        var home = core.LibraryRoot;
        var before = core.Machines().Count;
        var elsewhere = Path.Combine(dir, "elsewhere-library");
        w.ApplySettings(new AppSettings { LibraryRoot = elsewhere });
        Check("library folder: the list switches to the new, empty folder", await Until(() => w.LibraryCount == 0 && core.LibraryRoot == elsewhere, 5000), $"count {w.LibraryCount}");
        // The machine that was selected is gone from this folder: refreshing must neither throw nor keep a tab for it.
        Check("library folder: a selected machine that left the list leaves no tab and no crash", await Until(() => w.TabCount == 0 && w.SelectedKey is null, 5000), $"tabs {w.TabCount}, selected {w.SelectedKey}");
        await core.CreateMachineAsync("Other folder", core.Catalogue()[0].Id);
        Check("library folder: a machine made there is listed", await Until(() => w.LibraryCount == 1, 5000), $"count {w.LibraryCount}");
        w.ApplySettings(new AppSettings { LibraryRoot = home });
        Check("library folder: the old folder's machines are back", await Until(() => w.LibraryCount == before, 5000), $"count {w.LibraryCount}, want {before}");

        // --- a 1280x1024 picture on the core view: exact sizes from first appearance and the View menu ---
        var fallback0 = disp.ForceFallback;
        disp.ForceFallback = false;
        var big = await core.CreateMachineAsync("Big picture (fake)", "fakebig.1");
        w.Reload();
        w.SelectForTest(big);
        await Until(() => w.SelectedKey == big, 2000);
        w.Invoke("machine.start");
        var bigGuest = StartGuest(core, big, out var bigGuestTask);
        var bigUp = await Until(() => disp.Attached && disp.HeadWidth == 1280 && disp.HeadHeight == 1024, 15_000);
        Check("big picture: the 1280x1024 head attaches", bigUp, $"{disp.HeadWidth}x{disp.HeadHeight} {disp.LastError}");
        Note("renderer (big picture)", $"{disp.Renderer} (core view status {disp.CoreViewStatus})");
        Check("big picture: the core view draws it, not the fallback", disp.Renderer == "core view", disp.Renderer ?? "");
        if (bigUp)
        {
            await Task.Delay(500);
            await Until(() => w.LayoutCurrentForTest, 5000);
            var bdip = disp.DeviceScale;
            var bpad = ViewScale.PanePadding * bdip;
            var bwa = w.WorkArea;
            PixelRect WinRect() => new(w.AppWindow.Position.X, w.AppWindow.Position.Y, w.AppWindow.Size.Width, w.AppWindow.Size.Height);
            string Where() => $"window {WinRect().W}x{WinRect().H}, work area {bwa.W}x{bwa.H}, device px per DIP {bdip:F2}";
            var (bpw, bph) = disp.PaneSize;
            var (_, _, brw, brh) = disp.ScreenRect();
            var bwin = WinRect();
            // Whether 1 DIP per guest pixel fits: this window's chrome plus the picture and padding.
            bool oneFits = 1280 * bdip + bpad + (bwin.W - bpw * bdip) <= bwa.W + 2 && 1024 * bdip + bpad + (bwin.H - bph * bdip) <= bwa.H + 2;
            if (oneFits)
                Check("big picture: first window shows 1 DIP per guest pixel (it fits)", Math.Abs(disp.LastFactor - bdip) * 1280 <= 2, $"factor {disp.LastFactor:F4}, want {bdip:F4}; {Where()}");
            else
            {
                bool touches = bwin.W >= bwa.W - 2 || bwin.H >= bwa.H - 2;
                Check("big picture: first window is the largest fit (1 DIP does not fit): it touches the work area on one axis", touches, Where());
                Check("big picture: first window's picture fills the pane on one axis, aspect 1.25", (Math.Abs(brw - (bpw * bdip - bpad)) <= 2 || Math.Abs(brh - (bph * bdip - bpad)) <= 2) && Math.Abs(brw / (double)brh - 1.25) < 0.01,
                    $"picture {brw}x{brh}, pane {bpw * bdip:F0}x{bph * bdip:F0} px, padding {bpad:F0}");
            }
            foreach (var (id, dpg) in new[] { ("view.actual-size", 1 / bdip), ("view.scale1x", 1.0), ("view.scale2x", 2.0) })
            {
                var wb = WinRect();
                var (p0w, p0h) = disp.PaneSize;
                var exp = ViewScale.WindowFor(wb, bwa, bdip, p0w, p0h, 1280, 1024, MainWindow.MinWidthDips, MainWindow.MinHeightDips, dpg);
                var expF = ViewScale.Factor(1280, 1024, p0w * bdip + (exp.W - wb.W) - bpad, p0h * bdip + (exp.H - wb.H) - bpad);
                var wantF = dpg * bdip;
                var kase = Math.Abs(expF - wantF) * 1280 <= 2 ? "fits" : "capped to the work area";
                w.Invoke(id);
                // The picture follows the window a layout pass later: wait for the factor too, not a fixed delay.
                await Until(() => Math.Abs(WinRect().W - exp.W) <= 2 && Math.Abs(WinRect().H - exp.H) <= 2 && Math.Abs(disp.LastFactor - expF) * 1280 <= 2, 5000);
                Check($"big picture: {id} ({kase}) gives the expected window and factor", Math.Abs(WinRect().W - exp.W) <= 2 && Math.Abs(WinRect().H - exp.H) <= 2 && Math.Abs(disp.LastFactor - expF) * 1280 <= 2,
                    $"factor {disp.LastFactor:F4}, want {expF:F4} (uncapped {wantF:F4}); window {WinRect().W}x{WinRect().H}, want {exp.W}x{exp.H}; work area {bwa.W}x{bwa.H}");
            }
        }
        if (bigUp) await BehaviourSelftest.Run(w, core, disp, big, core.Machines().First(m => m.Key != big).Key, Check);
        bigGuest.Cancel(); await bigGuestTask;
        w.Invoke("machine.stop", true);
        await Until(() => core.Status(big).State == MachineState.Stopped, 8000);
        disp.ForceFallback = fallback0;

        // --- an older machine (the format 4 fixture) is listed as needing an upgrade, cannot start, and is upgraded from its card ---
        var fixture = Path.Combine(Environment.GetEnvironmentVariable("ORIGAMI_FIXTURES") ?? "", "format4-origin200.toml");
        Check("upgrade: the format 4 fixture is found", File.Exists(fixture), fixture);
        var olderDir = Path.Combine(core.LibraryRoot, "Older");
        Directory.CreateDirectory(olderDir);
        if (File.Exists(fixture)) File.Copy(fixture, Path.Combine(olderDir, "machine.toml"), true);
        // A real older machine has its state files; the fake engine wants these two to start it.
        Directory.CreateDirectory(Path.Combine(olderDir, "state"));
        foreach (var f in new[] { "nvram0", "flash0" }) File.WriteAllBytes(Path.Combine(olderDir, "state", f + ".raw"), new byte[16]);
        core.Relist();
        Check("upgrade: the library lists it as needing an upgrade", await Until(() => core.Machines().Any(m => m.Name == "Older" && m.State == MachineState.NeedsUpgrade), 6000));
        var older = core.Machines().FirstOrDefault(m => m.Name == "Older")?.Key ?? "";
        w.Reload();
        w.SelectForTest(older);
        Check("upgrade: its card shows with the core's title, Start is off and the state says Needs upgrade",
            await Until(() => w.UpgradeCardForTest && w.PillTextForTest == "Needs upgrade" && w.CardTitleForTest == "Made by an older Origami" && !core.CommandEnabled("machine.start", core.Status(older).State), 3000), w.PillTextForTest + " | " + w.CardTitleForTest);
        await Capture(w, dir, "card-needs-upgrade");
        w.PressCardForTest(CardActionKind.Upgrade);
        Check("upgrade: the machine replaces the card and a toast says it was upgraded",
            await Until(() => !w.UpgradeCardForTest && core.Status(older).State == MachineState.Stopped && w.StatusForTest == "Older upgraded", 15_000),
            $"{w.PillTextForTest} | {w.UpgradeErrorForTest} | {w.StatusForTest}");
        w.Invoke("machine.start");
        Check("upgrade: the upgraded machine starts", await Until(() => core.Status(older).State == MachineState.Running, 15_000), core.Status(older).Reason);
        w.Invoke("machine.stop", true);
        await Until(() => core.Status(older).State == MachineState.Stopped, 8000);
    }
}
#endif
