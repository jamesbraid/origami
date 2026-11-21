// A machine's settings, read from and edited through the core. Every core call runs off the main actor.

import Foundation

@MainActor
final class SettingsModel: ObservableObject {
    let machine: MachineModel
    @Published private(set) var settings: OrigamiMachineSettings?
    /// What went wrong, in the core's words, shown under the pane.
    @Published private(set) var error: String?
    @Published private(set) var working = false
    /// What each disk drive holds by drive name, read in the background; absent until read.
    @Published private(set) var badges: [String: String] = [:]
    /// The copy's progress, while one runs.
    @Published private(set) var progress: (fraction: Double, text: String)?
    private var operation: OrigamiOperation?
    private var ticker: Timer?

    init(machine: MachineModel) { self.machine = machine }

    /// Whether edits are allowed: the machine file changes only while it is stopped.
    var locked: Bool { machine.isRunning }

    func load() {
        guard !working else { return }
        run { m in try m.settings() }
    }

    func apply(_ change: OrigamiChange) {
        guard !working else { return }
        if case .detachDrive(let name) = change { badges[name] = nil }
        if locked { error = "Power off the machine to change its settings."; return }
        run { m in try m.applyChange(change: change); return try m.settings() }
    }

    /// Insert a CD-ROM image in a new drive at `target`, while stopped.
    func attachCD(_ url: URL, target: UInt32) {
        apply(.attachDrive(path: url.path, kind: .cdrom, target: target, overlay: false))
    }

    /// Swap or eject the CD in `drive`. Unlike the other edits it works while the machine runs.
    func changeMedia(_ drive: String, to url: URL?) {
        guard !working else { return }
        run { m in try m.changeMedia(drive: drive, image: url?.path); return try m.settings() }
    }

    /// Add the chosen image as a disk at `target`; a copy shows progress and can be cancelled.
    func addDisk(_ choice: ImageChoice, target: UInt32, done: @escaping @MainActor (Bool) -> Void) {
        guard !working, let path = choice.path else { return }
        if locked { error = "Power off the machine to change its settings."; return }
        working = true
        let (copy, overlay) = (choice.copy, choice.overlay)
        let host = machine.coreHost, key = machine.record.key
        DispatchQueue.global(qos: .userInitiated).async {
            var failure: String?
            do {
                let op = try host.core.openMachine(key: key).importDrive(path: path.path, kind: .disk, target: target, overlay: overlay, copy: copy)
                DispatchQueue.main.async { MainActor.assumeIsolated { self.watch(op) } }
                if case .failed(_, let message) = op.wait(timeoutMs: nil) { failure = message }
            } catch { failure = Self.say(error) }
            let settings = failure == nil ? (try? host.core.openMachine(key: key).settings()) : nil
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.ticker?.invalidate(); self.progress = nil; self.operation = nil
                    self.working = false
                    if let failure { self.error = failure; done(false); return }
                    if let settings { self.badges["disk\(target)"] = nil; self.settings = settings; self.error = nil; self.readBadges() }
                    done(true)
                }
            }
        }
    }

    func cancelCopy() { operation?.cancel() }

    private func watch(_ op: OrigamiOperation) {
        operation = op
        ticker = Timer.scheduledTimer(withTimeInterval: 0.2, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                if let f = op.fraction() { self?.progress = (Double(f), op.progress()) }
            }
        }
    }

    /// Name the OS on each disk drive not read yet. Off the main actor; a running machine's disks are not read.
    func readBadges() {
        guard let drives = settings?.drives else { return }
        let host = machine.coreHost, key = machine.record.key
        for d in drives where d.kind == "disk" && badges[d.name] == nil {
            let name = d.name
            DispatchQueue.global(qos: .utility).async {
                // An empty label hides the badge: the disk could not be read.
                let label = (try? host.core.openMachine(key: key).driveInfo(drive: name)) ?? ""
                DispatchQueue.main.async { MainActor.assumeIsolated { self.badges[name] = label } }
            }
        }
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
                    case .success(let s): self.settings = s; self.error = nil; self.readBadges()
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
