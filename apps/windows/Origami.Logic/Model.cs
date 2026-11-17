namespace Origami.Logic;

public enum MachineControl { Reset, PowerButton, Nmi, Pause, Resume }

public enum MachineState { Stopped, Starting, Running, Paused, Stopping, Exited, LeftRunning }

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
    bool HasDisplay = true);

public sealed record MachineStatus(MachineState State, string Reason, string? LastError);

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

public static class Pill
{
    /// <summary>One or two words, as the UX design's state table words them.</summary>
    public static string Text(MachineState s, bool headless = false) => s switch
    {
        MachineState.Stopped => "Stopped",
        MachineState.Starting => "Starting",
        MachineState.Running => headless ? "Console only" : "Running",
        MachineState.Paused => "Paused",
        MachineState.Stopping => "Saving",
        MachineState.Exited => "Stopped unexpectedly",
        MachineState.LeftRunning => "Left running",
        _ => "Unknown",
    };

    /// <summary>Why a machine that ended on its own stopped: the core's last error, else its reason; empty in any other state.</summary>
    public static string StoppedNote(MachineStatus st) =>
        st.State != MachineState.Exited ? "" : "Stopped unexpectedly. " + (st.LastError is { Length: > 0 } e ? e : st.Reason);

    /// <summary>"grey", "amber", "green" or "red". Colour always travels with the word.</summary>
    public static string Tone(MachineState s) => s switch
    {
        MachineState.Stopped => "grey",
        MachineState.Starting or MachineState.Stopping or MachineState.LeftRunning => "amber",
        MachineState.Running or MachineState.Paused => "green",
        MachineState.Exited => "red",
        _ => "grey",
    };
}
