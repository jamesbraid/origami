// What a machine's tab shows in each state.

import SwiftUI

/// The CAMetalLayer view, hosted. Tabs can come and go while the machine runs.
struct MachineDisplay: NSViewRepresentable {
    @ObservedObject var model: MachineModel

    final class Coordinator { weak var model: MachineModel? }
    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> MachineView {
        context.coordinator.model = model
        let view = model.makeView()
        DispatchQueue.main.async { view.window?.makeFirstResponder(view) }
        return view
    }

    func updateNSView(_ view: MachineView, context: Context) {
        view.absolutePointer = model.pointerMode == .follow
        if model.pointerMode == .follow && view.captured { view.endCapture() }
    }

    static func dismantleNSView(_ view: MachineView, coordinator: Coordinator) {
        view.endCapture()
        view.releaseAllKeys()
        view.detach()
        if coordinator.model?.view === view { coordinator.model?.view = nil }
    }
}

extension MachineModel {
    /// The display view for this machine, wired to its model: pointer mode in,
    /// capture state out. The app's views and the headless self-test both use it.
    func makeView() -> MachineView {
        let view = MachineView(session: session!)
        self.view = view
        view.onCaptureChange = { [weak self] on in self?.captureChanged(on) }
        view.absolutePointer = pointerMode == .follow
        return view
    }
}

struct MachineTab: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel

    var body: some View {
        switch model.state {
        case .stopped: Hero(model: model)
        case .starting: StartingCard(model: model)
        case .running: ScreenView(model: model)
        case .noPicture: ProblemCard(model: model)
        case .stopping: SavingCard(model: model)
        case .stoppedUnexpectedly: StoppedCard(model: model)
        case .leftRunning: LeftRunningCard(model: model)
        }
    }
}

// MARK: running

struct ScreenView: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    @Environment(\.displayScale) private var scale
    @State private var showHint = false

    var body: some View {
        GeometryReader { geo in
            ZStack {
                GraniteBackground()
                let w = Double(model.picture?.width ?? 1280), h = Double(model.picture?.height ?? 1024)
                let aspect = w / h
                let actual = app.scaleMode == .actual
                let inset = 24.0
                let fitW = min(geo.size.width - inset * 2, (geo.size.height - inset * 2) * aspect)
                let width = actual ? min(w / scale, geo.size.width - inset * 2) : max(fitW, 80)
                MachineDisplay(model: model)
                    .frame(width: width, height: width / aspect)
                    .bezel()
                    .accessibilityLabel("\(model.name) screen")
                    .accessibilityHint("The machine's display. Keyboard and mouse go to the machine.")
                if showHint {
                    Text(hint)
                        .font(.callout.weight(.medium)).foregroundStyle(.white)
                        .padding(.horizontal, 14).padding(.vertical, 8)
                        .background(Capsule().fill(.black.opacity(0.7)))
                        .frame(maxHeight: .infinity, alignment: .top).padding(.top, 14)
                        .transition(.opacity)
                        .accessibilityAddTraits(.updatesFrequently)
                }
            }
        }
        .onChange(of: model.hudUntil) { _, until in
            guard until != nil else { withAnimation { showHint = false }; return }
            withAnimation { showHint = true }
            Task { @MainActor in
                try? await Task.sleep(for: .seconds(3))
                if model.hudUntil == until { withAnimation { showHint = false } }
            }
        }
    }

    private var hint: String { "Mouse captured. Press ⌃⌥ to release." }
}

// MARK: stopped

struct Tile: View {
    let symbol: String, title: String, value: String
    var body: some View {
        VStack(spacing: 6) {
            Image(systemName: symbol).font(.title2).foregroundStyle(.secondary).accessibilityHidden(true)
            Text(value).font(.headline).lineLimit(1)
            Text(title).font(.caption).foregroundStyle(.secondary)
        }
        .frame(minWidth: 110).padding(.vertical, 14).padding(.horizontal, 10)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color(nsColor: .controlBackgroundColor)))
        .accessibilityElement(children: .combine)
    }
}

