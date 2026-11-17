using System.Text;

namespace Origami.Logic;

public sealed record HeadDiag(int Index, int Width, int Height, bool Active, ulong Seq);

/// <summary>What the core says about one machine, for the Diagnostics window and its Copy Report.</summary>
public sealed record MachineDiag(
    string Name, string Engine, MachineState State, string Reason, string? LastError,
    IReadOnlyList<HeadDiag> Heads, bool ConsoleConnected, string? ConsoleDownReason, ulong ConsoleBytes);

public static class DiagnosticsReport
{
    /// <summary>The monospaced report; <paramref name="wakes"/> and <paramref name="presents"/> are the app's counts for the screen.</summary>
    public static string Format(MachineDiag d, long wakes, long presents)
    {
        var s = new StringBuilder();
        s.AppendLine($"Machine:      {d.Name}");
        s.AppendLine($"Engine:       {d.Engine}");
        s.AppendLine($"State:        {Pill.Text(d.State)} ({d.State})");
        s.AppendLine($"Reason:       {d.Reason}");
        s.AppendLine($"Last error:   {(string.IsNullOrEmpty(d.LastError) ? "none" : d.LastError)}");
        s.AppendLine("Heads:");
        if (d.Heads.Count == 0) s.AppendLine("  none");
        foreach (var h in d.Heads)
            s.AppendLine($"  #{h.Index}  {h.Width}x{h.Height}  picture {(h.Active ? "yes" : "no")}  sequence {h.Seq}");
        s.AppendLine($"Console:      {(d.ConsoleConnected ? "connected" : "not connected" + (string.IsNullOrEmpty(d.ConsoleDownReason) ? "" : " (" + d.ConsoleDownReason + ")"))}, {d.ConsoleBytes} bytes");
        s.AppendLine($"Screen:       {wakes} wake-ups, {presents} presents");
        return s.ToString();
    }
}
