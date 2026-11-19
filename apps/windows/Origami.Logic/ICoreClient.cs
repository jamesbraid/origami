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
    Task<string> CreateMachineAsync(string name, string offeringId, string? engine = null, CancellationToken ct = default);

    /// <summary>Reads the machine file.</summary>
    Task<MachineSettings> SettingsAsync(string key);
    /// <summary>Edits a stopped machine; throws the core's Busy error while it runs. Read the settings again afterwards.</summary>
    Task ApplyChangeAsync(string key, MachineChange change);

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
