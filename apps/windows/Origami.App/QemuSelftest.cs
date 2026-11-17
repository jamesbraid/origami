#if !ORIGAMI_FAKE
using System.Text.Json;
using Origami.Logic;

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

    public static async Task RunAsync(MainWindow w, UniffiCoreClient core, string outFile)
    {
        _out = outFile;
        var dir = Path.GetDirectoryName(Path.GetFullPath(outFile))!;
        _ = Task.Run(async () => { await Task.Delay(420_000); w.DispatcherQueue.TryEnqueue(() => Fail("watchdog: 420 s")); });
        try { await Scenario(w, core, dir); }
        catch (Exception e) { Add("exception", false, e.ToString()); }
        // A machine outlives the app by design: do not leave one running behind a failed test.
        foreach (var m in core.Machines().Where(m => m.State is MachineState.Running or MachineState.Starting or MachineState.Paused))
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
            try { key = await Task.Run(() => core.CreateFrom("selftest-" + product, product, App.ArgValue("--graphics"))); }
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
        // Exact compare: the core's own readback of what its view drew against QEMU's screendump of the same moment.
        // The PROM menu is static once drawn, so the two are taken a moment apart without a frame between them.
        {
            var ppm = Path.Combine(dir, "qemu-screendump.ppm");
            var rbk = disp.CoreReadback(out var rw, out var rh, headSize: true);
            core.Screendump(key, 0, ppm);
            var (pw, ph, engine) = Shot.ReadPpm(ppm);
            if (rbk == null) Add("first-picture: the core's readback equals QEMU's screendump", false, "no readback: " + disp.LastError);
            else if (rw != pw || rh != ph) Add("first-picture: the core's readback equals QEMU's screendump", false, $"size {rw}x{rh} vs {pw}x{ph}");
            else
            {
                int bad = 0;
                for (int i = 0; i < pw * ph; i++)
                {
                    int o = i * 4;
                    if (rbk[o] != engine[o] || rbk[o + 1] != engine[o + 1] || rbk[o + 2] != engine[o + 2]) bad++;
                }
                Add("first-picture: the core's readback equals QEMU's screendump", bad == 0, $"{bad} mismatching pixels of {pw * ph} ({pw}x{ph})");
                await Shot.Readback(rbk, rw, rh, rw * 4, Path.Combine(dir, "core-readback.png"));
            }
        }
        Add("QEMU screendump shows a picture", lit > 100, dumpError is null ? $"{lit} lit pixels of {hw * hh} ({hw}x{hh})" : "screendump failed: " + dumpError);
        if (disp.Verify(out _) is { } rb) Add("fallback readback taken", true, $"{rb.Length} bytes");
        try { await Shot.Xaml((Microsoft.UI.Xaml.UIElement)w.Content, Path.Combine(dir, "xaml.png")); } catch { }

        File.WriteAllText(Path.Combine(dir, "ready.txt"), "ready");
        await Until(() => File.Exists(Path.Combine(dir, "done.txt")), 15_000);

        // Pause and resume the real machine; the picture keeps its head.
        w.Invoke("machine.pause");
        Add("pause: the machine is Paused", await Until(() => core.Status(key).State == MachineState.Paused, 30_000), core.Status(key).Reason + " | status bar: " + w.StatusForTest + " | enabled: " + core.CommandEnabled("machine.pause", core.Status(key).State));
        if (core.Status(key).State == MachineState.Paused)
        {
            w.Invoke("machine.pause");
            Add("resume: the same command resumes", await Until(() => core.Status(key).State == MachineState.Running, 30_000), core.Status(key).State.ToString());
        }
        else Add("resume: the same command resumes", false, "not tested: the machine never paused");
        // A clean stop waits for the guest (and a PROM menu never answers); force-quit ends QEMU at once.
        w.Invoke("machine.force-quit");
        Add("force quit settles as Stopped, not stopped unexpectedly", await Until(() => core.Status(key).State == MachineState.Stopped, 60_000), core.Status(key).State + " " + core.Status(key).Reason);
    }
}
#endif
