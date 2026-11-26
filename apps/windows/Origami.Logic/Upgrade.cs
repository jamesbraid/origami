namespace Origami.Logic;

public enum LineKind { Result, Detail, Advice, Warning, Note, GuestNote }

/// <summary>One line of what an operation had to say.</summary>
/// <summary><c>Shown</c> is what a person is told, from the core, or null for the lines kept for the log; <c>Copy</c> is a command to offer a Copy button for.</summary>
public sealed record ReportLine(LineKind Kind, string Text, string? Shown = null, string? Copy = null);
