namespace Origami.Logic;

public enum LineKind { Result, Detail, Advice, Warning, Note, GuestNote }

/// <summary>One line of what an operation had to say.</summary>
/// <summary><c>Shown</c> is what a person is told, from the core, or null for the lines kept for the log; <c>Copy</c> is a command to offer a Copy button for.</summary>
public sealed record ReportLine(LineKind Kind, string Text, string? Shown = null, string? Copy = null);

/// <summary>The words of upgrading an older machine; the macOS and GTK apps say the same.</summary>
public static class UpgradeText
{
    public const string Title = "Needs upgrade";
    public const string Explanation = "This machine was made by an older Origami. Upgrade it once to use it. Flash, NVRAM, drives, network settings and the Ethernet address are kept.";

    /// <summary>The core's sentence, with the command for local firmware files when a download failed.</summary>
    public static string Failure(string error, string dir) =>
        error.Contains("download", StringComparison.OrdinalIgnoreCase) ? $"{error} To use local firmware files, run: origami upgrade {dir} --prom FILE --io-prom FILE" : error;

    /// <summary>What a person is told: the lines the core marks to show.</summary>
    public static List<string> Shown(IEnumerable<ReportLine> lines) => lines.Select(l => l.Shown).OfType<string>().ToList();
}
