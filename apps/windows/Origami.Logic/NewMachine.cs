namespace Origami.Logic;

/// <summary>One catalogue entry: a machine the core can create.</summary>
public sealed record Offering(string Id, string Engine, string Product, string Topology, int Nodes, int Cpus);

/// <summary>What the New Machine sheet shows, matching the Linux app.</summary>
public static class NewMachine
{
    public static string Label(Offering o) => $"{o.Id} ({o.Cpus} CPU{(o.Cpus == 1 ? "" : "s")})";

    /// <summary>The Octane2 VPro when offered, else the first.</summary>
    public static int DefaultIndex(IReadOnlyList<Offering> offerings)
    {
        for (int i = 0; i < offerings.Count; i++) if (offerings[i].Id == "octane2-vpro") return i;
        return 0;
    }

    /// <summary>Create is offered once the name has something besides spaces; the core rejects the rest.</summary>
    public static bool CanCreate(string name, int selected) => selected >= 0 && name.Trim().Length > 0;
}
