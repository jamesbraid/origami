#if !ORIGAMI_FAKE
using System.Text.Json;
using Origami.Logic;
using uniffi.origami_ffi;
using UI = Microsoft.UI.Xaml;
using UIC = Microsoft.UI.Xaml.Controls;
using UIM = Microsoft.UI.Xaml.Media;
using OStep = uniffi.origami_ffi.OrigamiInstallStep;

namespace Origami.App;

/// <summary>
/// `Origami.exe --selftest OUT.json --library DIR` against the core's QEMU engine: starts the library's machine, and checks that the PROM's picture reaches the screen and equals
/// QEMU's own screendump. Exits with the number of failed steps.
/// </summary>
public static class Selftest
{
    sealed record Step(string Name, bool Ok, string Detail);
    static readonly List<Step> Steps = new();
    static string _out = "";
    static bool _done;

    static void Add(string name, bool ok, string detail = "")
    {
        Steps.Add(new Step(name, ok, detail));
        try { File.AppendAllText(_out + ".partial", (ok ? "ok   " : "FAIL ") + name + " | " + detail + Environment.NewLine); } catch { }
    }

    public static void Fail(string why)
    {
        if (_out == "" || _done) return;
        Add("fatal", false, why);
        Finish();
    }

    static void Finish()
    {
        _done = true;
        var failed = Steps.Count(s => !s.Ok);
        File.WriteAllText(_out, JsonSerializer.Serialize(new { failed, steps = Steps }, new JsonSerializerOptions { WriteIndented = true }));
        Environment.Exit(failed);
    }

    static async Task<bool> Until(Func<bool> cond, int ms)
    {
        var t = Environment.TickCount64 + ms;
        while (Environment.TickCount64 < t) { if (cond()) return true; await Task.Delay(50); }
        return cond();
    }

    static async Task Grab(string dir, string name, IntPtr hwnd)
    {
        // Only the capture workflow answers these requests; a release self-test has no one to ask.
        if (Environment.GetEnvironmentVariable("ORIGAMI_CAPTURES") != "1") return;
        // An inactive window draws its title bar greyed: bring the target to the front first and record whether the OS allowed it.
        Win.Foreground(hwnd);
        var front = await Until(() => Win.IsForeground(hwnd), 3000);
        Add("foreground for " + name, true, front ? "foreground" : $"not foreground (fg=0x{Win.ForegroundHandle().ToInt64():x}, target=0x{hwnd.ToInt64():x})");
        File.WriteAllText(Path.Combine(dir, $"want-{name}.txt"), hwnd.ToInt64().ToString());
        Add("capture " + name, await Until(() => File.Exists(Path.Combine(dir, $"got-{name}.txt")), 20_000));
    }

