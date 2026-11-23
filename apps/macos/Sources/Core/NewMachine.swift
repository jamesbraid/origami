// The New Machine sheet's logic, apart from its views so the self-test can drive it.

import Combine
import Foundation

/// The system disk New Machine offers. Install from discs joins this list later.
enum DiskChoice: Int, CaseIterable, Identifiable {
    case noDisk, empty, image
    /// Offered only while the server has a disk for the model.
    case ready
    var id: Int { rawValue }
    /// The core words the choices, in this order, and the ready-made one after them.
    var title: String { let l = origamiDiskLabels(); return self == .ready ? l.ready : l.choices[rawValue] }
}

@MainActor
final class NewMachineModel: ObservableObject {
    @Published private(set) var offerings: [OrigamiOffering] = []
    @Published var selected = 0 { didSet { loadReady(); refreshIdentity() } }
    /// The engine the machine will run on. It is always explicit; the catalogue's first engine until the person picks.
    @Published var engine: String? {
        didSet {
            guard offerings.indices.contains(selected), offerings[selected].engine != engine else { return }
            // Keep the model when the new engine offers it; otherwise take that engine's first.
            let id = offerings[selected].id
            selected = offerings.firstIndex { $0.engine == engine && $0.id == id } ?? offerings.firstIndex { $0.engine == engine } ?? selected
        }
    }
    @Published var name = ""
    /// Advanced: the machine ID, prefilled with the next free one, and an Ethernet address to use instead of the one it makes.
    @Published var idText = "" { didSet { if !writing { refreshIdentity() } } }
    @Published var useAddress = false { didSet { refreshIdentity() } }
    @Published var addressText = "" { didSet { refreshIdentity() } }
    @Published private(set) var identity: OrigamiIdentityPreview?
    @Published private(set) var previewError: String?
    private var writing = false
    private var identityRound = 0
    @Published var disk = DiskChoice.noDisk
    /// The empty disk's size in GB.
    @Published var sizeGB = Int(origamiDiskLabels().defaultGb)
    /// The ready-made disks that fit the model, and the one picked.
    @Published private(set) var readyDisks: [OrigamiReadyDisk] = []
    @Published var readyPick = 0
    private var asked = 0
    /// Open the Install IRIX assistant on the new machine; offered only with an empty disk.
    @Published var installAfter = false
    var offersInstall: Bool { disk == .empty && offerings.indices.contains(selected) && offerings[selected].runsIrix }
    /// The copy's progress, while one runs.
    @Published private(set) var progress: (fraction: Double, text: String)?
    let image: ImageChoice
    private var imageChanges: AnyCancellable?
    private var operation: OrigamiOperation?
    private var ticker: Timer?
    @Published private(set) var error: String?
    @Published private(set) var creating = false
    @Published private(set) var loading = true
    private let app: AppModel
    /// A firmware file for engines that need one named; the sheet leaves it unset.
    var bootProm: String?

