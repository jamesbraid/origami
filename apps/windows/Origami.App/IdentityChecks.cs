using Origami.Logic;

namespace Origami.App;

/// <summary>Self-test checks of the identity pieces (cube, About) shared by the fake-engine and QEMU runs.</summary>
static class IdentityChecks
{
    /// <summary>A picture with real content: many distinct colours, not one flat fill.</summary>
    public static int Colours(byte[] bgrx)
    {
        var seen = new HashSet<int>();
        for (int i = 0; i + 3 < bgrx.Length && seen.Count < 4096; i += 4) seen.Add(bgrx[i] | bgrx[i + 1] << 8 | bgrx[i + 2] << 16);
        return seen.Count;
    }

    public static bool Differ(byte[] a, byte[] b)
    {
        if (a.Length != b.Length) return true;
        for (int i = 0; i < a.Length; i += 4) if (a[i] != b[i] || a[i + 1] != b[i + 1] || a[i + 2] != b[i + 2]) return true;
        return false;
    }

    /// <summary>Whether the cube turns: true once a readback differs from <paramref name="first"/>, polled for up to a few seconds.</summary>
    public static async Task<bool> Turns(CubeHost cube, byte[] first)
    {
        for (int i = 0; i < 12; i++)
        {
            await Task.Delay(300);
            if (cube.Readback(out _, out _) is { } next && Differ(first, next)) return true;
        }
        return false;
    }

    /// <summary>A cube with no window drawn once at rest in <paramref name="scene"/> over nothing: tight BGRA rows.</summary>
    static unsafe byte[]? OffscreenCube(uint w, uint h, double scale, uint scene)
    {
        if (Frame.origami_cube_attach_offscreen(w, h, scale, out var cube) != Frame.Ok) return null;
        try
        {
            var st = new Frame.CubeStyle
            {
                Size = (uint)System.Runtime.InteropServices.Marshal.SizeOf<Frame.CubeStyle>(), Env = Frame.CubeEnvScene, Scene = scene,
                HorizonRgb = Frame.CubeNoColour, GraniteLightRgb = Granite.DarkLight, GraniteDarkRgb = Granite.DarkDark, Backdrop = Frame.CubeBackdropNone,
            };
            if (Frame.origami_cube_set_style(cube, ref st) != Frame.Ok) return null;
            if (Frame.origami_cube_present(cube, out _, out _) != Frame.Ok) return null;
            Frame.origami_cube_readback(cube, null, 0, out var uw, out var uh, out var stride);
            var buf = new byte[stride * uh];
            int rc;
            fixed (byte* p = buf) rc = Frame.origami_cube_readback(cube, p, (nuint)buf.Length, out uw, out uh, out stride);
            return rc == Frame.Ok && stride == uw * 4 ? buf : null;
        }
        finally { Frame.origami_cube_detach(cube); }
    }

    /// <summary>How rough the cube's edges are: the distinct partial-coverage alpha levels on its silhouette (more is smoother), and the
    /// mean error in 0 to 255 of a <paramref name="side"/> px render against one at 4x box-filtered down (less is smoother).</summary>
    public static (int Levels, double Error)? EdgeQuality(int side, uint scene)
    {
        var one = OffscreenCube((uint)side, (uint)side, 1, scene);
        var big = OffscreenCube((uint)(side * 4), (uint)(side * 4), 4, scene);
        if (one is null || big is null) return null;
        var levels = new HashSet<byte>();
        for (int i = 3; i < one.Length; i += 4) if (one[i] > 0 && one[i] < 255) levels.Add(one[i]);
        long sum = 0;
        for (int y = 0; y < side; y++)
            for (int x = 0; x < side; x++)
                for (int c = 0; c < 4; c++)
                {
                    int acc = 0;
                    for (int k = 0; k < 16; k++) acc += big[((y * 4 + k / 4) * side * 4 + x * 4 + k % 4) * 4 + c];
                    sum += Math.Abs(one[(y * side + x) * 4 + c] - (int)Math.Round(acc / 16.0));
                }
        return (levels.Count, (double)sum / (side * side * 4));
    }

    public static string? OutDir;

