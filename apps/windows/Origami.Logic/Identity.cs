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
}
