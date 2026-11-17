using Origami.Logic;

namespace Origami.App;

/// <summary>Self-test checks of the identity pieces shared by the fake-engine and QEMU runs.</summary>
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

    public static string? OutDir;
}
