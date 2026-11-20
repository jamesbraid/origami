// Lists machines, creates and starts one on the fake engine, and receives an
// event. Exits non-zero on the first failure.
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading;
using uniffi.origami_ffi;

internal sealed class Recorder : OrigamiObserver
{
    private readonly object gate = new();
    private readonly List<OrigamiEvent> events = new();

    public void OnEvent(OrigamiEvent @event)
    {
        lock (gate)
        {
            events.Add(@event);
            Monitor.PulseAll(gate);
        }
    }

    public bool Wait(Func<OrigamiEvent, bool> match, int timeoutMs = 10_000)
    {
        var deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
        lock (gate)
        {
            while (!events.Any(match))
            {
                var left = deadline - DateTime.UtcNow;
                if (left <= TimeSpan.Zero || !Monitor.Wait(gate, left)) return events.Any(match);
            }
            return true;
        }
    }
}

internal static class Program
{
    private static void Expect(bool condition, string what)
    {
        if (!condition)
        {
            Console.Error.WriteLine($"expected {what}");
            Environment.Exit(1);
        }
    }

    private static int Main()
    {
        var root = Path.Combine(Path.GetTempPath(), $"origami-cs-test-{Environment.ProcessId}");
        Directory.CreateDirectory(root);
        var prom = Path.Combine(root, "prom.bin");
        File.WriteAllBytes(prom, new byte[16]);

        var recorder = new Recorder();
        var core = new OrigamiCore(root, recorder);
        Expect(core.ListMachines().Length == 0, "an empty library");
        Expect(core.Engines().First().id == "fake", "the fake engine");
        Expect(core.Commands().Any(c => c.Id == "machine.start"), "a Start command");

        var create = core.CreateMachine(new OrigamiCreateRequest("alpha", "fake.1", null, null, prom, null, null));
        Expect(create.Wait(10_000) is OrigamiOperationState.Succeeded, "the machine is created");
        var machines = core.ListMachines();
        Console.WriteLine("machines: " + string.Join(", ", machines.Select(m => m.Name)));
        Expect(machines.Length == 1 && machines[0].Name == "alpha", "alpha in the list");

        var key = machines[0].Key;
        var stopped = core.OpenMachine(key);
        stopped.ApplyChange(new OrigamiChange.SetConsole("ttyS1"));
        Expect(stopped.Settings().ConsolePort == "ttyS1", "the console line chosen");
        var machine = core.StartMachine(key);
        Expect(machine.View().State == OrigamiMachineState.Running, "a running machine");
        Expect(recorder.Wait(e => e.Machine == key && e.Kinds.Contains(OrigamiEventKind.State)), "a state event");

        core.FakeAdvanceFrame(key);
        Expect(recorder.Wait(e => e.Machine == key && e.Kinds.Contains(OrigamiEventKind.Display)), "a display event");

        try
        {
            core.OpenMachine("no-such-machine");
            Expect(false, "NotFound for an unknown machine");
        }
        catch (CoreException.NotFound)
        {
        }

        core.Shutdown();
        Console.WriteLine("ok");
        return 0;
    }
}
