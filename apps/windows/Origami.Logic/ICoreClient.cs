namespace Origami.Logic;

/// <summary>A machine's frame plane handles, for the view and the wake-up. Owned by the client.</summary>
public interface IFrames : IDisposable
{
    /// <summary>The core's <c>origami_machine*</c>, as an integer.</summary>
    IntPtr MachineHandle { get; }
    /// <summary>An auto-reset event signalled while a new frame waits; wait on it, never close it.</summary>
    IntPtr WakeHandle { get; }
}

/// <summary>What the app needs from the core; <c>UniffiCoreClient</c> implements it.</summary>
public interface ICoreClient : IDisposable
{
    IReadOnlyList<MachineSummary> Machines();
    string LibraryRoot { get; }
    /// <summary>Lists another folder now. Running machines keep running from their own folders.</summary>
    void SetLibraryRoot(string root);
    MachineStatus Status(string key);

    /// <summary>What can be created. The first call may start an engine's program: call it off the UI thread.</summary>
    IReadOnlyList<Offering> Catalogue();
    /// <summary>Creates a machine from an offering and returns its key; the core fetches any firmware. <paramref name="engine"/> null takes the first engine that offers it.</summary>
    /// <param name="disk">The system disk to start with; a copy reports <paramref name="progress"/> and stops when <paramref name="ct"/> is cancelled, leaving a machine with no disk.</param>
    Task<string> CreateMachineAsync(string name, string offeringId, string? engine = null, CancellationToken ct = default, SystemDisk? disk = null, IProgress<CopyProgress>? progress = null, IdentityRequest? identity = null);
    /// <summary>The ID and Ethernet address a new machine would get with the choices so far; reads the library folder, so call off the UI thread.</summary>
    Task<IdentityPreview> PreviewIdentityAsync(string offeringId, string? engine, string emuId, string? mac);

    /// <summary>Whether a file can be attached and what a copy needs. <paramref name="machineKey"/> is the machine it is for; without it the library folder stands in.</summary>
    Task<DiskImageCheck> CheckDiskImageAsync(string path, string? machineKey);
    /// <summary>The ready-made disks that fit an offering. Asks the disk server, or uses its last list: off the UI thread.</summary>
    Task<IReadOnlyList<ReadyDisk>> ReadyDisksAsync(string offeringId);
    /// <summary>The disks downloaded so far.</summary>
    IReadOnlyList<CachedDisk> CachedDisks();
    void RemoveCachedDisk(string key);
    /// <summary>The OS and release on a disk image, such as "IRIX 6.5.30", or null when it is not recognised.</summary>
    Task<string?> DiskOsAsync(string path);
    /// <summary>The same for the disk in a machine's drive; the core refuses a running machine it has not read before.</summary>
    Task<string?> DriveOsAsync(string key, string drive);
    /// <summary>Each drive's title and caption for the Drives page, in the core's words.</summary>
    Task<IReadOnlyList<DriveRow>> DriveRowsAsync(string key);
    /// <summary>Where the PROM prints, as the core words it after "Startup messages on": "Screen" or "Console".</summary>
    Task<string> StartupValueAsync(string key);
    /// <summary>What to say about a checked image and whether it can be added, given whether it is copied in.</summary>
    ImageVerdict Verdict(DiskImageCheck check, bool copy);
    /// <summary>The disk choices and how an image is kept, in the core's words.</summary>
    DiskLabels Labels();
    /// <summary>The engine as a person knows it, such as "QEMU (Origami)".</summary>
    string EngineName(string id);
    /// <summary>A byte count as every app shows it, such as "3.3 GB".</summary>
    string SizeLabel(ulong bytes);
    /// <summary>Adds a disk image to a stopped machine, used where it is or copied into its folder.</summary>
    Task ImportDriveAsync(string key, string path, int target, bool overlay, bool copy, IProgress<CopyProgress>? progress = null, CancellationToken ct = default);
    /// <summary>Puts <paramref name="image"/> in a CD-ROM drive, or ejects with null. A running machine changes at once; a stopped one's file changes.</summary>
    Task ChangeMediaAsync(string key, string drive, string? image);

