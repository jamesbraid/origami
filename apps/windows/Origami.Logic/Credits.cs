using System.Text.Json;
using System.Text.Json.Serialization;

namespace Origami.Logic;

/// <summary>The credits the core builds (origami_credits_json), the same document the other apps show.</summary>
public sealed class CreditsDocument
{
    public sealed class Project { public string Name { get; init; } = ""; public string By { get; init; } = ""; public string Url { get; init; } = ""; public string Licence { get; init; } = ""; public string Note { get; init; } = ""; }
    public sealed class ThanksEntry { public string Name { get; init; } = ""; [JsonPropertyName("for")] public string Reason { get; init; } = ""; public string Url { get; init; } = ""; }
    public sealed class Photograph { public string Title { get; init; } = ""; public string Author { get; init; } = ""; public string Year { get; init; } = ""; public string Licence { get; init; } = ""; public string LicenceUrl { get; init; } = ""; public string Source { get; init; } = ""; public string Changes { get; init; } = ""; }
    public sealed class ArtworkEntry { public string Name { get; init; } = ""; public string From { get; init; } = ""; public string Url { get; init; } = ""; }
    public sealed class QemuSource { public string Tag { get; init; } = ""; public string Url { get; init; } = ""; public string ArchiveUrl { get; init; } = ""; public string Note { get; init; } = ""; }
    public sealed class LicenceEntry
    {
        public List<string> Components { get; init; } = new();
        public string Text { get; init; } = "";
        /// <summary>The components as one line that fits a row: the first few, then how many more.</summary>
        public string Title
        {
            get
            {
                var s = "";
                for (var i = 0; i < Components.Count; i++)
                {
                    if (i > 0 && s.Length + Components[i].Length > 60) return s + $" and {Components.Count - i} more";
                    s += (i > 0 ? ", " : "") + Components[i];
                }
                return s;
            }
        }
    }
    public sealed class Licence
    {
        public string Name { get; init; } = "";
        public List<LicenceEntry> Entries { get; init; } = new();
        public int ComponentCount => Entries.Sum(e => e.Components.Count);
        public string Count => ComponentCount == 1 ? "1 component" : $"{ComponentCount} components";
    }

    public string Copyright { get; init; } = "";
    public string LicenceName { get; init; } = "";
    public string ProjectUrl { get; init; } = "";
    public string Trademark { get; init; } = "";
    public List<Project> BuiltOn { get; init; } = new();
    public List<ThanksEntry> Thanks { get; init; } = new();
    public List<Photograph> Photographs { get; init; } = new();
    public List<ArtworkEntry> Artwork { get; init; } = new();
    public QemuSource Qemu { get; init; } = new();
    public List<Licence> Licences { get; init; } = new();

    static readonly JsonSerializerOptions Options = new() { PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower };

    /// <summary>Reads the document; throws with the core's message when it reports an error instead.</summary>
    public static CreditsDocument Parse(string json)
    {
        using (var probe = JsonDocument.Parse(json))
            if (probe.RootElement.TryGetProperty("error", out var error))
                throw new InvalidOperationException(error.GetString() ?? "the credits could not be built");
        return JsonSerializer.Deserialize<CreditsDocument>(json, Options) ?? throw new InvalidOperationException("empty credits");
    }

    /// <summary>The licences with only the entries whose components or licence name contain <paramref name="query"/>, ignoring case.</summary>
    public List<Licence> Filter(string query)
    {
        if (string.IsNullOrWhiteSpace(query)) return Licences;
        var found = new List<Licence>();
        foreach (var l in Licences)
        {
            var nameHit = l.Name.Contains(query, StringComparison.OrdinalIgnoreCase);
            var entries = l.Entries.Where(e => nameHit || e.Components.Any(c => c.Contains(query, StringComparison.OrdinalIgnoreCase))).ToList();
            if (entries.Count > 0) found.Add(new Licence { Name = l.Name, Entries = entries });
        }
        return found;
    }
}
