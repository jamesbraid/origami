namespace Origami.Logic;

public sealed record Forward(string Name, string Protocol, int HostPort, int GuestPort);
/// <summary>A shared folder; <c>MountLines</c> are what to run as root in IRIX, empty when <c>MountProblem</c> says why not.</summary>
public sealed record Share(string Name, string HostDir, IReadOnlyList<string>? MountLines = null, string? MountProblem = null, bool NeedsAddress = false);
public sealed record Drive(string Name, string Kind, int Bus, int Target, string Image, bool ReadOnly);
public sealed record Pipe(string Slot, string Product, string Family);
public sealed record ConsoleLine(string Name, string Label, bool IsDefault);

/// <summary>A machine's network as its file has it; <c>Mode</c> is user, private, none, tap or vmnet.</summary>
public sealed record NetworkSetting(string Mode, string Device, string? ShareAddress, string Subnet, string? Mac, string MacNote, IReadOnlyList<string> Warnings);

/// <summary>One way to connect a machine that this computer offers.</summary>
public sealed record NetworkChoice(string Mode, string Label, string Detail);

/// <summary>How to set up the host side of Bridged here: the field's label, an example for it, the steps and the commands to copy.</summary>
public sealed record BridgedSetup(string Field, string Example, string Steps, string? Commands);

public enum IdentityField { Id, Address }

/// <summary>What New Machine shows for the machine ID and Ethernet address; <c>Problem</c> is the core's reason it cannot be used, and <c>Field</c> the one it belongs under.</summary>
public sealed record IdentityPreview(uint EmuId, string Mac, bool FromId, string? Problem, IdentityField? Field = null);

/// <summary>A machine ID and an Ethernet address to give a new machine instead of the next free ones.</summary>
public sealed record IdentityRequest(string? EmuId, string? Mac);

/// <summary>What a machine's file says, for the settings sheet.</summary>
public sealed record MachineSettings(
    int Nodes, string Processor, string ProcessorPart,
    int CpusPerNode, IReadOnlyList<int> CpusAllowed,
    int MemoryMiB, IReadOnlyList<int> MemoryAllowed,
    IReadOnlyList<Pipe> Pipes, IReadOnlyList<Drive> Drives,
    NetworkSetting Network, IReadOnlyList<Forward> Forwards, IReadOnlyList<Share> Shares,
    string ConsolePort, IReadOnlyList<ConsoleLine> ConsoleLines,
    /// <summary>The first free target on the first SCSI bus, for a disk added in Settings.</summary>
    int? DiskTarget = null,
    /// <summary>The last free target there, for a CD-ROM.</summary>
    int? CdTarget = null);

/// <summary>One edit to a stopped machine; the core refuses it with Busy while the machine runs.</summary>
public abstract record MachineChange
{
    public sealed record SetCpus(int CpusPerNode) : MachineChange;
    public sealed record SetMemory(int MiB) : MachineChange;
    /// <summary><c>Mode</c> as in <see cref="NetworkSetting"/>; the core builds the whole change from the machine's file, <c>Device</c> being the Bridged field's text.</summary>
    public sealed record SetNetwork(string Mode, string Device = "", string? ShareAddress = null, string? Subnet = null) : MachineChange;
    public sealed record AddForward(Forward Forward) : MachineChange;
    public sealed record RemoveForward(string Name) : MachineChange;
    public sealed record AddShare(Share Share) : MachineChange;
    public sealed record RemoveShare(string Name) : MachineChange;
    public sealed record DetachDrive(string Name) : MachineChange;
    /// <summary>A null port returns to the machine's default line.</summary>
    public sealed record SetConsole(string? Port) : MachineChange;
    /// <summary>A CD-ROM image in a new drive at <c>Target</c>.</summary>
    public sealed record AttachCd(string Path, int Target) : MachineChange;
    /// <summary>A new, empty system disk.</summary>
    public sealed record CreateDisk(ulong SizeMib) : MachineChange;
}

/// <summary>What the settings sheet decides itself, matching the Linux app.</summary>
public static class MachineSettingsRules
{
    /// <summary>The person's lines from what an edit returned, each with the command to copy when it names one.</summary>
    public static List<(string Text, string? Copy)> Notes(IEnumerable<ReportLine> lines) =>
        lines.Where(l => l.Shown != null).Select(l => (l.Shown!, l.Copy)).ToList();

    /// <summary>Choosing the default line clears the choice, as the CLI's console-set without --port does.</summary>
    public static MachineChange SetConsoleTo(ConsoleLine l) => new MachineChange.SetConsole(l.IsDefault ? null : l.Name);

    /// <summary>Hardware settings are locked while the machine is up.</summary>
    public static bool Editable(MachineState s) => s is MachineState.Stopped or MachineState.Exited;

    public static bool CanAddForward(string name, int hostPort, int guestPort) =>
        name.Trim().Length > 0 && hostPort is >= 1 and <= 65535 && guestPort is >= 1 and <= 65535;

    public static bool CanAddShare(string name, string dir) => name.Trim().Length > 0 && dir.Trim().Length > 0;
}