    /// <summary>Opens About, presses Acknowledgements and Licences and checks what each shows against the core's credits.
    /// <paramref name="shoot"/>, when given, photographs a sheet under a name.</summary>
    public static async Task<List<(string, bool, string)>> Credits(MainWindow w, Func<Func<bool>, int, Task<bool>> until, Func<string, Microsoft.UI.Xaml.Window, Task>? shoot = null)
    {
        var steps = new List<(string, bool, string)>();
        w.Invoke("app.about");
        var about = w.AboutForTest;
        if (about is null) { steps.Add(("credits: About opens", false, "no About window")); return steps; }
        var doc = CreditsDocument.Parse(uniffi.origami_ffi.OrigamiFfiMethods.OrigamiCreditsJson());

        about.ClickAcknowledgements();
        var opened = await until(() => about.Sheet is { Which: CreditsWindow.Page.Acknowledgements }, 5000);
        steps.Add(("credits: the Acknowledgements button opens its sheet", opened, ""));
        if (opened && about.Sheet is { } ack)
        {
            await Task.Delay(500);
            var missing = doc.Photographs.Where(p => !ack.Text.Contains(p.Author) || !ack.Text.Contains(p.Licence) || !ack.Text.Contains(p.Source)).Select(p => p.Title).ToList();
            steps.Add(("credits: Acknowledgements credits every photograph with its author, licence and source", missing.Count == 0, string.Join(", ", missing)));
            steps.Add(("credits: Acknowledgements names QEMU's source build, Iris, the thanks and the artwork",
                ack.Text.Contains(doc.Qemu.Label) && ack.Text.Contains("Iris") && doc.Thanks.All(t => ack.Text.Contains(t.Name)) && doc.Artwork.All(a => ack.Text.Contains(a.Name)), doc.Qemu.Label));
            if (shoot != null) await shoot("windows-acknowledgements", ack);
            ack.Close();
            steps.Add(("credits: closing Acknowledgements leaves no sheet behind", await until(() => about.Sheet is null, 3000), ""));
        }

        about.ClickLicences();
        opened = await until(() => about.Sheet is { Which: CreditsWindow.Page.Licences }, 5000);
        steps.Add(("credits: the Licences button opens its sheet", opened, ""));
        if (opened && about.Sheet is { } lic)
        {
            await Task.Delay(500);
            steps.Add(("credits: Licences lists every licence group, QEMU's GPL first", lic.ShownGroups.Count == doc.Licences.Count && lic.ShownGroups[0] == "GNU General Public License", string.Join(", ", lic.ShownGroups)));
            steps.Add(("credits: Licences carries the written note that QEMU's source is at the tag", lic.Text.Contains(doc.Qemu.Note), doc.Qemu.Note));
            lic.ExpandGroup(0);
            await Task.Delay(500);
            if (shoot != null) await shoot("windows-licences", lic);
            lic.Search("ghostty");
            steps.Add(("credits: searching Licences narrows the groups to the match", await until(() => lic.ShownGroups.Count == doc.Filter("ghostty").Count && lic.ShownGroups.Count is > 0 and < 3, 3000), string.Join(", ", lic.ShownGroups)));
            lic.Close();
        }
        about.Close();
        steps.Add(("credits: closing About leaves no window behind", await until(() => w.AboutForTest is null, 3000), ""));
        return steps;
    }

