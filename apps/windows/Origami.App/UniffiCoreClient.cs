using Origami.Logic;
using uniffi.origami_ffi;

namespace Origami.App;

/// <summary>The core is already doing something to this machine (for example a stop in progress).</summary>
public sealed class CoreBusyException : Exception { public CoreBusyException(string m) : base(m) { } }

/// <summary><see cref="ICoreClient"/> over the UniFFI control plane; generated types stay in this file.</summary>
public sealed class UniffiCoreClient : ICoreClient
{
    sealed class Observer : OrigamiObserver
    {
        readonly UniffiCoreClient _o;
        public Observer(UniffiCoreClient o) { _o = o; }
        public void OnEvent(OrigamiEvent e)
        {
            _o.Poke();
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

    // The core's reads (a machine's view asks QEMU over QMP) can block, so nothing on the UI thread calls them: a
    // background loop reads every machine and the UI reads what it stored.
    sealed record Snapshot(IReadOnlyList<OrigamiMachineSummary> List, Dictionary<string, OrigamiMachineView> Views,
        Dictionary<string, OrigamiShownState> Shown, Dictionary<string, BarWords> Bars);
    /// <summary>The status bar's words for a running machine, one pointer line for each mode, read with the view.</summary>
    sealed record BarWords(string? Picture, string? Follow, string? Capture);
    volatile Snapshot _snap = new(Array.Empty<OrigamiMachineSummary>(), new(), new(), new());
    readonly HashSet<string> _installing = new();
    readonly SemaphoreSlim _wake = new(0, 1); // one pending wake is enough: the pass reads everything
    readonly ManualResetEventSlim _first = new(false);
    readonly CancellationTokenSource _stop = new();
    int _tick, _relist;

    public UniffiCoreClient(string libraryRoot)
    {
        Directory.CreateDirectory(libraryRoot);
        _core = new OrigamiCore(libraryRoot, new Observer(this));
        _ = Task.Run(Loop);
        _first.Wait(TimeSpan.FromSeconds(3)); // bounded: the window opens empty and fills in if the first read is slower
    }

    async Task Loop()
    {
        try { ReadCore(true); } catch (Exception e) { App.Log("first core read failed: " + e.Message); }
        _first.Set();
        while (!_stop.IsCancellationRequested)
        {
            try { await _wake.WaitAsync(TimeSpan.FromMilliseconds(750), _stop.Token); } catch (OperationCanceledException) { return; }
            try { ReadCore(++_tick % 4 == 0 | Interlocked.Exchange(ref _relist, 0) == 1); } catch (Exception e) { App.Log("core read failed: " + e.Message); }
        }
    }

    /// <summary>One pass over the core, on the loop's thread. The library list (a disk read) is refreshed every fourth pass or on request.</summary>
    void ReadCore(bool relist)
    {
        var list = relist ? _core.ListMachines() : _snap.List.ToArray();
        var views = new Dictionary<string, OrigamiMachineView>();
        var shown = new Dictionary<string, OrigamiShownState>();
        var bars = new Dictionary<string, BarWords>();
        foreach (var m in list)
        {
            try
            {
                var machine = M(m.Key);
                var view = machine.View();
                views[m.Key] = view;
                shown[m.Key] = machine.ShownState(IsInstalling(m.Key));
                if (view.State is OrigamiMachineState.Running or OrigamiMachineState.Paused)
                {
                    var follow = machine.StatusBar(true, false, OrigamiPlatform.Windows);
                    var capture = machine.StatusBar(false, true, OrigamiPlatform.Windows);
                    bars[m.Key] = new BarWords(follow?.Picture, follow?.Pointer, capture?.Pointer);
                }
            }
            catch (Exception e) when (e is CoreException) { /* a machine that went away has no view this pass */ }
        }
        var before = _snap.List.Select(m => m.Key).ToList();
        _snap = new Snapshot(list, views, shown, bars);
        if (!before.SequenceEqual(list.Select(m => m.Key))) Events?.Invoke(new MachineEvent("", MachineEventKind.LibraryChanged));
    }

    /// <summary>The generated core and machine objects, for the install assistant, which drives the core's own flow.</summary>
    internal OrigamiCore Raw => _core;
    internal OrigamiMachine Open(string key) => M(key);

    public string LibraryRoot => _core.LibraryRoot();

    public void SetLibraryRoot(string root)
    {
        Directory.CreateDirectory(root);
        _relist = 1; // the core's library event wakes the loop, which then lists the new folder
        _core.SetLibraryRoot(root);
    }

    /// <summary>Ask the loop to read the core now; a core event calls it. A create reads at once itself.</summary>
    void Poke() { try { _wake.Release(); } catch (SemaphoreFullException) { } }

    /// <summary>A machine's stored view, or null when it has none yet (it is new, or its view failed on the last pass).</summary>
    OrigamiMachineView? TryView(string key) => _snap.Views.GetValueOrDefault(key);

    public string EngineName(string id) => _core.Engines().FirstOrDefault(e => e.Id == id)?.Name ?? id;
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
            var op = _core.CreateMachine(new OrigamiCreateRequest(name, "fake.1", null, null, prom, null, null, null, null));
            if (op.Wait(30_000) is not OrigamiOperationState.Succeeded) throw new InvalidOperationException("could not create " + name);
        }
        ReadCore(true);
    }

