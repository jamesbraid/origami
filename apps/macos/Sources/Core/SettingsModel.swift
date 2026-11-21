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
    /// Advice the last edit returned, such as the lease to clear after a network move.
    @Published private(set) var notes: [OrigamiReportLine] = []
    /// The Bridged choice was made but not applied: the core needs the device with the mode.
    @Published var bridging = false
    @Published var bridgeDevice = ""
    @Published var bridgeShare = ""
    @Published var subnetText = ""
    /// A folder picked to share, waiting for the name IRIX will call it.
    struct ShareDraft: Identifiable { let dir: URL; var name: String; var id: String { dir.path } }
    @Published var draft: ShareDraft?
    let networkChoices = origamiHostNetworkModes()
    private var operation: OrigamiOperation?
    private var ticker: Timer?

    init(machine: MachineModel) { self.machine = machine }

    /// Whether edits are allowed: the machine file changes only while it is stopped.
    var locked: Bool { machine.isRunning }

    func load() {
        guard !working else { return }
        run { m in (try m.settings(), []) }
    }

    func apply(_ change: OrigamiChange) {
        guard !working else { return }
        if case .detachDrive(let name) = change { badges[name] = nil }
        if locked { error = "Power off the machine to change its settings."; return }
        run { m in let lines = try m.applyChange(change: change); return (try m.settings(), lines) }
    }

    /// Insert a CD-ROM image in a new drive at `target`, while stopped.
    func attachCD(_ url: URL, target: UInt32) {
        apply(.attachDrive(path: url.path, kind: .cdrom, target: target, overlay: false))
    }

    /// Swap or eject the CD in `drive`. Unlike the other edits it works while the machine runs.
    func changeMedia(_ drive: String, to url: URL?) {
        guard !working else { return }
        run { m in try m.changeMedia(drive: drive, image: url?.path); return (try m.settings(), []) }
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

    private static func isBridged(_ m: OrigamiNetworkMode) -> Bool { m == .tap || m == .vmnet }

    /// The mode the picker shows: the Bridged choice while its fields are open, else the file's.
    var networkMode: OrigamiNetworkMode? {
        if bridging { return networkChoices.first { Self.isBridged($0.mode) }?.mode }
        return networkChoices.first { $0.mode == currentMode }?.mode
    }

    /// The file's mode, which the core names as a string.
    private var currentMode: OrigamiNetworkMode? {
        switch settings?.network.mode {
        case "user": return .user
        case "private": return .private
        case "none": return OrigamiNetworkMode.none
        case "tap": return .tap
        case "vmnet": return .vmnet
        default: return nil
        }
    }

    private func readNetwork(_ n: OrigamiNetworkSetting) {
        bridging = networkChoices.contains { Self.isBridged($0.mode) && $0.mode == currentMode }
        bridgeDevice = n.device
        bridgeShare = n.shareAddress ?? ""
        subnetText = n.subnet
    }

    /// A picked mode. Bridged only opens its fields; choosing the file's own mode closes them.
    func chooseNetwork(_ mode: OrigamiNetworkMode) {
        bridging = Self.isBridged(mode)
        if !bridging && mode != currentMode { applyNetwork(mode, device: "", share: nil, subnet: nil) }
    }

    /// Apply Bridged with the device or socket and the shared-folder address as typed.
    func applyBridged() {
        guard let mode = networkChoices.first(where: { Self.isBridged($0.mode) })?.mode else { return }
        applyNetwork(mode, device: bridgeDevice, share: bridgeShare, subnet: nil)
    }

    /// Move the private /24 of Shared or Private.
    func applySubnet() {
        guard let mode = currentMode else { return }
        applyNetwork(mode, device: "", share: nil, subnet: subnetText)
    }

    /// The core builds the change from the machine's file; both steps run off the main actor.
    private func applyNetwork(_ mode: OrigamiNetworkMode, device: String, share: String?, subnet: String?) {
        guard !working else { return }
        if locked { error = "Power off the machine to change its settings."; return }
        run { m in
            let lines = try m.setNetwork(mode: mode, device: device, shareAddress: share, subnet: subnet)
            return (try m.settings(), lines)
        }
    }

    func chooseShareFolder(_ dir: URL) { draft = ShareDraft(dir: dir, name: origamiSuggestShareName(dir: dir.path)) }

    /// Why the name in the sheet cannot be used, in the core's words.
    var draftProblem: String? {
        guard let d = draft else { return nil }
        do { try origamiCheckShareName(name: d.name); return nil } catch { return Self.say(error) }
    }

    func addDraft() {
        guard let d = draft, draftProblem == nil else { return }
        draft = nil
        apply(.addShare(name: d.name, hostDir: d.dir.path))
    }

    /// The default line is chosen by sending none, as the CLI's console-set without --port does.
    func setConsole(_ name: String) {
        let isDefault = settings?.consolePorts.first { $0.name == name }?.isDefault ?? false
        apply(.setConsole(port: isDefault ? nil : name))
    }

    private func run(_ work: @escaping @Sendable (OrigamiMachine) throws -> (OrigamiMachineSettings, [OrigamiReportLine])) {
        let host = machine.coreHost
        let key = machine.record.key
        let open = machine.session?.machine
        working = true
        DispatchQueue.global(qos: .userInitiated).async {
            let result: Result<(OrigamiMachineSettings, [OrigamiReportLine]), Error>
            do { result = .success(try work(try open ?? host.core.openMachine(key: key))) } catch { result = .failure(error) }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.working = false
                    switch result {
                    case .success(let (s, lines)):
                        self.settings = s; self.error = nil; self.readBadges()
                        self.notes = lines.filter { $0.shown != nil }
                        self.readNetwork(s.network)
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
