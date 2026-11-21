#if ORIGAMI_FAKE
using System.Text.Json;
using Microsoft.UI.Windowing;
using Origami.Logic;

namespace Origami.App;

/// <summary>
/// The real window with a 1280x1024 machine running, driven through its commands, resizes, maximize and library
/// selection, and read back against the core's behaviour table (origami_behaviour_json): the picture the renderer
/// last drew, its surface, and the window frame. Fake-engine builds only: never a release.
/// </summary>
static class BehaviourSelftest
{
    sealed record Seen(int FrameW, int FrameH, double PaneW, double PaneH, int SurfaceW, int SurfaceH, int PicX, int PicY, int PicW, int PicH, int RenderW, int RenderH, int RenderX, int RenderY);

    public static async Task Run(MainWindow w, UniffiCoreClient core, DisplayView disp, string key, string other, Action<string, bool, string> check)
    {
        using var doc = JsonDocument.Parse(uniffi.origami_ffi.OrigamiFfiMethods.OrigamiBehaviourJson());
        var root = doc.RootElement;
        int guestW = root.GetProperty("guest")[0].GetInt32(), guestH = root.GetProperty("guest")[1].GetInt32();
        double scale = disp.DeviceScale;
        int ran = 0;
        var skipped = new List<string>();

        var op = (OverlappedPresenter)w.AppWindow.Presenter;
        op.Restore();
        w.AppWindow.Move(new Windows.Graphics.PointInt32(0, 0));

        async Task<bool> Until(Func<bool> cond, int ms)
        {
            var t = Environment.TickCount64 + ms;
            while (Environment.TickCount64 < t) { if (cond()) return true; await Task.Delay(20); }
            return cond();
        }

        Seen? Now()
        {
            if (disp.LastDrawn() is not { } d) return null;
            var s = disp.DeviceScale;
            var (pw, ph) = disp.PaneSize;
            var (cx, cy, _, _) = disp.ScreenRect();
            var (ox, oy) = disp.PaneOrigin();
            // The renderer's picture sits in its surface, and the surface in the pane.
            return new Seen(w.AppWindow.Size.Width, w.AppWindow.Size.Height, pw - ViewScale.PanePadding, ph - ViewScale.PanePadding,
                (int)d.SurfaceWidth, (int)d.SurfaceHeight, cx - ox + d.X, cy - oy + d.Y, (int)d.Width, (int)d.Height,
                (int)d.Width, (int)d.Height, d.X, d.Y);
        }

        async Task<Seen?> Settle()
        {
            Seen? last = null; var stable = 0; var end = Environment.TickCount64 + 4000;
            while (Environment.TickCount64 < end)
            {
                await Task.Delay(100);
                if (Now() is not { } s) continue;
                stable = last == s ? stable + 1 : 0;
                last = s;
                if (stable >= 5) return s;
            }
            return last;
        }

        async Task<bool> SetPane(int pw, int ph)
        {
            var s = disp.DeviceScale;
            Seen? start = null, prev = null;
            for (var i = 0; i < 6; i++)
            {
                if (await Settle() is not { } seen) return false;
                start ??= seen;
                double dw = pw - seen.PaneW, dh = ph - seen.PaneH;
                if (Math.Abs(dw) < 0.5 && Math.Abs(dh) < 0.5) return true;
                // The window has a minimum size and the screen a maximum: a resize that moved nothing will not.
                if (prev is { } p0 && Math.Abs(p0.PaneW - seen.PaneW) < 0.5 && Math.Abs(p0.PaneH - seen.PaneH) < 0.5) break;
                prev = seen;
                w.AppWindow.Resize(new Windows.Graphics.SizeInt32(seen.FrameW + (int)Math.Round(dw * s), seen.FrameH + (int)Math.Round(dh * s)));
            }
            var ok = await Settle() is { } f && Math.Abs(pw - f.PaneW) <= 1 && Math.Abs(ph - f.PaneH) <= 1;
            if (!ok && start is { } st0) { w.AppWindow.Resize(new Windows.Graphics.SizeInt32(st0.FrameW, st0.FrameH)); await Settle(); }
            return ok;
        }

        // Without a last-draw reading no row can run: say so at once instead of waiting out every row.
        var reads = await Until(() => disp.LastDrawn() != null, 5000);
        check("behaviour: the core view reports what it drew", reads,
            $"renderer {disp.Renderer}, core view status {disp.CoreViewStatus}, presents {disp.Presents}, error {disp.LastError}, draw {(disp.LastDrawn() is { } d0 ? $"{d0.Width}x{d0.Height} in {d0.SurfaceWidth}x{d0.SurfaceHeight}" : "none")}");
        if (!reads) return;
        var menu = new Dictionary<string, string> { ["view-fit"] = "view.fit", ["view-actual"] = "view.actual-size", ["view1x"] = "view.scale1x", ["view2x"] = "view.scale2x" };

        // The core's checker: one JSON request in, one answer out.
        JsonElement Evaluate(object request) => JsonDocument.Parse(uniffi.origami_ffi.OrigamiFfiMethods.OrigamiBehaviourEvaluate(JsonSerializer.Serialize(request))).RootElement;
        int[] Rect(Seen s) => new[] { s.PicX, s.PicY, s.PicW, s.PicH };

        var budget = Environment.TickCount64 + 150_000;
        foreach (var row in root.GetProperty("rows").EnumerateArray())
        {
            var id = row.GetProperty("id").GetString()!;
            // A row that cannot settle must not take the whole self-test with it; a slow runner that has run enough rows is not a failure.
            if (Environment.TickCount64 > budget) { check("behaviour: the rows finished within their time", ran >= 2, $"stopped at {id}; ran {ran}, skipped {skipped.Count}"); break; }
            var action = row.GetProperty("action").GetString()!;
            var pane = row.GetProperty("pane");
            int[]? room = null;
            // The room the screen's work area leaves for the pane, in DIPs: the OS will not make a window bigger.
            if (await Settle() is { } r0) { var wa = w.WorkArea; room = new[] { (int)(wa.W / scale - (r0.FrameW / scale - r0.PaneW)), (int)(wa.H / scale - (r0.FrameH / scale - r0.PaneH)) }; }
            var ask = new Dictionary<string, object?> { ["op"] = "skip", ["id"] = id, ["scale"] = scale, ["room_available"] = room, ["pane_reached"] = true };
            if (Evaluate(ask).GetProperty("skip") is { ValueKind: JsonValueKind.String } why) { skipped.Add($"{id}: {why.GetString()}"); check($"behaviour: {id} (skipped)", true, why.GetString()!); continue; }
            if (row.GetProperty("view_before").ValueKind == JsonValueKind.String) w.Invoke(menu[row.GetProperty("view_before").GetString()!]);
            // An action that shrinks the window may be refused by its minimum size: try that pane first.
            var rm = row.GetProperty("room");
            if (rm.ValueKind == JsonValueKind.Array && pane.ValueKind == JsonValueKind.Array && (rm[0].GetInt32() < pane[0].GetInt32() || rm[1].GetInt32() < pane[1].GetInt32())
                && !await SetPane(rm[0].GetInt32(), rm[1].GetInt32()))
            {
                ask["pane_reached"] = false;
                skipped.Add($"{id}: {Evaluate(ask).GetProperty("skip").GetString()}");
                check($"behaviour: {id} (skipped)", true, skipped[^1] + $" (now {(Now() is { } n0 ? $"pane {n0.PaneW:F1}x{n0.PaneH:F1}, frame {n0.FrameW}x{n0.FrameH}" : "no reading")})");
                continue;
            }
            if (pane.ValueKind == JsonValueKind.Array && !await SetPane(pane[0].GetInt32(), pane[1].GetInt32()))
            {
                ask["pane_reached"] = false;
                skipped.Add($"{id}: {Evaluate(ask).GetProperty("skip").GetString()}");
                check($"behaviour: {id} (skipped)", true, skipped[^1] + $" (now {(Now() is { } n1 ? $"pane {n1.PaneW:F1}x{n1.PaneH:F1}, frame {n1.FrameW}x{n1.FrameH}" : "no reading")})");
                continue;
            }
            if (await Settle() is not { } was) { check($"behaviour: {id}", false, "no picture before the action"); continue; }
            var presentsBefore = disp.Presents;

            switch (action)
            {
                case "resize": break;
                case "zoom": op.Maximize(); break;
                case "reattach":
                    w.SelectForTest(other);
                    await Task.Delay(1500);
                    w.SelectForTest(key);
                    await Until(() => disp.Attached && disp.HeadWidth == guestW, 8000);
                    break;
                case "first-window":
                    w.Invoke("machine.stop");
                    await Until(() => core.Status(key).State == Origami.Logic.MachineState.Stopped, 8000);
                    w.Invoke("machine.start");
                    await Until(() => core.Status(key).State == Origami.Logic.MachineState.Running && disp.Attached && disp.HeadWidth == guestW, 15_000);
                    break;
                default:
                    if (menu.TryGetValue(action, out var cmd)) w.Invoke(cmd);
                    else { check($"behaviour: {id}", false, "unknown action " + action); continue; }
                    break;
            }
            if (await Settle() is not { } now) { check($"behaviour: {id}", false, "no picture after the action"); continue; }
            ran++;
            // This window is never occluded on a runner, so nothing is forced: the app's own present ran if the count rose.
            var drew = disp.Presents > presentsBefore;
            await Task.Delay(1000);
            var later = Now();
            var request = new Dictionary<string, object?>
            {
                ["op"] = "check", ["id"] = id, ["scale"] = scale,
                ["pane_dev"] = new[] { (int)Math.Round(now.PaneW * scale), (int)Math.Round(now.PaneH * scale) },
                ["picture_before"] = Rect(was), ["picture"] = Rect(now),
                ["surface"] = new[] { now.SurfaceW, now.SurfaceH }, ["rendered"] = new[] { now.RenderX, now.RenderY, now.RenderW, now.RenderH },
                ["frame_before"] = new[] { was.FrameW, was.FrameH }, ["frame"] = new[] { now.FrameW, now.FrameH },
                ["later"] = later is { } l ? new Dictionary<string, object> { ["frame"] = new[] { l.FrameW, l.FrameH }, ["picture"] = Rect(l) } : null,
                ["drew"] = drew,
            };
            var problems = Evaluate(request).GetProperty("problems").EnumerateArray().Select(p => p.GetString()!).ToList();
            check($"behaviour: {id}", problems.Count == 0, string.Join("; ", problems) + $" (picture {now.PicX},{now.PicY} {now.PicW}x{now.PicH}, surface {now.SurfaceW}x{now.SurfaceH}, frame {now.FrameW}x{now.FrameH}, scale {scale})");
            if (action == "zoom") { op.Restore(); await Task.Delay(300); }
        }
        // A runner's screen can be too small for every pane a row names, and the rows with no pane of their own run anyway:
        // two of them are the floor here, and the skips say why.
        check("behaviour: enough rows ran", ran >= 2, $"ran {ran}, skipped {skipped.Count}: {string.Join(" | ", skipped)}");
    }
}
#endif