    /// <summary>The pictures docs/app.md shows of the New Machine sheet, the Network settings and the Install IRIX assistant, taken with the machine stopped.</summary>
    static async Task Scenes(MainWindow w, UniffiCoreClient core, string key, string dir)
    {
        if (Environment.GetEnvironmentVariable("ORIGAMI_CAPTURES") != "1") return;
        // A ContentDialog lives in a popup, so search the open popups; an id of null matches the first of the type.
        T? FindOpen<T>(string? id) where T : UI.FrameworkElement
        {
            T? In(UI.DependencyObject o)
            {
                if (o is T t && (id == null || UI.Automation.AutomationProperties.GetAutomationId(t) == id)) return t;
                for (var i = 0; i < UIM.VisualTreeHelper.GetChildrenCount(o); i++) if (In(UIM.VisualTreeHelper.GetChild(o, i)) is { } hit) return hit;
                return null;
            }
            foreach (var p in UIM.VisualTreeHelper.GetOpenPopupsForXamlRoot(w.XamlRootForTest)) if (p.Child is { } c && In(c) is { } found) return found;
            return null;
        }
        async Task CloseDialog()
        {
            FindOpen<UIC.ContentDialog>(null)?.Hide();
            await Until(() => FindOpen<UIC.ContentDialog>(null) == null, 3000);
        }
        List<T> Find<T>() where T : UI.FrameworkElement
        {
            var all = new List<T>();
            void In(UI.DependencyObject o)
            {
                if (o is T t) all.Add(t);
                for (var i = 0; i < UIM.VisualTreeHelper.GetChildrenCount(o); i++) In(UIM.VisualTreeHelper.GetChild(o, i));
            }
            foreach (var p in UIM.VisualTreeHelper.GetOpenPopupsForXamlRoot(w.XamlRootForTest)) if (p.Child is { } c) In(c);
            return all;
        }
        // The names the other apps must match: tools/screenshots parity compares this file.
        var parity = new Dictionary<string, object> { ["platform"] = "windows", ["sidebar_heading"] = w.LibraryHeadingForTest };
        parity["about"] = new[] { Ui.T(OrigamiUiString.AboutOrigami), OrigamiFfiMethods.OrigamiVersionLine(typeof(AboutWindow).Assembly.GetName().Version?.ToString(3) ?? "0") };
        parity["dialog_titles"] = new[] { Ui.T(OrigamiUiString.NewMachine), Ui.T(OrigamiUiString.Settings), Ui.T(OrigamiUiString.Diagnostics), Ui.T(OrigamiUiString.InstallIrix) };
        parity["toolbar_tooltips"] = new[] { Ui.T(OrigamiUiString.ShowOrHideTheEmulated), Ui.T(OrigamiUiString.OpenTheMainMenu) };
        void SaveParity() { try { File.WriteAllText(Path.Combine(dir, "windows-parity.json"), JsonSerializer.Serialize(parity, new JsonSerializerOptions { WriteIndented = true })); } catch { } }
        void Press(UIC.Button? b) { if (b != null) new UI.Automation.Peers.ButtonAutomationPeer(b).Invoke(); }
        try
        {
            // New Machine with the ready-made choice: the list comes from origami-dist, so the choice appears once it answers.
            for (var tries = 0; tries < 4 && FindOpen<UIC.RadioButtons>("new.disk") == null; tries++)
            {
                w.Invoke("machine.new");
                await Until(() => FindOpen<UIC.RadioButtons>("new.disk") != null, 2000);
            }
            var plain = OrigamiFfiMethods.OrigamiDiskOptions(false).Length;
            if (await Until(() => FindOpen<UIC.RadioButtons>("new.disk") is { } d && d.Items.Count > plain, 30_000))
            {
                // The ready-made choice is listed first, so it is already the one selected.
                if (FindOpen<UIC.TextBox>("new.name") is { } typed) typed.Text = "irix";
                await Task.Delay(1500);
                parity["new_machine_fields"] = new[] { "new.engine", "new.model", "new.name" }
                    .Select(id => FindOpen<UI.FrameworkElement>(id)).Select(e => e switch { UIC.ComboBox c => (string)c.Header, UIC.TextBox t => (string)t.Header, _ => "" })
                    .Append(Find<UIC.Expander>().Select(e => e.Header as string ?? "").FirstOrDefault() ?? "").ToList();
                await Grab(dir, "windows-ready-disk", w.Hwnd);
            }
            else Add("windows-ready-disk.png", false, "origami-dist listed no ready-made disk");
            await CloseDialog();

            // Settings at Network, with Bridged chosen and its fields open.
            for (var tries = 0; tries < 4 && FindOpen<UIC.RadioButtons>("settings.network") == null; tries++)
            {
                w.OpenSettingsForTest("network");
                await Until(() => FindOpen<UIC.RadioButtons>("settings.network") != null, 3000);
            }
            var bridgedAt = core.NetworkModes().ToList().FindIndex(c => c.Label == "Bridged");
            if (FindOpen<UIC.RadioButtons>("settings.network") is { } mode && bridgedAt >= 0)
            {
                mode.SelectedIndex = bridgedAt;
                await Until(() => FindOpen<UIC.TextBox>("settings.bridge.device") != null, 3000);
                await Task.Delay(1500);
                parity["settings_tabs"] = MachineSettingsDialog.Pages().Select(p => p.Title).ToList();
                await Grab(dir, "windows-network", w.Hwnd);
            }
            else Add("windows-network.png", false, $"network modes found: {FindOpen<UIC.RadioButtons>("settings.network") != null}, Bridged at {bridgedAt}");
            await CloseDialog();

            // The Install IRIX assistant at its Software step; its window is separate, so it comes to the front of the main one.
            InstallWindow? iw = null;
            for (var tries = 0; tries < 4 && iw == null; tries++)
            {
                w.Invoke("machine.install-irix");
                await Until(() => (iw = w.InstallForTest(key)) != null, 2000);
            }
            if (iw == null) { Add("windows-install-assistant.png", false, "the assistant did not open"); return; }
            async Task Go(OStep next)
            {
                await Until(() => iw.Find<UIC.Button>("install.continue") is { IsEnabled: true }, 8000);
                Press(iw.Find<UIC.Button>("install.continue"));
                await Until(() => iw.StepForTest == next, 8000);
            }
            await Until(() => iw.StepForTest == OStep.Release, 8000);
            await Go(OStep.Media);
            await Go(OStep.Software);
            await Until(() => iw.Find<UIC.CheckBox>("install.addon.mipspro") != null, 8000);
            parity["install_steps"] = iw.StepNamesForTest;
            var main = w.AppWindow.Presenter as Microsoft.UI.Windowing.OverlappedPresenter;
            var wasTop = main?.IsAlwaysOnTop ?? false;
            if (main != null) main.IsAlwaysOnTop = false;
            iw.Activate();
            if (iw.AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter ip) ip.IsAlwaysOnTop = true;
            await Task.Delay(2000);
            await Grab(dir, "windows-install-assistant", WinRT.Interop.WindowNative.GetWindowHandle(iw));
            await iw.StopAndCloseAsync();
            if (main != null) main.IsAlwaysOnTop = wasTop;
        }
        catch (Exception e) { Add("pictures of the dialogs", false, e.ToString()); }
        finally { SaveParity(); }
    }

