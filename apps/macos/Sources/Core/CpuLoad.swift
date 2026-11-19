// The CPU panel's data: the core's activity view (per-CPU host load as bars) for a running machine, polled at 2 Hz while the panel is shown.

import Foundation

@MainActor
final class CpuLoadModel: ObservableObject {
    @Published private(set) var scene: OrigamiOsview?
    private var timer: Timer?
    private var busy = false
    /// Samples taken, so a test can see that a hidden panel asks for none.
    private(set) var samples = 0
    private weak var machine: MachineModel?

    /// Start polling `machine`; with the panel hidden nothing runs.
    func start(_ machine: MachineModel) {
        stop()
        self.machine = machine
        poll()
        let t = Timer(timeInterval: 0.5, repeats: true) { [weak self] _ in MainActor.assumeIsolated { self?.poll() } }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func stop() {
        timer?.invalidate(); timer = nil
        scene = nil
    }

    /// One sample, read off the main actor; a sample still being read is not asked for twice.
    func poll() {
        guard !busy, let session = machine?.session else { return }
        busy = true
        let m = session.machine
        DispatchQueue.global(qos: .utility).async {
            let r = m.osview()
            DispatchQueue.main.async { MainActor.assumeIsolated { self.scene = r; self.busy = false; self.samples += 1 } }
        }
    }
}

/// The text shown when there are no bars: the core's reason, or that the first sample is pending.
func cpuLoadPlaceholder(_ v: OrigamiOsview?) -> String {
    v?.note ?? "measuring…"
}