    init(app: AppModel) {
        self.app = app
        image = ImageChoice(core: { app.host?.core }, machine: nil)
        // The Create button depends on the image, which is its own object.
        imageChanges = image.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() }
    }

    /// The engines the catalogue offers machines on, in the catalogue's order.
    var engines: [String] { offerings.reduce(into: [String]()) { if !$0.contains($1.engine) { $0.append($1.engine) } } }

    /// The offerings the picked engine can run, as indexes into `offerings`.
    var visible: [Int] { offerings.indices.filter { engine == nil || offerings[$0].engine == engine } }

    /// More than one engine to choose between; with one the picker is shown but cannot be changed.
    var canPickEngine: Bool { engines.count > 1 }

    var choices: [DiskChoice] { readyDisks.isEmpty ? DiskChoice.allCases.filter { $0 != .ready } : DiskChoice.allCases }

    /// Ask the server for the disks that fit the model, off the main thread; a later model choice wins.
    private func loadReady() {
        asked += 1
        let turn = asked
        guard let core = app.host?.core, offerings.indices.contains(selected) else { return }
        let id = offerings[selected].id
        DispatchQueue.global(qos: .userInitiated).async {
            let found = (try? core.readyDisks(offering: id)) ?? []
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard turn == self.asked else { return }
                    self.readyDisks = found; self.readyPick = 0
                    if found.isEmpty && self.disk == .ready { self.disk = .noDisk }
                }
            }
        }
    }

    var trimmedName: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }
    var canCreate: Bool {
        !trimmedName.isEmpty && !creating && offerings.indices.contains(selected) && (disk != .image || image.ready) && (disk != .ready || readyDisks.indices.contains(readyPick)) && identityProblem == nil
    }

    /// Why the ID or address cannot be used, in the core's words.
    var identityProblem: String? { identity?.problem ?? previewError }

    /// The address row: the one the ID makes, or the typed one, or that the machine sets its own.
    var addressShown: String {
        guard let identity else { return "" }
        return identity.mac.isEmpty ? "Set by the machine" : identity.mac
    }

    /// Ask the core what the choices so far give. It reads the library folder, so off the main thread.
    func refreshIdentity() {
        guard let core = app.host?.core, offerings.indices.contains(selected) else { return }
        identityRound += 1
        let round = identityRound, offering = offerings[selected].id, chosen = self.engine ?? offerings[selected].engine
        let typed = idText, mac = useAddress && !addressText.isEmpty ? addressText : nil
        DispatchQueue.global(qos: .userInitiated).async {
            let result = Result { try core.previewNewIdentity(offering: offering, engine: chosen, emuId: typed, mac: mac) }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard round == self.identityRound else { return }
                    switch result {
                    case .success(let p):
                        self.identity = p; self.previewError = nil
                        // The next free ID fills an empty field, never one the person has started typing.
                        if typed.trimmingCharacters(in: .whitespaces).isEmpty, self.idText.trimmingCharacters(in: .whitespaces).isEmpty {
                            self.writing = true; self.idText = String(p.emuId); self.writing = false
                        }
                        if p.mac.isEmpty, !self.useAddress { self.useAddress = true }
                    case .failure(let e): self.previewError = SettingsModel.say(e)
                    }
                }
            }
        }
    }

    /// Stops a download or a copy.
    func cancelCopy() { operation?.cancel() }

    /// Show the operation's progress until it ends.
    private func watch(_ op: OrigamiOperation) {
        operation = op
        ticker = Timer.scheduledTimer(withTimeInterval: 0.2, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                if let f = op.fraction() { self?.progress = (Double(f), op.progress()) }
            }
        }
    }

    func label(_ o: OrigamiOffering) -> String { o.label }

    /// The engine as a person knows it, from the core.
    func engineName(_ id: String) -> String { app.host?.core.engines().first { $0.id == id }?.name ?? id }

    /// The catalogue's first call may start the engine's program, so it runs off the main thread.
    func load() {
        guard let core = app.host?.core else { error = app.launchError ?? "No core."; loading = false; return }
        DispatchQueue.global(qos: .userInitiated).async {
            let result = Result { try core.catalogue() }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.loading = false
                    switch result {
                    case .success(let o):
                        self.offerings = o
                        self.selected = Int(origamiDefaultOffering(offerings: o.map { OrigamiOfferingKey(id: $0.id, product: $0.product) }) ?? 0)
                        self.engine = o.indices.contains(self.selected) ? o[self.selected].engine : nil
                    case .failure(let e): self.error = "The machines could not be listed: \(e)"
                    }
                }
            }
        }
    }

    private var identityRequest: OrigamiIdentityRequest {
        OrigamiIdentityRequest(emuId: idText, mac: useAddress && !addressText.isEmpty ? addressText : nil)
    }

    /// Create the machine; `done(true)` once it exists and is open, `done(false)` with `error` set.
    func create(done: @escaping @MainActor (Bool) -> Void) {
        guard canCreate, let core = app.host?.core else { return }
        let systemDisk: OrigamiSystemDisk?
        switch disk {
        case .noDisk: systemDisk = nil
        case .empty: systemDisk = .empty(gb: UInt32(max(1, sizeGB)))
        case .image: systemDisk = image.system
        case .ready: systemDisk = .ready(name: readyDisks[readyPick].name)
        }
        let request = OrigamiCreateRequest(name: trimmedName, offering: offerings[selected].id, memoryPerNodeMib: nil,
                                           graphics: nil, bootProm: bootProm, ioProm: nil, engine: engine ?? offerings[selected].engine,
                                           systemDisk: systemDisk, identity: identityRequest)
        creating = true; error = nil
        DispatchQueue.global(qos: .userInitiated).async {
            var key: String?
            var failure: String?
            var nothingMade = false
            do {
                let op = try core.createMachine(request: request)
                DispatchQueue.main.async { MainActor.assumeIsolated { self.watch(op) } }
                switch op.wait(timeoutMs: nil) {
                case .succeeded:
                    key = op.resultMachine()
                    if key == nil { failure = "The machine was created but could not be opened. Look for it in the library." }
                case .failed(_, let message): failure = message
                // A cancelled copy leaves the machine without a disk; a cancelled download made nothing.
                case .cancelled: nothingMade = op.resultMachine() == nil
                default: failure = "The machine was not created."
                }
            } catch { failure = "\(error)" }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.creating = false
                    self.ticker?.invalidate(); self.progress = nil; self.operation = nil
                    if let failure { self.error = failure; done(false); return }
                    if nothingMade { done(false); return }
                    self.app.rescanNow()   // the new machine must be listed before its tab opens
                    if let key { self.app.openInNewTab(key) }
                    if self.offersInstall && self.installAfter, let key { self.app.installIrix(key) }
                    done(true)
                }
            }
        }
    }
}
