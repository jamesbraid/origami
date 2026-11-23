namespace Origami.Logic;

/// <summary>One catalogue entry: a machine the core can create.</summary>
public sealed record Offering(string Id, string Engine, string Product, string Topology, int Nodes, int Cpus, bool RunsIrix = true, string Label = "");

/// <summary>What the New Machine sheet shows, matching the Linux app.</summary>
public static class NewMachine
{
    public static string Label(Offering o) => o.Label;

    /// <summary>One entry per offering ID: two engines may list the same machine.</summary>
    public static IReadOnlyList<Offering> Models(IReadOnlyList<Offering> offerings) => offerings.DistinctBy(o => o.Id).ToList();

    /// <summary>The engines that offer <paramref name="id"/>, in catalogue order.</summary>
    public static IReadOnlyList<string> EnginesFor(IReadOnlyList<Offering> offerings, string id) =>
        offerings.Where(o => o.Id == id).Select(o => o.Engine).Distinct().ToList();

    /// <summary>The Octane2 VPro when offered, else the first.</summary>
    public static int DefaultIndex(IReadOnlyList<Offering> offerings)
    {
        for (int i = 0; i < offerings.Count; i++) if (offerings[i].Id == "octane2-vpro") return i;
        return 0;
    }

    /// <summary>Create is offered once the name has something besides spaces; the core rejects the rest.</summary>
    public static bool CanCreate(string name, int selected) => selected >= 0 && name.Trim().Length > 0;
}