    /// <summary>The folders the core has moved to its stand-in Recycle Bin, in order.</summary>
    public string[] FakeTrashed() => _core.FakeTrashed();
    public void FakeAdvanceFrame(string key) => _core.FakeAdvanceFrame(key);
    public void FakeSay(string key, string text) => _core.FakeSay(key, text);
    /// <summary>What reached the fake engine, in order, as the core's Debug text.</summary>
    public string[] FakeInput(string key) => _core.FakeInput(key);
    /// <summary>The CD changes that reached the fake engine, in order, as "drive image" or "drive eject".</summary>
    public string[] FakeMedia(string key) => _core.FakeMedia(key);
#else
    /// <summary>Creates a machine from the first catalogue offering whose product contains <paramref name="product"/>; the core fetches its firmware.</summary>
    public string CreateFrom(string name, string product, string? graphics = null)
    {
        var o = _core.Catalogue().First(x => x.Product.Contains(product, StringComparison.OrdinalIgnoreCase));
        var op = _core.CreateMachine(new OrigamiCreateRequest(name, o.Id, null, graphics, null, null, null, null, null));
        if (op.Wait(300_000) is not OrigamiOperationState.Succeeded) throw new InvalidOperationException("create failed: " + op.State());
        var made = op.ResultMachine() ?? "";
        ReadCore(true);
        return made;
    }
#endif

    public IReadOnlyList<Offering> Catalogue() => _core.Catalogue()
        .Select(o => new Offering(o.Id, o.Engine, o.Product, o.Topology, (int)o.Nodes, (int)o.Cpus, o.RunsIrix, o.Label)).ToList();

    public Task<string> CreateMachineAsync(string name, string offeringId, string? engine = null, CancellationToken ct = default, SystemDisk? disk = null, IProgress<CopyProgress>? progress = null, IdentityRequest? identity = null) => Task.Run(() =>
    {
#if ORIGAMI_FAKE
        var prom = _fakeProm;
#else
        string? prom = null;
#endif
        OrigamiSystemDisk? system = disk switch
        {
            SystemDisk.Empty e => new OrigamiSystemDisk.Empty(e.Gb),
            SystemDisk.Image i => new OrigamiSystemDisk.Image(i.Path, i.Copy, i.Overlay),
            SystemDisk.Ready r => new OrigamiSystemDisk.Ready(r.Name),
            _ => null,
        };
        var op = _core.CreateMachine(new OrigamiCreateRequest(name, offeringId, null, null, prom, null, engine, system, identity is null ? null : new OrigamiIdentityRequest(identity.EmuId, identity.Mac)));
        var reg = ct.Register(() => op.Cancel());
        try
        {
            Watch(op, progress);
            switch (op.Wait(null))
            {
                case OrigamiOperationState.Failed f: throw new InvalidOperationException(f.Message);
                // A cancelled copy leaves the machine without a disk, so the library still has it; a cancelled download made nothing.
                case OrigamiOperationState.Cancelled when op.ResultMachine() is not null:
                    ReadCore(true);
                    return "";
                case OrigamiOperationState.Cancelled: throw new OperationCanceledException();
            }
            var made = op.ResultMachine() ?? throw new InvalidOperationException("the core created the machine but gave no key");
            ReadCore(true); // show it at once
            return made;
        }
        finally { reg.Dispose(); }
    }, ct);

