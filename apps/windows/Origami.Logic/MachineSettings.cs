namespace Origami.Logic;

public sealed record Forward(string Name, string Protocol, int HostPort, int GuestPort);
public sealed record Share(string Name, string HostDir);
public sealed record Drive(string Name, string Kind, int Bus, int Target, string Image, bool ReadOnly);
public sealed record Pipe(string Slot, string Product, string Family);

/// <summary>What a machine's file says, for the settings sheet.</summary>
public sealed record MachineSettings(
    int Nodes, string Processor, string ProcessorPart,
    int CpusPerNode, IReadOnlyList<int> CpusAllowed,
    int MemoryMiB, IReadOnlyList<int> MemoryAllowed,
    IReadOnlyList<Pipe> Pipes, IReadOnlyList<Drive> Drives,
    string NetworkMode, IReadOnlyList<Forward> Forwards, IReadOnlyList<Share> Shares);

/// <summary>One edit to a stopped machine; the core refuses it with Busy while the machine runs.</summary>
public abstract record MachineChange
{
    public sealed record SetCpus(int CpusPerNode) : MachineChange;
    public sealed record SetMemory(int MiB) : MachineChange;
    /// <summary><c>user</c>, <c>private</c> or <c>none</c>.</summary>
    public sealed record SetNetwork(string Mode) : MachineChange;
    public sealed record AddForward(Forward Forward) : MachineChange;
    public sealed record RemoveForward(string Name) : MachineChange;
    public sealed record AddShare(Share Share) : MachineChange;
    public sealed record RemoveShare(string Name) : MachineChange;
    public sealed record DetachDrive(string Name) : MachineChange;
}

/// <summary>What the settings sheet decides itself, matching the Linux app.</summary>
public static class MachineSettingsRules
{
    public static readonly (string Mode, string Label)[] NetworkModes = { ("user", "Shared (NAT)"), ("private", "Private"), ("none", "Off") };

    /// <summary>Hardware settings are locked while the machine is up.</summary>
    public static bool Editable(MachineState s) => s is MachineState.Stopped or MachineState.Exited;

    public static bool CanAddForward(string name, int hostPort, int guestPort) =>
        name.Trim().Length > 0 && hostPort is >= 1 and <= 65535 && guestPort is >= 1 and <= 65535;

    public static bool CanAddShare(string name, string dir) => name.Trim().Length > 0 && dir.Trim().Length > 0;

    /// <summary>The folder's own name, as the default share name.</summary>
    public static string ShareNameFor(string dir) => Path.GetFileName(dir.TrimEnd('\\', '/')) is { Length: > 0 } n ? n : "share";
}
