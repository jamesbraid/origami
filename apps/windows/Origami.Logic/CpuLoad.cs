namespace Origami.Logic;

public sealed record CpuBarInfo(int Cpu, double UserPct, double SysPct);

/// <summary>The latest CPU load sample; with no CPUs, <see cref="Unavailable"/> says why, or the first sample is still pending.</summary>
public sealed record CpuLoadInfo(IReadOnlyList<CpuBarInfo> Cpus, string? Unavailable);

public static class CpuPanel
{
    public const string Title = "Emulated CPU load (measured on the host)";
    public static readonly TimeSpan Interval = TimeSpan.FromMilliseconds(500); // 2 Hz

    public static string Label(int cpu) => $"cpu {cpu}";

    /// <summary>The user+system percentage at the right of a bar, whole numbers.</summary>
    public static string Total(CpuBarInfo c) => $"{Math.Round(Math.Clamp(c.UserPct + c.SysPct, 0, 100)):0}%";

    /// <summary>Widths of the user and system segments within a bar of <paramref name="width"/>; together never more than the bar.</summary>
    public static (double User, double Sys) Segments(CpuBarInfo c, double width)
    {
        var user = Math.Clamp(c.UserPct, 0, 100);
        var sys = Math.Clamp(c.SysPct, 0, 100 - user);
        return (width * user / 100, width * sys / 100);
    }

    /// <summary>What to show when there are no bars.</summary>
    public static string Empty(CpuLoadInfo r) => string.IsNullOrWhiteSpace(r.Unavailable) ? "measuring…" : r.Unavailable!;
}