    /// <summary>Reports a long operation's progress about five times a second until it ends.</summary>
    static void Watch(OrigamiOperation op, IProgress<CopyProgress>? progress)
    {
        if (progress is null) return;
        _ = Task.Run(async () =>
        {
            while (op.State() is OrigamiOperationState.Running)
            {
                if (op.Fraction() is { } f) progress.Report(new CopyProgress(f, op.Progress()));
                await Task.Delay(200);
            }
        });
    }

    public Task<DiskImageCheck> CheckDiskImageAsync(string path, string? machineKey) => Task.Run(() =>
    {
        var c = _core.CheckDiskImage(path, machineKey);
        return new DiskImageCheck(c.Bytes, c.IsChd, c.FreeBytes, c.UsedBy, c.Problem);
    });

    public Task<IReadOnlyList<ReadyDisk>> ReadyDisksAsync(string offeringId) => Task.Run(() =>
        (IReadOnlyList<ReadyDisk>)_core.ReadyDisks(offeringId).Select(d => new ReadyDisk(d.Name, d.Version, d.Bytes, d.Cached, d.Label)).ToList());

    public IReadOnlyList<CachedDisk> CachedDisks() =>
        _core.CachedDisks().Select(d => new CachedDisk(d.Key, d.Label, d.Bytes)).ToList();

    public void RemoveCachedDisk(string key) => _core.RemoveCachedDisk(key);

    public Task<string?> DiskOsAsync(string path) => Task.Run(() => _core.DiskOs(path));
    public Task<string?> DriveOsAsync(string key, string drive) => Task.Run(() => M(key).DriveOs(drive));
    public Task<string> StartupValueAsync(string key) => Task.Run(() => OrigamiFfiMethods.OrigamiStartupValue(M(key).BootConsole()));
    public Task<IReadOnlyList<DriveRow>> DriveRowsAsync(string key) => Task.Run<IReadOnlyList<DriveRow>>(() => M(key).DriveRows().Select(r => new DriveRow(r.Name, r.Title, r.Caption)).ToList());
    public ImageVerdict Verdict(DiskImageCheck c, bool copy)
    {
        var v = OrigamiFfiMethods.OrigamiImageVerdict(new OrigamiDiskImageCheck(c.Bytes, c.IsChd, c.FreeBytes, c.UsedBy, c.Problem), copy);
        return new ImageVerdict(v.Note, v.Warning, v.Ready);
    }

    public DiskLabels Labels()
    {
        var l = OrigamiFfiMethods.OrigamiDiskLabels();
        return new DiskLabels(l.Choices, l.Place, l.Keep, l.SameKind, l.Ready, l.Release, l.DefaultGb);
    }

    public string SizeLabel(ulong bytes) => OrigamiFfiMethods.OrigamiSizeLabel(bytes);

    public Task ImportDriveAsync(string key, string path, int target, bool overlay, bool copy, IProgress<CopyProgress>? progress = null, CancellationToken ct = default) =>
        Run(() =>
        {
            var op = M(key).ImportDrive(path, OrigamiDriveKind.Disk, (uint)target, overlay, copy);
            Watch(op, progress);
            return op;
        }, ct);

    public Task ChangeMediaAsync(string key, string drive, string? image) => Task.Run(() =>
    {
        M(key).ChangeMedia(drive, image);
        ReadCore(true); // a stopped machine's file changed
    });

    public Task<MachineSettings> SettingsAsync(string key) => Task.Run(() =>
    {
        var s = M(key).Settings();
        return new MachineSettings((int)s.Nodes, s.Processor, s.ProcessorPart,
            (int)s.CpusPerNode, s.CpusPerNodeAllowed.Select(x => (int)x).ToList(),
            (int)s.MemoryPerNodeMib, s.MemoryAllowedMib.Select(x => (int)x).ToList(),
            s.Pipes.Select(p => new Pipe(p.Slot, p.Product, p.Family)).ToList(),
            s.Drives.Select(d => new Drive(d.Name, d.Kind, (int)d.Bus, (int)d.Target, d.Image, d.ReadOnly)).ToList(),
            new NetworkSetting(s.Network.Mode, s.Network.Device, s.Network.ShareAddress, s.Network.Subnet, s.Network.Mac, s.Network.MacNote, s.Network.Warnings.ToList()),
            s.Forwards.Select(f => new Forward(f.Name, f.Protocol, f.HostPort, f.GuestPort)).ToList(),
            s.Shares.Select(x => new Share(x.Name, x.HostDir, x.MountLines.ToList(), x.MountProblem, x.NeedsAddress)).ToList(),
            s.ConsolePort, s.ConsolePorts.Select(c => new ConsoleLine(c.Name, c.Label, c.IsDefault)).ToList(),
            (int?)s.DiskTarget, (int?)s.CdTarget);
    });

