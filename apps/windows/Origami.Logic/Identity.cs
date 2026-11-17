using System.Text.Json;

namespace Origami.Logic;

public sealed record Still(string Id, string Name, uint Rgb);

/// <summary>The shared stills manifest (resources/identity/stills/manifest.json): which still a machine uses and its measured colour.</summary>
public sealed class StillCatalog
{
    readonly Dictionary<string, string> _byTopology = new(), _byProduct = new();
    readonly Dictionary<string, Still> _stills = new();

    public static StillCatalog Parse(string json)
    {
        var c = new StillCatalog();
        using var d = JsonDocument.Parse(json);
        var root = d.RootElement;
        foreach (var p in root.GetProperty("topologies").EnumerateObject()) c._byTopology[p.Name] = p.Value.GetString()!;
        foreach (var p in root.GetProperty("products").EnumerateObject()) c._byProduct[p.Name] = p.Value.GetString()!;
        foreach (var p in root.GetProperty("stills").EnumerateObject())
        {
            var hex = p.Value.GetProperty("colour").GetString()!.TrimStart('#');
            c._stills[p.Name] = new Still(p.Name, p.Value.GetProperty("name").GetString()!, Convert.ToUInt32(hex, 16));
        }
        return c;
    }

    public static StillCatalog Empty => Parse("""{"topologies":{},"products":{},"stills":{}}""");

    /// <summary>The topology names the exact machine; the product is the fallback.</summary>
    public Still? Resolve(string product, string? topology)
    {
        if (topology != null && _byTopology.TryGetValue(topology, out var t) && _stills.TryGetValue(t, out var s)) return s;
        return _byProduct.TryGetValue(product, out var p) && _stills.TryGetValue(p, out var s2) ? s2 : null;
    }
}

/// <summary>The two granite tones the app draws on, light and dark window themes.</summary>
public static class Granite
{
    public const uint DarkLight = 0x282A30, DarkDark = 0x1F2126, LightLight = 0xE2E4E8, LightDark = 0xD6D8DD;

    /// <summary>Recolours a two-tone BGRA tile to <paramref name="hi"/> and <paramref name="lo"/> and repeats it over
    /// <paramref name="w"/> by <paramref name="h"/> pixels, each tile pixel <paramref name="factor"/> wide, never smoothed.</summary>
    public static byte[] Fill(byte[] tileBgra, int tw, int th, int w, int h, int factor, uint hi, uint lo)
    {
        var mid = 0;
        for (int i = 0; i < tw * th; i++) mid += tileBgra[i * 4 + 1];
        mid /= tw * th; // the mean separates the two tones
        var outp = new byte[w * h * 4];
        factor = Math.Max(1, factor);
        for (int y = 0; y < h; y++)
            for (int x = 0; x < w; x++)
            {
                int tx = x / factor % tw, ty = y / factor % th;
                var light = tileBgra[(ty * tw + tx) * 4 + 1] >= mid;
                var c = light ? hi : lo;
                int o = (y * w + x) * 4;
                outp[o] = (byte)c; outp[o + 1] = (byte)(c >> 8); outp[o + 2] = (byte)(c >> 16); outp[o + 3] = 255;
            }
        return outp;
    }
}