    public static async Task RunAsync(MainWindow w, UniffiCoreClient core, string outFile)
    {
        _out = outFile;
        var dir = Path.GetDirectoryName(Path.GetFullPath(outFile))!;
        _ = Task.Run(async () => { await Task.Delay(420_000); w.DispatcherQueue.TryEnqueue(() => Fail("watchdog: 420 s")); });
        try { await Scenario(w, core, dir); }
        catch (Exception e) { Add("exception", false, e.ToString()); }
        // A machine outlives the app by design: do not leave one running behind a failed test.
        foreach (var m in core.Machines().Where(m => m.State is MachineState.LeftRunning or MachineState.Running or MachineState.Starting or MachineState.Paused))
            try { await core.ForceQuitAsync(m.Key); } catch { }
        Finish();
    }

    static async Task Scenario(MainWindow w, UniffiCoreClient core, string dir)
    {
        await Task.Delay(500);
        if (w.AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter op) op.IsAlwaysOnTop = true;
        w.Activate();
        Win.Foreground(w.Hwnd);
        await Task.Delay(500);
        Add("an engine is linked", core.EngineIds.Length > 0, core.EngineIds);
        Add("window is the foreground window", Win.IsForeground(w.Hwnd));

        // With an empty library, create the machine through the FFI (the product's graphics default).
        string key;
        if (w.LibraryCount == 0)
        {
            var product = App.ArgValue("--create-product") ?? "octane2";
            try { key = await Task.Run(() => core.CreateFrom(App.ArgValue("--machine-name") ?? "selftest-" + product, product, App.ArgValue("--graphics"))); }
            catch (Exception e) { Add("create a machine (firmware download)", false, e.Message); return; }
            Add("create a machine (firmware download)", key.Length > 0, key);
            w.Reload();
            await Until(() => w.LibraryCount == 1, 5000);
        }
        key = w.SelectedKey ?? "";
        Add("library has a machine to start", key.Length > 0, key);

        w.Invoke("machine.start");
        Add("start reaches Running", await Until(() => core.Status(key).State == MachineState.Running, 180_000), core.Status(key).Reason + " " + core.Status(key).LastError);
        var disp = w.DisplayViewForTest;
        var attached = await Until(() => disp.Attached, 120_000);
        var why = disp.LastError ?? "";
        if (!attached)
        {
            // What the core itself says about the display: the status reason, its last error, and each head.
            var st = core.Status(key);
            var heads = core.Heads(key);
            why += $" | core: state={st.State} reason='{st.Reason}' lastError='{st.LastError}' heads=[{heads}]";
        }
        Add("display attached", attached, why);
        if (!attached) return;
        Add("renderer", true, $"{disp.Renderer} (core view status {disp.CoreViewStatus}; {disp.LastError})");
        Add("frames present", await Until(() => disp.Presents >= 1, 120_000), $"wakes={disp.Wakes} presents={disp.Presents}");

        // Let the PROM draw, then read what the core says about how the picture got to the GPU.
        await Task.Delay(8000);
        if (disp.CoreFrameCounters() is { } fc)
            Add("core display counters", true, $"counted={fc.Counted} frames={fc.Frames} lines={fc.Lines} imported_lines={fc.ImportedLines} cpu_copied_bytes={fc.CopiedBytes}");
        if (disp.CoreViewCounters() is { } vc)
            Add("core view counters", vc.Presents > 0, $"presents={vc.Presents} take={vc.TakeNs / 1000} us draw={vc.DrawNs / 1000} us present={vc.PresentNs / 1000} us (totals)");
        Add("app counters", true, $"wakes={disp.Wakes} presents={disp.Presents}");

        // QEMU's own screendump (a QMP call), as the reference picture; the core's view has no readback hook.
        string? dumpError = null; int lit = 0, hw = 0, hh = 0;
        for (int attempt = 0; attempt < 3 && lit == 0; attempt++)
        {
            try
            {
                var ppm = Path.Combine(dir, "qemu-screendump.ppm");
                core.Screendump(key, 0, ppm);
                var (pw, ph, engine) = Shot.ReadPpm(ppm);
                hw = pw; hh = ph;
                for (int i = 0; i < pw * ph; i++) if (engine[i * 4] != 0 || engine[i * 4 + 1] != 0 || engine[i * 4 + 2] != 0) lit++;
                await Shot.Readback(engine, pw, ph, pw * 4, Path.Combine(dir, "picture.png"));
            }
            catch (Exception e) { dumpError = e.Message; }
        }
        // Exact compare: the core's readback of what its view drew against QEMU's screendump. The machine is paused
        // first, so the PROM cannot draw between the two calls and the two pictures must be identical.
        {
            var ppm = Path.Combine(dir, "qemu-screendump.ppm");
            string result = ""; bool same = false; byte[]? shot = null; int cw = 0, ch = 0;
            try
            {
                await core.ControlAsync(key, MachineControl.Pause);
                if (!await Until(() => core.Status(key).State == MachineState.Paused, 30_000)) result = "the machine did not pause";
                else
                {
                    var rbk = disp.CoreReadback(out cw, out ch, headSize: true);
                    core.Screendump(key, 0, ppm);
                    var (pw, ph, engine) = Shot.ReadPpm(ppm);
                    if (rbk == null) result = "no readback: " + disp.LastError;
                    else if (cw != pw || ch != ph) result = $"size {cw}x{ch} vs {pw}x{ph}";
                    else
                    {
                        int bad = 0;
                        for (int i = 0; i < pw * ph; i++)
                        {
                            int o = i * 4;
                            if (rbk[o] != engine[o] || rbk[o + 1] != engine[o + 1] || rbk[o + 2] != engine[o + 2]) bad++;
                        }
                        same = bad == 0; shot = rbk;
                        result = $"{bad} mismatching pixels of {pw * ph} ({pw}x{ph}), machine paused";
                    }
                }
            }
            catch (Exception e) { result = e.GetType().Name + ": " + e.Message; }
            finally { try { await core.ControlAsync(key, MachineControl.Resume); } catch { } }
            Add("first-picture: the core's readback equals QEMU's screendump", same, result);
            if (shot != null) await Shot.Readback(shot, cw, ch, cw * 4, Path.Combine(dir, "core-readback.png"));
        }
        Add("QEMU screendump shows a picture", lit > 100, dumpError is null ? $"{lit} lit pixels of {hw * hh} ({hw}x{hh})" : "screendump failed: " + dumpError);
        if (disp.Verify(out _) is { } rb) Add("fallback readback taken", true, $"{rb.Length} bytes");
        try { await Shot.Xaml((Microsoft.UI.Xaml.UIElement)w.Content, Path.Combine(dir, "xaml.png")); } catch { }

        // The workflow's window capture of the running app follows: an inactive window draws its title bar greyed.
        Win.Foreground(w.Hwnd);
        var front = await Until(() => Win.IsForeground(w.Hwnd), 3000);
        Add("foreground for window", true, front ? "foreground" : $"not foreground (fg=0x{Win.ForegroundHandle().ToInt64():x})");
        File.WriteAllText(Path.Combine(dir, "ready.txt"), "ready");
        await Until(() => File.Exists(Path.Combine(dir, "done.txt")), 15_000);

        Add("library: the machine's still is shown", w.LibraryStills == 1, $"stills in the library: {w.LibraryStills}");
        Add("library: the chassis colour is the measured one (octane2 #2e354e)", w.ChassisHex(key) == "#2e354e", w.ChassisHex(key));
        IdentityChecks.OutDir = dir;
        foreach (var (name, ok, detail) in await IdentityChecks.About(w, Until)) Add(name, ok, detail);
        // Each sheet must be in front of the main window, which is always on top for input, to be photographed.
        var main = w.AppWindow.Presenter as Microsoft.UI.Windowing.OverlappedPresenter;
        var wasTop = main?.IsAlwaysOnTop ?? false;
        foreach (var (name, ok, detail) in await IdentityChecks.Credits(w, Until, async (shot, sheet) =>
        {
            if (main != null) main.IsAlwaysOnTop = false;
            sheet.Activate();
            if (sheet.AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter sp) sp.IsAlwaysOnTop = true;
            await Task.Delay(2000);
            await Grab(dir, shot, WinRT.Interop.WindowNative.GetWindowHandle(sheet));
        })) Add(name, ok, detail);
        if (main != null) main.IsAlwaysOnTop = wasTop;
        // The pictures docs/app.md shows: the workflow's window capture, asked for by file (PrintWindow sees the machine's own picture).
        w.SetCpuPanel(true);
        await Until(() => w.CpuText.Contains("CPU 0 host time"), 10_000);
        await Task.Delay(1500);
        await Grab(dir, "windows-cpu-panel", w.Hwnd);
        w.SetCpuPanel(false);
        w.Invoke("window.diagnostics");
        if (await Until(() => w.DiagnosticsForTest(key) != null, 5000) && w.DiagnosticsForTest(key) is { } diag)
        {
            await Until(() => diag.ReportText.Contains("Frame updates"), 5000);
            // The main window is always on top for input; Diagnostics must be in front of it to be photographed.
            if (w.AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter top) top.IsAlwaysOnTop = false;
            diag.Activate();
            if (diag.AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter dp) dp.IsAlwaysOnTop = true;
            await Task.Delay(2000);
            await Grab(dir, "windows-diagnostics", WinRT.Interop.WindowNative.GetWindowHandle(diag));
            diag.Close();
        }
        else Add("windows-diagnostics.png", false, "the Diagnostics window did not open");
        // Pause and resume the real machine; the picture keeps its head.
        // Call the core directly first, so a QMP failure shows its own error and timing (the status bar hides it).
        {
            var sw = System.Diagnostics.Stopwatch.StartNew();
            string res;
            try { await core.ControlAsync(key, MachineControl.Pause); res = "ok"; } catch (Exception e) { res = e.GetType().Name + ": " + e.Message; }
            Add("pause: the core call returns without error", res == "ok", $"{res} after {sw.ElapsedMilliseconds} ms; state now {core.Status(key).State}");
            if (res == "ok")
            {
                string back;
                try { await core.ControlAsync(key, MachineControl.Resume); back = "ok"; } catch (Exception e) { back = e.GetType().Name + ": " + e.Message; }
                Add("resume: the core call returns without error", back == "ok", back);
            }
        }
        await Until(() => core.Status(key).State == MachineState.Running, 30_000); // Invoke toggles on the cached state
        w.Invoke("machine.pause");
        Add("pause: the machine is Paused", await Until(() => core.Status(key).State == MachineState.Paused, 30_000), core.Status(key).Reason + " | status bar: " + w.StatusForTest + " | enabled: " + core.CommandEnabled("machine.pause", core.Status(key).State));
        if (core.Status(key).State == MachineState.Paused)
        {
            w.Invoke("machine.pause");
            Add("resume: the same command resumes", await Until(() => core.Status(key).State == MachineState.Running, 30_000), core.Status(key).State.ToString());
        }
        else Add("resume: the same command resumes", false, "not tested: the machine never paused");
        // Quit with a machine up must ask, not close the window; hiding the prompt cancels.
        {
            UIC.ContentDialog? In(UI.DependencyObject o)
            {
                if (o is UIC.ContentDialog d) return d;
                for (var i = 0; i < UIM.VisualTreeHelper.GetChildrenCount(o); i++) if (In(UIM.VisualTreeHelper.GetChild(o, i)) is { } hit) return hit;
                return null;
            }
            UIC.ContentDialog? Asked()
            {
                if (w.XamlRootForTest is not { } root) return null;
                foreach (var p in UIM.VisualTreeHelper.GetOpenPopupsForXamlRoot(root)) if (p.Child is { } c && In(c) is { } found) return found;
                return null;
            }
            w.Invoke("app.quit");
            var asked = await Until(() => w.XamlRootForTest is null || Asked() != null, 10_000) && w.XamlRootForTest is not null;
            Add("quit with a machine running asks first", asked, asked ? "prompt open" : "the window closed without asking");
            if (asked) { Asked()!.Hide(); await Until(() => Asked() == null, 3000); }
        }
        // A clean stop waits for the guest (and a PROM menu never answers); force-quit ends QEMU at once.
        w.Invoke("machine.force-quit", true);
        Add("force quit settles as Stopped, not stopped unexpectedly", await Until(() => core.Status(key).State == MachineState.Stopped, 60_000), core.Status(key).State + " " + core.Status(key).Reason);
        await Scenes(w, core, key, dir);
        // A runner that dies without a word must not leave the machine Running.
        w.Invoke("machine.start");
        if (await Until(() => core.Status(key).State == MachineState.Running, 180_000))
        {
            var run = System.Text.Json.JsonDocument.Parse(File.ReadAllText(Directory.EnumerateFiles(App.ArgValue("--library") ?? ".", "run.json", SearchOption.AllDirectories).First())).RootElement;
            System.Diagnostics.Process.GetProcessById(run.GetProperty("pid").GetInt32()).Kill();
            Add("a killed runner settles the machine, not Running", await Until(() => core.Status(key).State != MachineState.Running, 30_000), core.Status(key).State + " " + core.Status(key).Reason);
            try { System.Diagnostics.Process.GetProcessById(run.GetProperty("qemu_pid").GetInt32()).Kill(); } catch { }
        }
        else Add("a killed runner settles the machine, not Running", false, "the machine did not start again");
    }
}
#endif