    /// <summary>Opens About, checks the cube drew, and closes it. Returns (name, ok, detail) steps.</summary>
    public static async Task<List<(string, bool, string)>> About(MainWindow w, Func<Func<bool>, int, Task<bool>> until)
    {
        var steps = new List<(string, bool, string)>();
        w.Invoke("app.about");
        var about = w.AboutForTest;
        if (about is null) { steps.Add(("about: the window opens", false, "no About window")); return steps; }
        var attached = await until(() => about.Cube.Attached, 10_000);
        steps.Add(("about: the cube attached to its window", attached, about.Cube.LastError ?? $"status {about.Cube.Status}"));
        if (!attached) { about.Close(); return steps; }
        steps.Add(("about: the cube shows a photograph, not the running machine", !about.ReflectsMachine, ""));
        await Task.Delay(800);
        var a = about.Cube.Readback(out var aw, out var ah);
        steps.Add(("about: the cube draws a picture (not a flat fill)", a != null && Colours(a) > 50, a == null ? about.Cube.LastError ?? "no readback" : $"{aw}x{ah}, {Colours(a)} colours"));
        if (a != null && OutDir != null) await Shot.Readback(a, aw, ah, aw * 4, Path.Combine(OutDir, "about-cube.png"));
        await Task.Delay(500);
        var b = about.Cube.Readback(out _, out _);
        if (!about.ReflectsMachine) steps.Add(("about: the cube is at rest (two frames equal)", a != null && b != null && !Differ(a, b), ""));
        // A gradient, not a tile: the left edge steps through many levels (a tile has two) and the top is not the bottom.
        if (a != null && aw > 0 && ah > 1)
        {
            var edge = new HashSet<int>();
            for (int y = 0; y < ah; y++) { var o = y * aw * 4; edge.Add(a[o] | a[o + 1] << 8 | a[o + 2] << 16); }
            var bottom = (ah - 1) * aw * 4;
            steps.Add(("about: the background is a gradient, not granite", edge.Count >= 8 && (a[0] != a[bottom] || a[1] != a[bottom + 1] || a[2] != a[bottom + 2]), $"{edge.Count} levels down the edge"));
        }
        // A drag turns the cube; once released it comes to rest at the logo pose, the same pixels as before.
        about.Cube.PointerForTest(Frame.CubePointerPress, 120, 100, 0);
        about.Cube.PointerForTest(Frame.CubePointerMove, 220, 120, 16);
        var turned = about.Cube.Readback(out _, out _);
        about.Cube.PointerForTest(Frame.CubePointerRelease, 220, 120, 500);
        var returned = await until(() => !about.Cube.Moving, 10_000);
        var back = about.Cube.Readback(out _, out _);
        steps.Add(("about: the cube turns when dragged and returns to rest", a != null && turned != null && back != null && Differ(a, turned) && returned && !Differ(a, back),
            $"turned={a != null && turned != null && Differ(a, turned)}, rest={returned}, same pixels={a != null && back != null && !Differ(a, back)}"));
        // Edges: the silhouette has partial-coverage pixels from 4x multisampling or better, and a render at the view's size is
        // close to the same render at 4x filtered down.
        var q = EdgeQuality(160, Frame.origami_cube_scene_for_time(12, 0));
        steps.Add(("cube: the edges are antialiased", q is { Levels: >= 3, Error: < 4.5 }, q is { } e ? $"{e.Levels} partial-coverage levels, mean error {e.Error:F2} of 255 against 4x supersampling" : "no offscreen cube"));
        // The scene is the one for the local time; the right arrow and a Shift-click step on, a plain click does not.
        var t0 = DateTime.Now;
        var timed = new[] { (int)Frame.origami_cube_scene_for_time((uint)t0.Hour, (uint)t0.Minute), (int)Frame.origami_cube_scene_for_time((uint)DateTime.Now.Hour, (uint)DateTime.Now.Minute) };
        var scene0 = about.Cube.CurrentScene;
        steps.Add(("about: the scene is the one for the time of day", scene0 >= 0 && timed.Contains(scene0), $"scene {scene0}, for the time {string.Join("/", timed)}"));
        Win.Foreground(about.Hwnd);
        about.FocusForKeys();
        await Task.Delay(300);
        Win.Scan(0x4D, true, true); Win.Scan(0x4D, true, false); // Right
        steps.Add(("about: the right arrow steps to the next scene", await until(() => about.Cube.CurrentScene != scene0, 3000), $"scene {scene0} -> {about.Cube.CurrentScene}"));
        var scene1 = about.Cube.CurrentScene;
        about.Cube.ClickForTest(false); await Task.Delay(200);
        steps.Add(("about: a plain click does not step", about.Cube.CurrentScene == scene1, $"scene {scene1} -> {about.Cube.CurrentScene}"));
        about.Cube.ClickForTest(true);
        steps.Add(("about: a Shift-click steps to the next scene", await until(() => about.Cube.CurrentScene != scene1, 3000), $"scene {scene1} -> {about.Cube.CurrentScene}"));
        // The egg, with real key input to the About window.
        foreach (var sc in new[] { 0x23, 0x17, 0x31, 0x2F }) { Win.Scan(sc, false, true); Win.Scan(sc, false, false); await Task.Delay(40); }
        var shown = await until(() => about.HinvShown && about.HinvText.Length > 0, 15_000);
        steps.Add(("about: typing hinv slides the host inventory over the credits", shown, about.HinvText.Split('\n')[0]));
        Win.Scan(0x01, false, true); Win.Scan(0x01, false, false); // Esc
        steps.Add(("about: Esc puts the credits back", await until(() => !about.HinvShown, 3000), ""));
        about.Close();
        steps.Add(("about: closing it leaves no window behind", await until(() => w.AboutForTest is null, 3000), ""));
        return steps;
    }
}