struct Hero: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel

    var body: some View {
        let r = model.record
        let colour = Color(rgb: r.product.colour)
        VStack(spacing: 0) {
            ZStack {
                LinearGradient(colors: [colour.opacity(0.55), colour.opacity(0.12)], startPoint: .top, endPoint: .bottom)
                VStack(spacing: 14) {
                    ChassisGlyph(product: r.product).scaleEffect(3.2).frame(height: 84)
                    Text(r.name).font(.largeTitle.weight(.semibold))
                    Text(r.product.name).font(.title3).foregroundStyle(.secondary)
                    Button {
                        app.invoke(.start)
                    } label: {
                        Label("Start", systemImage: "play.fill").font(.title3).padding(.horizontal, 22).padding(.vertical, 4)
                    }
                    .buttonStyle(.borderedProminent).controlSize(.large)
                    .accessibilityLabel("Start \(r.name)")
                }
                .padding(.vertical, 32)
            }
            .frame(maxHeight: 340)
            HStack(spacing: 12) {
                Tile(symbol: "memorychip", title: "Memory", value: r.memory)
                if r.nodes > 1 { Tile(symbol: "cpu", title: "Nodes", value: "\(r.nodes)") }
                if let t = r.topology { Tile(symbol: "square.stack.3d.up", title: "Topology", value: t) }
            }
            .padding(24)
            Spacer(minLength: 0)
        }
    }
}

// MARK: transitions and problems

struct StageChip: View {
    let title: String, symbol: String, state: Bool?
    var body: some View {
        VStack(spacing: 5) {
            ZStack {
                Circle().fill(.white.opacity(0.1)).frame(width: 40, height: 40)
                Image(systemName: symbol).foregroundStyle(.white.opacity(state == nil ? 0.4 : 0.95))
            }
            .overlay(alignment: .bottomTrailing) {
                if let state {
                    Image(systemName: state ? "checkmark.circle.fill" : "xmark.circle.fill")
                        .foregroundStyle(state ? Color.green : Color.red).background(Circle().fill(.black)).font(.caption)
                }
            }
            Text(title).font(.caption).foregroundStyle(.white.opacity(0.8))
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(title): \(state == nil ? "waiting" : state! ? "done" : "failed")")
    }
}

struct StartingCard: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        ZStack {
            GraniteBackground()
            VStack(spacing: 18) {
                ChromeCube().frame(width: 170, height: 150)
                Text("Starting up the system…").font(.title3).foregroundStyle(.white)
                HStack(spacing: 28) {
                    StageChip(title: "Emulator", symbol: "cpu", state: model.emulatorUp ? true : nil)
                    StageChip(title: "Screen", symbol: "display", state: model.hasPicture ? true : nil)
                }
                Button("Power Off") { app.invoke(.powerOff) }.disabled(!app.isEnabled(.powerOff))
            }
        }
    }
}

struct Card: View {
    let symbol: String, tint: Color, title: String, detail: String
    let actions: [(String, () -> Void)]
    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: symbol).font(.system(size: 44)).foregroundStyle(tint).accessibilityHidden(true)
            Text(title).font(.title2.weight(.semibold))
            Text(detail).foregroundStyle(.secondary).multilineTextAlignment(.center).frame(maxWidth: 380)
            HStack { ForEach(actions.indices, id: \.self) { i in
                if i == 0 { Button(actions[i].0, action: actions[i].1).buttonStyle(.borderedProminent) }
                else { Button(actions[i].0, action: actions[i].1) }
            } }
        }
        .padding(32).frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct ProblemCard: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        VStack(spacing: 16) {
            HStack(spacing: 26) {
                StageChip(title: "Emulator", symbol: "cpu", state: true)
                StageChip(title: "Screen", symbol: "display", state: false)
            }
            .padding(14).background(RoundedRectangle(cornerRadius: 12).fill(Color.black.opacity(0.85)))
            Card(symbol: "exclamationmark.triangle.fill", tint: .red, title: "No picture",
                 detail: "The emulator is running but has sent no picture. Diagnostics shows what the display link reports.",
                 actions: [("Open Diagnostics", { app.invoke(.diagnostics) }), ("Power Off", { app.invoke(.powerOff) })])
        }
    }
}

struct SavingCard: View {
    @ObservedObject var model: MachineModel
    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: "internaldrive").font(.system(size: 44)).foregroundStyle(.secondary)
            Text("Saving").font(.title2.weight(.semibold))
            ProgressView().controlSize(.small)
            Text("The disk image is folding in the guest's changes.").foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct StoppedCard: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        Card(symbol: "exclamationmark.octagon.fill", tint: .red, title: "Stopped unexpectedly",
             detail: model.failure ?? "The emulator exited.",
             actions: [("Start", { app.invoke(.start) }), ("Open Diagnostics", { app.invoke(.diagnostics) })])
    }
}

struct LeftRunningCard: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        Card(symbol: "bolt.horizontal.circle.fill", tint: .orange, title: "Left running",
             detail: "\(model.name) kept running after Origami quit. Reconnect to see it, or power it off.",
             actions: [("Reconnect", { app.invoke(.start) }), ("Power Off", { app.invoke(.powerOff) })])
    }
}
