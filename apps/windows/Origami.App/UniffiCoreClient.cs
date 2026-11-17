using Origami.Logic;
using uniffi.origami_ffi;

namespace Origami.App;

/// <summary><see cref="ICoreClient"/> over the UniFFI control plane; generated types stay in this file.</summary>
public sealed class UniffiCoreClient : ICoreClient
{
    sealed class Observer : OrigamiObserver
    {
        readonly UniffiCoreClient _o;
        public Observer(UniffiCoreClient o) { _o = o; }
        public void OnEvent(OrigamiEvent e)
        {
            var key = e.Machine ?? "";
            foreach (var k in e.Kinds)
                _o.Events?.Invoke(new MachineEvent(key, k switch
                {
                    OrigamiEventKind.State => MachineEventKind.StateChanged,
                    OrigamiEventKind.Display => MachineEventKind.DisplayChanged,
                    OrigamiEventKind.Console => MachineEventKind.ConsoleOutput,
                    OrigamiEventKind.Operation => MachineEventKind.OperationChanged,
                    _ => MachineEventKind.LibraryChanged,
                }));
        }
    }

    sealed class Frames : IFrames
    {
        IntPtr _h;
        public Frames(IntPtr machine, IntPtr wake) { _h = machine; WakeHandle = wake; }
        public IntPtr MachineHandle => _h;
        public IntPtr WakeHandle { get; }
        public void Dispose() { if (_h != IntPtr.Zero) { Frame.origami_machine_release(_h); _h = IntPtr.Zero; } }
    }

    readonly OrigamiCore _core;
    readonly Dictionary<string, OrigamiMachine> _machines = new();
    public event Action<MachineEvent>? Events;

    public UniffiCoreClient(string libraryRoot)
    {
        Directory.CreateDirectory(libraryRoot);
        _core = new OrigamiCore(libraryRoot, new Observer(this));
    }

    public string EngineIds => string.Join(",", _core.Engines().Select(e => e.Id));

#if ORIGAMI_FAKE
    string? _fakeProm; // the fake engine cannot fetch firmware, so creates hand it this file
    /// <summary>Builds with only the fake engine start with an empty library; make two machines to show.</summary>
    public void SeedFake(string scratchDir)
    {
        Directory.CreateDirectory(scratchDir);
        var prom = _fakeProm = Path.Combine(scratchDir, "prom.bin");
        File.WriteAllBytes(prom, new byte[16]);
        if (_core.ListMachines().Length > 0) return;
        foreach (var name in new[] { "Octane2 (fake)", "O2 (fake)" })
        {
            var op = _core.CreateMachine(new OrigamiCreateRequest(name, "fake.1", null, null, prom, null, null));
            if (op.Wait(30_000) is not OrigamiOperationState.Succeeded) throw new InvalidOperationException("could not create " + name);
        }
    }

    public void FakeAdvanceFrame(string key) => _core.FakeAdvanceFrame(key);
    public void FakeSay(string key, string text) => _core.FakeSay(key, text);
    /// <summary>What reached the fake engine, in order, as the core's Debug text.</summary>
    public string[] FakeInput(string key) => _core.FakeInput(key);
#else
    /// <summary>Creates a machine from the first catalogue offering whose product contains <paramref name="product"/>; the core fetches its firmware.</summary>
    public string CreateFrom(string name, string product, string? graphics = null)
    {
        var o = _core.Catalogue().First(x => x.Product.Contains(product, StringComparison.OrdinalIgnoreCase));
        var op = _core.CreateMachine(new OrigamiCreateRequest(name, o.Id, null, graphics, null, null, null));
        if (op.Wait(300_000) is not OrigamiOperationState.Succeeded) throw new InvalidOperationException("create failed: " + op.State());
        return op.ResultMachine() ?? "";
    }
#endif

    public IReadOnlyList<Offering> Catalogue() => _core.Catalogue()
        .Select(o => new Offering(o.Id, o.Engine, o.Product, o.Topology, (int)o.Nodes, (int)o.Cpus)).ToList();

