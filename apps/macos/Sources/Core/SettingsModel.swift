// A machine's settings, read from and edited through the core. Every core call runs off the main actor.

import Foundation

@MainActor
final class SettingsModel: ObservableObject {
    let machine: MachineModel
    @Published private(set) var settings: OrigamiMachineSettings?
    /// What went wrong, in the core's words, shown under the pane.
    @Published private(set) var error: String?
    @Published private(set) var working = false

    init(machine: MachineModel) { self.machine = machine }

    /// Whether edits are allowed: the machine file changes only while it is stopped.
    var locked: Bool { machine.isRunning }

    func load() {
        guard !working else { return }
        run { m in try m.settings() }
    }

    func apply(_ change: OrigamiChange) {
        guard !working else { return }
        if locked { error = "Power off the machine to change its settings."; return }
        run { m in try m.applyChange(change: change); return try m.settings() }
    }

    /// The default line is chosen by sending none, as the CLI's console-set without --port does.
    func setConsole(_ name: String) {
        let isDefault = settings?.consolePorts.first { $0.name == name }?.isDefault ?? false
        apply(.setConsole(port: isDefault ? nil : name))
    }

    private func run(_ work: @escaping @Sendable (OrigamiMachine) throws -> OrigamiMachineSettings) {
        let host = machine.coreHost
        let key = machine.record.key
        let open = machine.session?.machine
        working = true
        DispatchQueue.global(qos: .userInitiated).async {
            let result: Result<OrigamiMachineSettings, Error>
            do { result = .success(try work(try open ?? host.core.openMachine(key: key))) } catch { result = .failure(error) }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.working = false
                    switch result {
                    case .success(let s): self.settings = s; self.error = nil
                    case .failure(let e): self.error = Self.say(e)
                    }
                }
            }
        }
    }

    /// The core's sentence, with Busy told as what to do about it.
    nonisolated static func say(_ e: Error) -> String {
        if let c = e as? CoreError, case .Busy = c { return "Power off the machine to change its settings." }
        if let c = e as? CoreError {
            switch c {
            case .Invalid(let m, _), .NotFound(let m, _), .Unsupported(let m, _), .Io(let m, _), .Engine(let m, _), .Internal(let m, _),
                 .Busy(let m, _), .NotRunning(let m, _), .NeedsUpgrade(let m, _), .Protocol(let m, _), .Cancelled(let m, _): return m
            }
        }
        return "\(e)"
    }
}
