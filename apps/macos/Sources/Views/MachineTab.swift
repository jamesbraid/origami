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
    @State private var dropping = false

    var body: some View {
        content
            .dropDestination(for: URL.self) { urls, _ in app.dropOnMachine(urls, model) } isTargeted: { dropping = $0 }
            .overlay { if dropping && model.isRunning { DropHint() } }
    }

    @ViewBuilder private var screen: some View {
        if model.headless || model.installConsole { ConsoleTab(model: model) } else { ScreenView(model: model) }
    }

    @ViewBuilder private var content: some View {
        switch model.state {
        case .stopped:
            if model.shown == .cantOpen { StateCard(model: model) } else { Hero(model: model) }
        case .starting: StartingCard(model: model)
        case .running: screen
        case .paused: screen.overlay { PausedOverlay(model: model) }
        case .noPicture, .stopping, .stoppedUnexpectedly, .leftRunning, .needsUpgrade: StateCard(model: model)
        }
    }
}

/// What a CD image dropped on the machine will do.
struct DropHint: View {
    var body: some View {
        RoundedRectangle(cornerRadius: 10).strokeBorder(Color.accentColor, lineWidth: 3)
            .background(Color.accentColor.opacity(0.15))
            .overlay { Text(origamiDropCaption(kind: .cdImage) ?? "").font(.title2.weight(.semibold)) }
            .padding(8).allowsHitTesting(false)
    }
}

/// The last picture dimmed, with the core's Resume.
struct PausedOverlay: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        let card = origamiMachineCard(state: .paused, error: nil, name: model.name, platform: thisPlatform)
        ZStack {
            Color.black.opacity(0.45)
            ForEach(card?.actions ?? [], id: \.label) { a in
                Button(a.label) { app.invoke(.resume) }
                    .buttonStyle(.borderedProminent).controlSize(.large).keyboardShortcut(.defaultAction)
            }
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
                    Text(origamiCaptureHint(platform: thisPlatform))
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
                Button {
                    app.invoke(.start)
                } label: {
                    Label(ui(.start), systemImage: "play.fill").font(.title3).padding(.horizontal, 22).padding(.vertical, 4)
                }
                .buttonStyle(.borderedProminent).controlSize(.large)
                .accessibilityLabel("\(ui(.start)) \(r.name)")
            }
            .padding(.vertical, 32)
            .frame(maxHeight: 300)
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
                Text(ui(.startingUpTheSystem)).font(.title3).foregroundStyle(.white)
                HStack(spacing: 28) {
                    StageChip(title: "Emulator", symbol: "cpu", state: model.emulatorUp ? true : nil)
                    StageChip(title: "Screen", symbol: "display", state: model.hasPicture ? true : nil)
                }
                Button(ui(.cancelStart)) { app.invoke(.cancelStart) }.disabled(!app.isEnabled(.cancelStart))
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

/// One card for every state that has one, worded by the core: a title, a sentence, Show details and the buttons.
struct StateCard: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel

    /// The error the page is about: why a start failed, how the emulator ended, or why the machine will not open.
    private var error: OrigamiErrorInfo? {
        switch model.shown {
        case .cantOpen:
            return model.record.problem.map { OrigamiErrorInfo(kind: .invalid, message: $0, detail: nil, fix: OrigamiErrorFix.none) }
        case .exited:
            return model.startError ?? OrigamiErrorInfo(kind: .engine, message: model.failure ?? "", detail: nil, fix: OrigamiErrorFix.none)
        default:
            return nil
        }
    }

    private var card: OrigamiCard? {
        guard var c = origamiMachineCard(state: model.shown, error: error, name: model.name, platform: thisPlatform) else { return nil }
        // An emulator that ran and then ended is not one that "couldn't start": the title is the state's word.
        if model.shown == .exited, model.startError == nil { c.title = model.word }
        return c
    }

    private var symbol: String {
        switch model.shown {
        case .noPicture: return "display"
        case .saving: return "internaldrive"
        case .leftRunning: return "bolt.horizontal.circle.fill"
        case .needsUpgrade: return "arrow.triangle.2.circlepath.circle.fill"
        default: return "exclamationmark.octagon.fill"
        }
    }

    var body: some View {
        if let card {
            VStack(spacing: 14) {
                Image(systemName: symbol).font(.system(size: 44))
                    .foregroundStyle(model.tone == .none ? Color.secondary : model.tone.colour).accessibilityHidden(true)
                if !card.title.isEmpty { Text(card.title).font(.title2.weight(.semibold)) }
                if !card.body.isEmpty {
                    Text(card.body).foregroundStyle(.secondary).multilineTextAlignment(.center).frame(maxWidth: 420)
                }
                if model.shown == .saving { ProgressView().controlSize(.small) }
                if model.shown == .needsUpgrade, let e = model.upgradeError {
                    Text(e.message).foregroundStyle(.red).multilineTextAlignment(.center).frame(maxWidth: 420).textSelection(.enabled)
                    if let d = e.detail { DetailsDisclosure(text: d) }
                }
                if let d = card.details { DetailsDisclosure(text: d) }
                HStack {
                    ForEach(Array(buttons(card).enumerated()), id: \.offset) { i, a in
                        if i == 0 {
                            Button(a.label) { perform(a) }.buttonStyle(.borderedProminent).keyboardShortcut(.defaultAction)
                        } else {
                            Button(a.label) { perform(a) }
                        }
                    }
                    if model.upgrading { ProgressView().controlSize(.small) }
                }
                .disabled(model.upgrading)
            }
            .padding(32).frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    /// The card's buttons, with the fix for a failed upgrade first.
    private func buttons(_ card: OrigamiCard) -> [OrigamiCardAction] {
        guard model.shown == .needsUpgrade, let e = model.upgradeError, e.fix != OrigamiErrorFix.none,
              let fix = origamiMachineCard(state: .stopped, error: e, name: model.name, platform: thisPlatform)?.actions.first(where: { $0.kind != .tryAgain })
        else { return card.actions }
        return [fix] + card.actions
    }

    private func perform(_ a: OrigamiCardAction) {
        switch a.kind {
        case .openConsole: app.invoke(.console)
        case .powerOff: app.invoke(.powerOff)
        case .reconnect, .tryAgain: app.invoke(.start)
        case .resume: app.invoke(.resume)
        case .upgrade: upgrade()
        case .showInFolder: app.invoke(.showInFinder)
        case .openSettings: app.settingsPane = .general; app.invoke(.settings)
        case .chooseDisk: app.settingsPane = .drives; app.invoke(.settings)
        case .chooseFirmware: chooseFirmware()
        }
    }

    private func upgrade(prom: String? = nil, ioProm: String? = nil) {
        model.upgrade(prom: prom, ioProm: ioProm) { ok in if ok { app.toast(.upgraded(name: model.name)) } }
    }

    /// The boot PROM, then the IO PROM where the machine has one; cancelling the second leaves it out.
    private func chooseFirmware() {
        guard let prom = PathPicker.choose(ui(.choose)) else { return }
        upgrade(prom: prom.path, ioProm: PathPicker.choose(ui(.choose))?.path)
    }
}

/// Show details: the technical text in monospace, with Copy.
struct DetailsDisclosure: View {
    let text: String
    @State private var open = false
    var body: some View {
        DisclosureGroup(ui(.showDetails), isExpanded: $open) {
            VStack(alignment: .leading, spacing: 6) {
                Text(text).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button(ui(.copy)) {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(text, forType: .string)
                }
            }
            .padding(.top, 4)
        }
        .frame(maxWidth: 420)
    }
}