    public Task<string> CreateMachineAsync(string name, string offeringId, CancellationToken ct = default) => Task.Run(() =>
    {
#if ORIGAMI_FAKE
        var prom = _fakeProm;
#else
        string? prom = null;
#endif
        var op = _core.CreateMachine(new OrigamiCreateRequest(name, offeringId, null, null, prom, null));
        var reg = ct.Register(() => op.Cancel());
        try
        {
            switch (op.Wait(null))
            {
                case OrigamiOperationState.Failed f: throw new InvalidOperationException(f.Message);
                case OrigamiOperationState.Cancelled: throw new OperationCanceledException();
            }
            return op.ResultMachine() ?? throw new InvalidOperationException("the core created the machine but gave no key");
        }
        finally { reg.Dispose(); }
    }, ct);

    public string Heads(string key) => string.Join("; ", M(key).View().Heads.Select(h => $"#{h.Index} {h.Width}x{h.Height} active={h.Active} seq={h.Seq}"));

    public void Screendump(string key, int head, string path) => M(key).Screendump((uint)head, path);

    OrigamiMachine M(string key)
    {
        lock (_machines)
        {
            if (!_machines.TryGetValue(key, out var m)) _machines[key] = m = _core.OpenMachine(key);
            return m;
        }
    }

    static MachineState Map(OrigamiMachineState s) => s switch
    {
        OrigamiMachineState.NotRunning => MachineState.Stopped,
        OrigamiMachineState.Starting => MachineState.Starting,
        OrigamiMachineState.Running => MachineState.Running,
        OrigamiMachineState.Paused => MachineState.Paused,
        OrigamiMachineState.Stopping => MachineState.Stopping,
        _ => MachineState.Exited,
    };

    // An exit the user asked for (clean stop with status 0, or a Force Quit) is Stopped; any other exit is unexpected.
    readonly HashSet<string> _forced = new();
    MachineState Settle(string key, OrigamiMachineState s)
    {
        if (s != OrigamiMachineState.Exited)
        {
            if (s == OrigamiMachineState.Starting) lock (_forced) _forced.Remove(key); // a new run forgets the old force quit
            return Map(s);
        }
        bool forced; lock (_forced) forced = _forced.Contains(key);
        return forced || M(key).View().ExitCode == 0 ? MachineState.Stopped : MachineState.Exited;
    }

    public IReadOnlyList<MachineSummary> Machines() =>
        _core.ListMachines().Select(m => new MachineSummary(m.Key, m.Name, m.Product, m.Topology, (int)m.Nodes,
            (int)m.MemoryPerNodeMib, Settle(m.Key, m.State), m.Problem)).ToList();

    public MachineStatus Status(string key)
    {
        var v = M(key).View();
        return new MachineStatus(Settle(key, v.State), v.Reason, v.LastError);
    }

    static Task Run(Func<OrigamiOperation> start, CancellationToken ct) => Task.Run(() =>
    {
        var op = start();
        var reg = ct.Register(() => op.Cancel());
        try
        {
            switch (op.Wait(null))
            {
                case OrigamiOperationState.Failed f: throw new InvalidOperationException(f.Message);
                case OrigamiOperationState.Cancelled: throw new OperationCanceledException();
            }
        }
        finally { reg.Dispose(); }
    }, ct);

    public Task StartAsync(string key, CancellationToken ct = default) => Run(() => M(key).Start(), ct);
    public Task PowerOffAsync(string key, CancellationToken ct = default) => Run(() => M(key).Stop(OrigamiStopKind.Clean), ct);
    public Task ForceQuitAsync(string key, CancellationToken ct = default)
    {
        lock (_forced) _forced.Add(key);
        return Run(() => M(key).Stop(OrigamiStopKind.Force), ct);
    }
    public Task ControlAsync(string key, MachineControl what, CancellationToken ct = default) => Task.Run(() => M(key).Control(what switch
    {
        MachineControl.Reset => OrigamiControl.Reset,
        MachineControl.PowerButton => OrigamiControl.PowerButton,
        MachineControl.Nmi => OrigamiControl.Nmi,
        MachineControl.Pause => OrigamiControl.Pause,
        _ => OrigamiControl.Resume,
    }), ct);