    static string ModeName(OrigamiNetworkMode m) => m switch
    {
        OrigamiNetworkMode.User => "user", OrigamiNetworkMode.Private => "private", OrigamiNetworkMode.None => "none",
        OrigamiNetworkMode.Tap => "tap", _ => "vmnet",
    };

    static OrigamiNetworkMode ModeOf(string name) => name switch
    {
        "user" => OrigamiNetworkMode.User, "private" => OrigamiNetworkMode.Private, "none" => OrigamiNetworkMode.None,
        "tap" => OrigamiNetworkMode.Tap, "vmnet" => OrigamiNetworkMode.Vmnet,
        _ => throw new ArgumentException("unknown network mode " + name),
    };

    public IReadOnlyList<NetworkChoice> NetworkModes() => OrigamiFfiMethods.OrigamiHostNetworkModes().Select(c => new NetworkChoice(ModeName(c.Mode), c.Label, c.Detail)).ToList();

    public BridgedSetup HostBridgedSetup()
    {
        var b = OrigamiFfiMethods.OrigamiBridgedSetup();
        return new BridgedSetup(b.Field, b.FieldNote, b.Example, b.ShareExample, b.ShareNote, b.Steps, b.Commands);
    }

    public int DefaultOffering(IReadOnlyList<Offering> offerings) => (int)(OrigamiFfiMethods.OrigamiDefaultOffering(offerings.Select(o => new OrigamiOfferingKey(o.Id, o.Product)).ToArray()) ?? 0);
    public string SuggestShareName(string dir) => OrigamiFfiMethods.OrigamiSuggestShareName(dir);

    public string? CheckShareName(string name)
    {
        try { OrigamiFfiMethods.OrigamiCheckShareName(name); return null; }
        catch (Exception e) { return e.Message; }
    }

    public Task<IdentityPreview> PreviewIdentityAsync(string offeringId, string? engine, string emuId, string? mac) => Task.Run(() =>
    {
        var p = _core.PreviewNewIdentity(offeringId, engine, emuId, mac);
        return new IdentityPreview(p.EmuId, p.Mac, p.FromId, p.Problem, p.ProblemField is { } f ? Enum.Parse<IdentityField>(f.ToString()) : null);
    });

    public Task<IReadOnlyList<ReportLine>> ApplyChangeAsync(string key, MachineChange change) => Task.Run<IReadOnlyList<ReportLine>>(() => (change is MachineChange.SetNetwork n
        ? M(key).SetNetwork(ModeOf(n.Mode), n.Device, n.ShareAddress, n.Subnet)
        : M(key).ApplyChange(change switch
    {
        MachineChange.SetCpus c => new OrigamiChange.SetProcessor((uint)c.CpusPerNode),
        MachineChange.SetMemory c => new OrigamiChange.SetMemory((uint)c.MiB),
        MachineChange.AddForward c => new OrigamiChange.AddForward(new OrigamiForward(c.Forward.Name, c.Forward.Protocol, (ushort)c.Forward.HostPort, (ushort)c.Forward.GuestPort)),
        MachineChange.RemoveForward c => new OrigamiChange.RemoveForward(c.Name),
        MachineChange.AddShare c => new OrigamiChange.AddShare(c.Share.Name, c.Share.HostDir),
        MachineChange.RemoveShare c => new OrigamiChange.RemoveShare(c.Name),
        MachineChange.DetachDrive c => new OrigamiChange.DetachDrive(c.Name),
        MachineChange.SetConsole c => new OrigamiChange.SetConsole(c.Port),
        MachineChange.AttachCd c => new OrigamiChange.AttachDrive(c.Path, OrigamiDriveKind.Cdrom, (uint)c.Target, false),
        MachineChange.CreateDisk c => new OrigamiChange.CreateDisk(c.SizeMib),
        _ => throw new ArgumentException("unknown change"),
    })).Select(l => new ReportLine(Enum.Parse<LineKind>(l.Kind.ToString()), l.Text, l.Shown, l.Copy)).ToList());

