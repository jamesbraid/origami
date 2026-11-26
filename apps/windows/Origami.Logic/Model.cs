namespace Origami.Logic;

public enum MachineControl { Reset, PowerButton, Nmi, Pause, Resume }

public enum MachineState { Stopped, Starting, Running, Paused, Stopping, Exited, LeftRunning, NeedsUpgrade }

public sealed record MachineSummary(
    string Key,
    string Name,
    string Product,
    string ProductLabel,
    string? Topology,
    int Nodes,
    int MemoryPerNodeMiB,
    MachineState State,
    string? Problem,
    bool HasDisplay = true,
    bool NeedsUpgrade = false,
    /// <summary>What the system disk holds, such as "IRIX 6.5.30", once the core has read it.</summary>
    string? SystemDisk = null,
    /// <summary>Whether the machine runs IRIX: Install IRIX is only for those.</summary>
    bool RunsIrix = true,
    /// <summary>The tiles under the name, as the core words them.</summary>
    IReadOnlyList<SummaryCard>? Cards = null);

public sealed record SummaryCard(string Title, string Value);

/// <summary>Note is the core's reason when it says more than the state pill does.</summary>
public sealed record MachineStatus(MachineState State, string Reason, string? LastError, string? Note = null);

/// <summary>One head's picture as the core reports it. Seq moves when the picture does.</summary>
public sealed record DisplayInfo(bool Active, int Width, int Height, ulong Seq);

public enum MachineEventKind { StateChanged, DisplayChanged, ConsoleOutput, OperationChanged, LibraryChanged }

public sealed record MachineEvent(string Key, MachineEventKind Kind);

public enum InputKind { Key, PointerMove, PointerAt, Button, Wheel }

/// <summary>One input event; a key's code is evdev, mapped to the core's key by <see cref="GuestKeys"/>.</summary>
public readonly record struct InputEvent(InputKind Kind, int A = 0, int B = 0, bool Down = false)
{
    public static InputEvent Key(int evdev, bool down) => new(InputKind.Key, evdev, 0, down);
    public static InputEvent Move(int dx, int dy) => new(InputKind.PointerMove, dx, dy);
    public static InputEvent At(int x, int y) => new(InputKind.PointerAt, x, y);
    /// <summary>Button 0 left, 1 middle, 2 right.</summary>
    public static InputEvent Button(int b, bool down) => new(InputKind.Button, b, 0, down);
    public static InputEvent Wheel(int notches) => new(InputKind.Wheel, notches);
}
