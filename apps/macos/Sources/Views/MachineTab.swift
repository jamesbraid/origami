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
        case .running, .paused: if model.headless { ConsoleTab(model: model) } else { ScreenView(model: model) }
        case .noPicture: ProblemCard(model: model)
        case .stopping: SavingCard(model: model)
        case .stoppedUnexpectedly: StoppedCard(model: model)
        case .leftRunning: LeftRunningCard(model: model)
        case .needsUpgrade: UpgradeCard(model: model)
        }
    }
}

// MARK: running

/// A machine with no display: its tab is its serial console, with the console window's own view.
struct ConsoleTab: View {
    @ObservedObject var model: MachineModel
    var body: some View {
        ConsoleHost(console: model.console())
            .accessibilityLabel("\(model.name) console")
    }
}

struct ScreenView: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    @Environment(\.displayScale) private var scale
    @State private var showHint = false

    var body: some View {
        GeometryReader { geo in
            ZStack {
                GraniteBackground()
                let guest = CGSize(width: model.picture?.width ?? 1280, height: model.picture?.height ?? 1024)
                // Zero until the machine reports its picture, so the one fit is not spent on the fallback size.
                let realGuest = model.picture.map { CGSize(width: $0.width, height: $0.height) } ?? .zero
                let inset = Scaling.inset   // granite margin and the bezel
                let avail = CGSize(width: max(80, geo.size.width - inset * 2), height: max(80, geo.size.height - inset * 2))
                let size = Scaling.viewSize(guest: guest, available: avail)
                WindowFitter(key: "\(model.id)-\(model.startedAt?.timeIntervalSince1970 ?? 0)", model: model, guest: realGuest,
                             available: geo.size, backing: scale)
                    .frame(width: 0, height: 0)
                MachineDisplay(model: model)
                    .frame(width: size.width, height: size.height)
                    .bezel()
                    .offset(Scaling.snap(container: geo.size, size: size, backing: scale))
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
            Image(systemName: symbol).font(.title2).foregroundStyle(.secondary).frame(height: 28).accessibilityHidden(true)
            Text(value).font(.headline).lineLimit(1).minimumScaleFactor(0.8)
            Text(title).font(.caption).foregroundStyle(.secondary)
        }
        .frame(width: Tokens.Size.card.width, height: Tokens.Size.card.height)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color(nsColor: .controlBackgroundColor)))
        .accessibilityElement(children: .combine)
    }
}

/// The summary pane's backdrop: the chassis colour at 35% over the window colour at the top, fading
/// linearly to the plain window colour at the bottom. No edges; the top stays dark or light enough for
/// primary text, and secondary text is drawn at 70% of the primary colour for contrast.
struct TintFade: View {
    let colour: Color
    static let topOpacity = Tokens.Pane.tintTopAlpha

    var body: some View {
        ZStack {
            Color(nsColor: .windowBackgroundColor)
            LinearGradient(stops: [.init(color: colour.opacity(Self.topOpacity), location: 0), .init(color: colour.opacity(Tokens.Pane.tintBottomAlpha), location: 1)],
                           startPoint: .top, endPoint: .bottom)
        }
    }
}

struct Hero: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel

    var body: some View {
        let r = model.record
        VStack(spacing: 0) {
            VStack(spacing: 14) {
                HeroArt(product: r.product)
                Text(r.name).font(.largeTitle.weight(.semibold))
                Text(r.product.name).font(.title3).foregroundStyle(.primary.opacity(Tokens.Pane.productLineAlpha))
                Button {
                    app.invoke(.start)
                } label: {
                    Label("Start", systemImage: "play.fill").font(.title3).padding(.horizontal, 22).padding(.vertical, 4)
                }
                .buttonStyle(.borderedProminent).controlSize(.large)
                .accessibilityLabel("Start \(r.name)")
            }
            .padding(.vertical, 32)
            .frame(maxHeight: 340)
            ForEach(model.upgradeNotes.compactMap(\.shown), id: \.self) { Text($0).font(.callout).foregroundStyle(.secondary).frame(maxWidth: 520) }
            HStack(spacing: 12) {
                ForEach(r.cards, id: \.title) { Tile(symbol: $0.symbol, title: $0.title, value: $0.value) }
            }
            .frame(maxWidth: .infinity)
            .padding(24)
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(TintFade(colour: Color(rgb: r.product.colour)))
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
            LinearGradient(colors: [Color(rgb: Granite.neutral.0), Color(rgb: Granite.neutral.1)], startPoint: .top, endPoint: .bottom)
            VStack(spacing: 18) {
                // 96 pt of cube: it fills 90% of its view, and sits on the card's gradient.
                CubeHost(look: .starting(chassis: model.record.product.colour))
                    .frame(width: 108, height: 108)
                    .accessibilityHidden(true)
                Text("Starting up the system…").font(.title3).foregroundStyle(.white)
                HStack(spacing: 28) {
                    StageChip(title: "Emulator", symbol: "cpu", state: model.emulatorUp ? true : nil)
                    StageChip(title: "Screen", symbol: "display", state: model.hasPicture ? true : nil)
                }
                Button("Power off") { app.invoke(.powerOff) }.disabled(!app.isEnabled(.powerOff))
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
                 actions: [("Open diagnostics", { app.invoke(.diagnostics) }), ("Power off", { app.invoke(.powerOff) })])
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
             actions: [("Start", { app.invoke(.start) }), ("Open diagnostics", { app.invoke(.diagnostics) })])
    }
}

struct LeftRunningCard: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        Card(symbol: "bolt.horizontal.circle.fill", tint: .orange, title: "Left running",
             detail: "\(model.name) kept running after Origami quit. Reconnect to see it, or power it off.",
             actions: [("Reconnect", { app.invoke(.start) }), ("Power off", { app.invoke(.powerOff) })])
    }
}

struct UpgradeCard: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: "arrow.triangle.2.circlepath.circle.fill").font(.system(size: 44)).foregroundStyle(.orange).accessibilityHidden(true)
            Text("Needs upgrade").font(.title2.weight(.semibold))
            Text("This machine was made by an older Origami. Upgrade it once to use it. Flash, NVRAM, drives, network settings and the Ethernet address are kept.")
                .foregroundStyle(.secondary).multilineTextAlignment(.center).frame(maxWidth: 420)
            if let e = model.upgradeError {
                Text(e)
                    .foregroundStyle(.red).multilineTextAlignment(.center).frame(maxWidth: 420).textSelection(.enabled)
            }
            HStack {
                Button { model.upgrade() } label: { Text("Upgrade") }
                    .buttonStyle(.borderedProminent).disabled(model.upgrading)
                Button("Show in Finder") { app.invoke(.showInFinder) }
                if model.upgrading { ProgressView().controlSize(.small) }
            }
        }
        .padding(32).frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
