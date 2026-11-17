// The New Machine sheet's logic, apart from its views so the self-test can drive it.

import Foundation

@MainActor
final class NewMachineModel: ObservableObject {
    @Published private(set) var offerings: [OrigamiOffering] = []
    @Published var selected = 0
    @Published var name = ""
    @Published private(set) var error: String?
    @Published private(set) var creating = false
    @Published private(set) var loading = true
    private let app: AppModel
    /// A firmware file for engines that need one named; the sheet leaves it unset.
    var bootProm: String?

    init(app: AppModel) { self.app = app }

    var trimmedName: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }
    var canCreate: Bool { !trimmedName.isEmpty && !creating && offerings.indices.contains(selected) }

    func label(_ o: OrigamiOffering) -> String { "\(o.id) (\(o.cpus) CPU\(o.cpus == 1 ? "" : "s"))" }

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
                        self.selected = o.firstIndex { $0.id == "octane2-vpro" } ?? 0
                    case .failure(let e): self.error = "The machines could not be listed: \(e)"
                    }
                }
            }
        }
    }

    /// Create the machine; `done(true)` once it exists and is open, `done(false)` with `error` set.
    func create(done: @escaping @MainActor (Bool) -> Void) {
        guard canCreate, let core = app.host?.core else { return }
        let request = OrigamiCreateRequest(name: trimmedName, offering: offerings[selected].id, memoryPerNodeMib: nil,
                                           graphics: nil, bootProm: bootProm, ioProm: nil, engine: nil)
        creating = true; error = nil
        DispatchQueue.global(qos: .userInitiated).async {
            var key: String?
            var failure: String?
            do {
                let op = try core.createMachine(request: request)
                switch op.wait(timeoutMs: nil) {
                case .succeeded:
                    key = op.resultMachine()
                    if key == nil { failure = "The machine was created but could not be opened. Look for it in the library." }
                case .failed(_, let message): failure = message
                default: failure = "The machine was not created."
                }
            } catch { failure = "\(error)" }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.creating = false
                    if let failure { self.error = failure; done(false); return }
                    self.app.rescanNow()   // the new machine must be listed before its tab opens
                    if let key { self.app.openInNewTab(key) }
                    done(true)
                }
            }
        }
    }
}