    public IReadOnlyList<CommandInfo> Commands() => _commands ??= _core.Commands().Select(c =>
        new CommandInfo(c.Id, c.Title, c.Shortcuts.FirstOrDefault(s => s.Platform == OrigamiPlatform.Windows)?.Accelerator)).ToList();

    IReadOnlyList<CommandInfo>? _commands;
    Dictionary<string, OrigamiEnabledWhen>? _when;

    public bool CommandEnabled(string id, MachineState? state)
    {
        _when ??= _core.Commands().ToDictionary(c => c.Id, c => c.EnabledWhen);
        if (!_when.TryGetValue(id, out var when)) return true; // a command only this app has
        OrigamiMachineState? s = state switch
        {
            null => null,
            MachineState.Stopped => OrigamiMachineState.NotRunning,
            MachineState.Starting => OrigamiMachineState.Starting,
            MachineState.Running => OrigamiMachineState.Running,
            MachineState.Paused => OrigamiMachineState.Paused,
            MachineState.Stopping => OrigamiMachineState.Stopping,
            _ => OrigamiMachineState.Exited,
        };
        return OrigamiFfiMethods.OrigamiCommandEnabled(when, s);
    }

    public DisplayInfo Display(string key, int head)
    {
        var h = M(key).View().Heads.FirstOrDefault(x => x.Index == head);
        return h is null ? new DisplayInfo(false, 0, 0, 0) : new DisplayInfo(h.Active, (int)h.Width, (int)h.Height, h.Seq);
    }

    public IFrames OpenFrames(string key)
    {
        var handle = (IntPtr)(long)M(key).FramePlaneHandle();
        var st = Frame.origami_frame_wake_handle(handle, out var wake);
        if (st != Frame.Ok) { Frame.origami_machine_release(handle); throw new InvalidOperationException("wake handle: " + Frame.LastError()); }
        return new Frames(handle, (IntPtr)wake);
    }

    /// <summary>Self-test hook: when set, every input event the app sends is kept here.</summary>
    public List<InputEvent>? InputLog { get; set; }

    public void SendInput(string key, InputEvent e)
    {
        if (InputLog is { } log) lock (log) log.Add(e);
        var m = M(key);
        try
        {
            switch (e.Kind)
            {
                case InputKind.Key:
                    var ord = GuestKeys.Ordinal(e.A);
                    if (ord >= 0) m.SendKey((OrigamiKey)ord, e.Down);
                    break;
                case InputKind.PointerMove: m.SendPointer(new OrigamiPointer.Move(e.A, e.B)); break;
                case InputKind.PointerAt: m.SendPointer(new OrigamiPointer.At(0, e.A, e.B)); break;
                case InputKind.Button:
                    m.SendPointer(new OrigamiPointer.Button((OrigamiPointerButton)e.A, e.Down)); break;
                case InputKind.Wheel: m.SendPointer(new OrigamiPointer.Wheel(e.A)); break;
            }
        }
        catch (Exception ex) when (Gone(ex)) { } // input to a stopped or disconnected machine is dropped
    }

    /// <summary>The machine is not running, or its display link is not connected: nothing to deliver to.</summary>
    static bool Gone(Exception e) =>
        e is CoreException.NotRunning || (e is CoreException.Engine en && en.message.Contains("not connected", StringComparison.OrdinalIgnoreCase));

    public int ReleaseAllKeys(string key)
    {
        // A machine that has stopped holds no keys, and a view detaches after the stop.
        try { return (int)M(key).ReleaseAllKeys(); }
        catch (Exception e) when (Gone(e)) { return 0; }
    }

    public byte[] ConsoleRead(string key, ulong from, out ulong next)
    {
        var c = M(key).ConsoleRead(from, 1 << 16);
        next = c.Next;
        return c.Bytes;
    }

    public void ConsoleSend(string key, ReadOnlySpan<byte> bytes) => M(key).ConsoleSend(bytes.ToArray());

    public void Dispose()
    {
        _core.Shutdown();
    }
}