    public string Heads(string key) => string.Join("; ", (TryView(key)?.Heads ?? Array.Empty<OrigamiHeadView>()).Select(h => $"#{h.Index} {h.Width}x{h.Height} active={h.Active} seq={h.Seq}"));

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
        OrigamiMachineState.LeftRunning => MachineState.LeftRunning, // a runner from an earlier session is still up: not an exit, so not Settle's business
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
        return forced || TryView(key)?.ExitCode == 0 ? MachineState.Stopped : MachineState.Exited;
    }

    public IReadOnlyList<MachineSummary> Machines()
    {
        var snap = _snap;
        return snap.List.Select(m =>
        {
            var has = snap.Views.TryGetValue(m.Key, out var v);
            var state = has ? v!.State : m.State;
            // The file says whether it has a graphics head; a head the engine reports counts too.
            var display = m.HasDisplay || (has && v!.Heads.Length > 0);
            return new MachineSummary(m.Key, m.Name, m.Product, m.ProductLabel, m.Topology, (int)m.Nodes, (int)m.MemoryPerNodeMib,
                m.NeedsUpgrade ? MachineState.NeedsUpgrade : Settle(m.Key, state), m.Problem, display, m.NeedsUpgrade, m.SystemDisk, m.RunsIrix,
                m.Cards.Select(c => new SummaryCard(c.Title, c.Value)).ToList());
        }).ToList();
    }

    public MachineStatus Status(string key)
    {
        if (TryView(key) is not { } v)
        {
            var listed = _snap.List.FirstOrDefault(m => m.Key == key);
            if (listed is { NeedsUpgrade: true }) return new MachineStatus(MachineState.NeedsUpgrade, "", null);
            return new MachineStatus(listed is null ? MachineState.Stopped : Settle(key, listed.State), "No information about this machine yet.", null);
        }
        return new MachineStatus(Settle(key, v.State), v.Reason, v.LastError, v.Note);
    }

    /// <summary>Runs a core operation to its end. With <paramref name="ignoreEnded"/>, a machine that is already not running is not an error.</summary>
    static Task Run(Func<OrigamiOperation> start, CancellationToken ct, bool ignoreEnded = false) => Task.Run(() =>
    {
        OrigamiOperation op;
        try { op = start(); }
        catch (CoreException.NotRunning) when (ignoreEnded) { return; }
        catch (CoreException.Busy b) { throw new CoreBusyException(b.message); }
        var reg = ct.Register(() => op.Cancel());
        try
        {
            switch (op.Wait(null))
            {
                case OrigamiOperationState.Failed { Kind: OrigamiErrorKind.NotRunning } when ignoreEnded: break;
                case OrigamiOperationState.Failed { Kind: OrigamiErrorKind.Busy } b: throw new CoreBusyException(b.Message);
                case OrigamiOperationState.Failed f: throw new InvalidOperationException(f.Message);
                case OrigamiOperationState.Cancelled: throw new OperationCanceledException();
            }
        }
        finally { reg.Dispose(); }
    }, ct);

    public Task<IReadOnlyList<ReportLine>> UpgradeAsync(string key) => Task.Run<IReadOnlyList<ReportLine>>(() =>
    {
        try { return Lines(_core.UpgradeMachine(key)); }
        finally { ReadCore(true); } // the list says at once whether the machine still needs it
    });

    public Task<IReadOnlyList<ReportLine>> UpgradeWithAsync(string key, string? prom, string? ioProm) => Task.Run<IReadOnlyList<ReportLine>>(() =>
    {
        try { return Lines(_core.UpgradeMachineWith(key, prom, ioProm)); }
        finally { ReadCore(true); }
    });

    static List<ReportLine> Lines(IEnumerable<OrigamiReportLine> lines) =>
        lines.Select(l => new ReportLine(Enum.Parse<LineKind>(l.Kind.ToString()), l.Text, l.Shown, l.Copy)).ToList();

    public Task<IReadOnlyList<string>> RemovePlanAsync(string key) => Task.Run<IReadOnlyList<string>>(() => _core.RemovePlan(key).DiskLeft.ToList());

    public Task<IReadOnlyList<string>> RemoveAsync(string key) => Task.Run<IReadOnlyList<string>>(() =>
    {
        try
        {
            var removed = _core.RemoveMachine(key);
            lock (_machines) _machines.Remove(key);
            return removed.DiskLeft.ToList();
        }
        catch (CoreException.Busy b) { throw new CoreBusyException(b.message); }
        finally { ReadCore(true); }
    });

    // ---- what the core says about a machine's state ----

    static ShownState ShownOf(OrigamiShownState s) => Enum.Parse<ShownState>(s.ToString());
    static Tone ToneOf(OrigamiTone t) => Enum.Parse<Tone>(t.ToString());

    bool IsInstalling(string key) { lock (_installing) return _installing.Contains(key); }

    public void SetInstalling(string key, bool on)
    {
        bool changed;
        lock (_installing) changed = on ? _installing.Add(key) : _installing.Remove(key);
        if (changed) Poke();
    }

    /// <summary>A machine that needs an upgrade or cannot be read is worded from the library alone: it has no view to ask.</summary>
    static bool Unreadable(OrigamiMachineSummary m) => m.NeedsUpgrade || m.Problem is not null;

    public ShownState Shown(string key)
    {
        var snap = _snap;
        var m = snap.List.FirstOrDefault(x => x.Key == key);
        if (m is null) return ShownState.Stopped;
        if (!Unreadable(m) && snap.Shown.TryGetValue(key, out var s)) return ShownOf(s);
        return ShownOf(OrigamiFfiMethods.OrigamiRow(m, IsInstalling(key)).State);
    }

    public RowInfo Row(string key)
    {
        var snap = _snap;
        var m = snap.List.FirstOrDefault(x => x.Key == key);
        if (m is null) return new RowInfo(ShownState.Stopped, "", Tone.None, "", Tone.None, "");
        var live = !Unreadable(m) && snap.Views.TryGetValue(key, out var v) ? m with { State = v.State } : m;
        var r = OrigamiFfiMethods.OrigamiRow(live, IsInstalling(key));
        if (!IsInstalling(key) && !Unreadable(m) && snap.Shown.TryGetValue(key, out var shown) && shown != r.State)
        {
            // What only a view knows (a blank screen, a clean exit) changes the word and the second line.
            var model = OrigamiFfiMethods.OrigamiRow(live with { State = OrigamiMachineState.NotRunning }, false).Line.Text;
            var label = OrigamiFfiMethods.OrigamiStateLabel(shown);
            var line = OrigamiFfiMethods.OrigamiLibraryLineFor(shown, model);
            return new RowInfo(ShownOf(shown), label.Word, ToneOf(label.Tone), line.Text, ToneOf(line.Tone),
                OrigamiFfiMethods.OrigamiRowDescriptionFor(shown, m.Name, model, m.SystemDisk));
        }
        return new RowInfo(ShownOf(r.State), r.Word, ToneOf(r.Tone), r.Line.Text, ToneOf(r.Line.Tone), r.Description);
    }

    public CardInfo? Card(string key, string name, ShownState state)
    {
        var m = _snap.List.FirstOrDefault(x => x.Key == key);
        var error = state == ShownState.CantOpen && m?.Problem is { } p
            ? new OrigamiErrorInfo(OrigamiErrorKind.Invalid, p, null, OrigamiErrorFix.None)
            : TryView(key)?.StartError;
        var c = OrigamiFfiMethods.OrigamiMachineCard(Enum.Parse<OrigamiShownState>(state.ToString()), error, name, OrigamiPlatform.Windows);
        return c is null ? null : new CardInfo(c.Title, c.Body,
            c.Actions.Select(a => new CardAction(Enum.Parse<CardActionKind>(a.Kind.ToString()), a.Label)).ToList(), c.Details);
    }

    public Problem ProblemOf(Exception e)
    {
        if (e is not CoreException ce) return new Problem(e.Message, null, ErrorFix.None);
        var i = OrigamiFfiMethods.OrigamiErrorInfo(ce);
        return new Problem(i.Message, i.Detail, Enum.Parse<ErrorFix>(i.Fix.ToString()));
    }

    public string? FixLabel(Problem p)
    {
        var error = new OrigamiErrorInfo(OrigamiErrorKind.Invalid, p.Message, p.Detail, Enum.Parse<OrigamiErrorFix>(p.Fix.ToString()));
        var card = OrigamiFfiMethods.OrigamiMachineCard(OrigamiShownState.Stopped, error, "", OrigamiPlatform.Windows);
        // The card offers the fix first and Try again last.
        return card?.Actions.FirstOrDefault(a => a.Kind != OrigamiActionKind.TryAgain)?.Label;
    }

    public StatusBarInfo? StatusBar(string key, bool follows, bool captured) =>
        _snap.Bars.TryGetValue(key, out var b) ? new StatusBarInfo(b.Picture, follows ? b.Follow : captured ? b.Capture : null) : null;

    /// <summary>Asks the loop to list the library again now.</summary>
    public void Relist() { _relist = 1; Poke(); }

    public Task StartAsync(string key, CancellationToken ct = default) => Run(() => M(key).Start(), ct);
    public Task PowerOffAsync(string key, CancellationToken ct = default) => Run(() => M(key).Stop(OrigamiStopKind.Clean), ct, ignoreEnded: true);
    public Task ForceQuitAsync(string key, CancellationToken ct = default)
    {
        lock (_forced) _forced.Add(key);
        return Run(() => M(key).Stop(OrigamiStopKind.Force), ct, ignoreEnded: true);
    }
    public Task ControlAsync(string key, MachineControl what, CancellationToken ct = default) => Task.Run(() =>
    {
        M(key).Control(what switch
        {
            MachineControl.Reset => OrigamiControl.Reset,
            MachineControl.PowerButton => OrigamiControl.PowerButton,
            MachineControl.Nmi => OrigamiControl.Nmi,
            MachineControl.Pause => OrigamiControl.Pause,
            _ => OrigamiControl.Resume,
        });
        ReadCore(false); // the pause/resume toggle reads this state; the 750 ms poll would leave it stale
    }, ct);

    public IReadOnlyList<CommandInfo> Commands() => _commands ??= _core.Commands().Select(c =>
        new CommandInfo(c.Id, c.Title, c.Shortcuts.FirstOrDefault(s => s.Platform == OrigamiPlatform.Windows)?.Accelerator)).ToList();

    IReadOnlyList<CommandInfo>? _commands;
    Dictionary<string, OrigamiEnabledWhen>? _when;

    public bool CommandEnabled(string id, MachineState? state)
    {
        _when ??= _core.Commands().ToDictionary(c => c.Id, c => c.EnabledWhen);
        if (state == MachineState.NeedsUpgrade) return !id.StartsWith("machine."); // nothing of the machine works until it is upgraded
        if (!_when.TryGetValue(id, out var when)) return true; // a command only this app has
        OrigamiMachineState? s = state switch
        {
            null => null,
            MachineState.Stopped => OrigamiMachineState.NotRunning,
            MachineState.Starting => OrigamiMachineState.Starting,
            MachineState.Running => OrigamiMachineState.Running,
            MachineState.Paused => OrigamiMachineState.Paused,
            MachineState.Stopping => OrigamiMachineState.Stopping,
            MachineState.LeftRunning => OrigamiMachineState.LeftRunning,
            _ => OrigamiMachineState.Exited,
        };
        return OrigamiFfiMethods.OrigamiCommandEnabled(when, s);
    }

    public DisplayInfo Display(string key, int head)
    {
        var h = TryView(key)?.Heads.FirstOrDefault(x => x.Index == head);
        return h is null ? new DisplayInfo(false, 0, 0, 0) : new DisplayInfo(h.Active, (int)h.Width, (int)h.Height, h.Seq);
    }

    public MachineDiag Diagnostics(string key)
    {
        if (TryView(key) is not { } v)
        {
            var listed = _snap.List.FirstOrDefault(m => m.Key == key);
            return new MachineDiag(listed?.Name ?? key, listed is null ? MachineState.Stopped : Settle(key, listed.State));
        }
        return new MachineDiag(v.Name, Settle(key, v.State));
    }

    public string DiagnosticsReport(string key, MachineState state, long updates, long shown) =>
        TryView(key) is null ? "No information about this machine yet." : M(key).DiagnosticsReport(Row(key).Word, (ulong)updates, (ulong)shown);

    public Task<OsviewScene> OsviewAsync(string key) => Task.Run(() =>
    {
        var r = M(key).Osview();
        return new OsviewScene(
            r.Bars.Select(b => new OsviewBar(b.Title,
                b.Legend.Select(k => new OsviewKey(k.Label, k.Rgb)).ToList(),
                b.Segments.Select(s => new OsviewSegment(s.Fraction, s.Rgb)).ToList())).ToList(),
            r.Ticks, r.Background, r.BarBackground, r.Caption, r.Note);
    });

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

    sealed class Term : ITerminal
    {
        readonly OrigamiTerminal _t;
        public Term(OrigamiTerminal t) { _t = t; }

        static TermColour Col(OrigamiTerminalColour c) => c switch
        {
            OrigamiTerminalColour.Indexed i => TermColour.Indexed(i.Index),
            OrigamiTerminalColour.Rgb r => TermColour.Rgb(r.R, r.G, r.B),
            _ => TermColour.Default,
        };

        static TermCell[] Cells(OrigamiTerminalCell[] cs) =>
            cs.Select(c => new TermCell(c.Text, Col(c.Fg), Col(c.Bg), c.Bold, c.Dim, c.Italic, c.Underline, c.Inverse, c.Wide, c.Continuation)).ToArray();

        public (int Rows, int Cols) Size { get { var s = _t.Size(); return (s.Rows, s.Cols); } }
        public void Resize(int rows, int cols) => _t.Resize((ushort)rows, (ushort)cols);
        public TermCell[] Row(int index) => Cells(_t.Row((ushort)index));
        public (int Row, int Col, bool Visible) Cursor { get { var c = _t.Cursor(); return (c.Row, c.Col, c.Visible); } }
        public int ScrollbackCount => (int)_t.ScrollbackCount();
        public TermCell[]? ScrollbackRow(int index) => _t.ScrollbackRow((uint)index) is { } r ? Cells(r) : null;
        public void Scroll(int lines) => _t.Scroll(lines);
        public int ScrollOffset => (int)_t.ScrollOffset();
        public TermDamage ChangedSince(ulong since) { var d = _t.ChangedSince(since); return new TermDamage(d.Sequence, d.Full, d.Rows.Select(r => (int)r).ToList()); }

        public void SendKey(TermKey key, bool shift, bool ctrl, bool alt) => _t.SendKeyWith(key.Kind switch
        {
            TermKeyKind.Text => new OrigamiTerminalKey.Text(key.Text),
            TermKeyKind.Enter => new OrigamiTerminalKey.Enter(),
            TermKeyKind.Backspace => new OrigamiTerminalKey.Backspace(),
            TermKeyKind.Tab => new OrigamiTerminalKey.Tab(),
            TermKeyKind.BackTab => new OrigamiTerminalKey.BackTab(),
            TermKeyKind.Escape => new OrigamiTerminalKey.Escape(),
            TermKeyKind.Up => new OrigamiTerminalKey.Up(),
            TermKeyKind.Down => new OrigamiTerminalKey.Down(),
            TermKeyKind.Left => new OrigamiTerminalKey.Left(),
            TermKeyKind.Right => new OrigamiTerminalKey.Right(),
            TermKeyKind.Home => new OrigamiTerminalKey.Home(),
            TermKeyKind.End => new OrigamiTerminalKey.End(),
            TermKeyKind.PageUp => new OrigamiTerminalKey.PageUp(),
            TermKeyKind.PageDown => new OrigamiTerminalKey.PageDown(),
            TermKeyKind.Insert => new OrigamiTerminalKey.Insert(),
            TermKeyKind.Delete => new OrigamiTerminalKey.Delete(),
            _ => new OrigamiTerminalKey.Function((byte)key.Number),
        }, shift, ctrl, alt);

        public void Paste(string text) => _t.Paste(text);
        public void Dispose() => _t.Dispose();
    }

    public ITerminal OpenTerminal(string key, int rows, int cols) => new Term(new OrigamiTerminal(M(key), (ushort)rows, (ushort)cols));

    public byte[] ConsoleRead(string key, ulong from, out ulong next)
    {
        var c = M(key).ConsoleRead(from, 1 << 16);
        next = c.Next;
        return c.Bytes;
    }

    public void ConsoleSend(string key, ReadOnlySpan<byte> bytes) => M(key).ConsoleSend(bytes.ToArray());

    public string Hinv() => _core.Hinv();

    public void Dispose()
    {
        _stop.Cancel();
        _core.Shutdown();
    }
}