    /// <summary>Reads the machine file.</summary>
    Task<MachineSettings> SettingsAsync(string key);
    /// <summary>Edits a stopped machine; throws the core's Busy error while it runs. Read the settings again afterwards. The lines carry advice to show.</summary>
    Task<IReadOnlyList<ReportLine>> ApplyChangeAsync(string key, MachineChange change);
    /// <summary>The modes this computer can run, in picker order.</summary>
    IReadOnlyList<NetworkChoice> NetworkModes();
    BridgedSetup HostBridgedSetup();
    /// <summary>The index in <paramref name="offerings"/> of the model New Machine selects, 0 when empty.</summary>
    int DefaultOffering(IReadOnlyList<Offering> offerings);
    /// <summary>The share name to offer for a host folder: its own name, made valid.</summary>
    string SuggestShareName(string dir);
    /// <summary>Why the guest cannot use <paramref name="name"/> as a share name, in the core's words, or null.</summary>
    string? CheckShareName(string name);

    /// <summary>Upgrades a stopped machine made by an older Origami, off the UI thread; the core's sentence is the exception message.</summary>
    Task<IReadOnlyList<ReportLine>> UpgradeAsync(string key);
    /// <summary>The same with firmware files the person chose instead of a download.</summary>
    Task<IReadOnlyList<ReportLine>> UpgradeWithAsync(string key, string? prom, string? ioProm);
    /// <summary>The disk images a removal would leave where they are, asked before the alert.</summary>
    Task<IReadOnlyList<string>> RemovePlanAsync(string key);
    /// <summary>Moves a stopped machine to the Recycle Bin; the core refuses a running one. Returns the disk images that stay where they are.</summary>
    Task<IReadOnlyList<string>> RemoveAsync(string key);

    // ---- what the core says about a machine's state ----
    /// <summary>The state a machine shows, from the last pass over the core: reading it never waits on the machine.</summary>
    ShownState Shown(string key);
    /// <summary>A library row: the state word, the second line and the tooltip.</summary>
    RowInfo Row(string key);
    /// <summary>The card for a machine in <paramref name="state"/>, or for why its last start failed; null where the state has none.</summary>
    CardInfo? Card(string key, string name, ShownState state);
    /// <summary>The bar under the picture; null unless the machine runs.</summary>
    StatusBarInfo? StatusBar(string key, bool follows, bool captured);
    /// <summary>What went wrong, from any exception a core call threw.</summary>
    Problem ProblemOf(Exception e);
    /// <summary>The label of the button that offers <paramref name="p"/>'s fix, such as "Choose firmware files…"; null when it has none.</summary>
    string? FixLabel(Problem p);
    /// <summary>Tells the core a machine's install guide is on show, so it words the machine Installing.</summary>
    void SetInstalling(string key, bool on);

    Task StartAsync(string key, CancellationToken ct = default);
    /// <summary>Clean stop. Can take minutes while a disk folds.</summary>
    Task PowerOffAsync(string key, CancellationToken ct = default);
    Task ForceQuitAsync(string key, CancellationToken ct = default);
    Task ControlAsync(string key, MachineControl what, CancellationToken ct = default);
    /// <summary>Writes the head's picture as a PPM file.</summary>
    void Screendump(string key, int head, string path);

    /// <summary>The core's command table with Windows shortcuts.</summary>
    IReadOnlyList<CommandInfo> Commands();
    /// <summary>Whether a command is available when the selected machine is in <paramref name="state"/> (null: none selected).</summary>
    bool CommandEnabled(string id, MachineState? state);

    DisplayInfo Display(string key, int head);
    MachineDiag Diagnostics(string key);
    /// <summary>The Diagnostics report, worded by the core and the same in every app.</summary>
    string DiagnosticsReport(string key, MachineState state, long updates, long shown);
    /// <summary>The activity view for the latest emulated-CPU load,  (the core glides it from the last one); the core samples while this keeps being called, so call it only while the panel is shown.</summary>
    Task<OsviewScene> OsviewAsync(string key);
    IFrames OpenFrames(string key);

    /// <summary>Never blocks.</summary>
    void SendInput(string key, InputEvent e);
    /// <summary>Lifts every key and button the guest thinks is down; returns how many.</summary>
    int ReleaseAllKeys(string key);

    /// <summary>The console as a terminal grid, fed by the core; dispose it when its window closes.</summary>
    ITerminal OpenTerminal(string key, int rows, int cols);

    byte[] ConsoleRead(string key, ulong from, out ulong next);
    void ConsoleSend(string key, ReadOnlySpan<byte> bytes);

    /// <summary>An IRIX-style inventory of this host, for About's egg. Reads the disk and may open the GPU: call off the UI thread.</summary>
    string Hinv();

    /// <summary>Raised on a core thread. Handlers must hop to the UI thread.</summary>
    event Action<MachineEvent>? Events;
}
