namespace Origami.Logic;

/// <summary>What the Diagnostics window shows around the core's report: the machine's name and state.</summary>
public sealed record MachineDiag(string Name, MachineState State);
