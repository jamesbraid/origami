// The About window: the cube on its granite band, the credits and an
// acknowledgements sheet.

import OrigamiFrame
import SwiftUI

/// The letters typed in About, matched against the egg's word. Any other key starts over.
struct EggBuffer {
    static let word = Array("hinv")
    private var seen: [Character] = []

    /// True when this key completes the word.
    mutating func feed(_ key: Character) -> Bool {
        seen.append(Character(key.lowercased()))
        if seen.count > Self.word.count { seen.removeFirst(seen.count - Self.word.count) }
        return seen == Self.word
    }
}

struct AboutView: View {
    @EnvironmentObject var app: AppModel
    @State private var acknowledgements = false

    /// Reflecting a running machine's screen is off for now; the code stays for when it returns.
    static let reflectsRunningMachine = false

    /// The machine whose screen the chrome reflects: the one on show if it runs, else any that does.
    private var running: MachineModel? {
        if let c = app.current, c.state == .running, c.view?.isPresenting == true { return c }
        return app.models.first { $0.state == .running && $0.view?.isPresenting == true }
    }

    @State private var qemuVersion: String?
    @Environment(\.colorScheme) private var scheme
    static let bandHeight: CGFloat = 240
    /// The cube's view is a square 60% of the band tall; the cube fills 90% of it, so about 23% of the band is clear above and below.
    static let cubeSide: CGFloat = bandHeight * 0.6
    @State private var hinv: String?
    @State private var egg = EggBuffer()
    @State private var keys: Any?
    @State private var clicks: Any?
    @State private var cube = CubeRef()

    /// The About cube's view, so a Shift-click or the right arrow can step its scene.
    final class CubeRef { weak var view: CubeNSView? }

    private var version: String {
        let v = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
        return "Version \(v)" + (qemuVersion.map { " · QEMU \($0)" } ?? "")
    }

    var body: some View {
        VStack(spacing: 0) {
            // The band: granite across the width, the cube in its middle with a fifth of the band's height
            // clear above and below it (the cube fills 90% of its own view).
            ZStack {
                GraniteBackground(tones: scheme == .dark ? Granite.neutral : Granite.light)
                CubeHost(look: .about, machine: Self.reflectsRunningMachine ? running : nil, made: { [cube] in cube.view = $0 })
                    .frame(width: Self.cubeSide, height: Self.cubeSide)
                    .accessibilityLabel("The SGI cube")
            }
            .frame(width: 360, height: Self.bandHeight)
            ZStack {
                if let hinv {
                    ScrollView {
                        Text(hinv).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading).padding(12)
                    }
                    .background(Color(nsColor: .textBackgroundColor))
                    .transition(.move(edge: .bottom))
                    .onTapGesture { withAnimation { self.hinv = nil } }
                    .accessibilityLabel("Host inventory. Press Escape to close.")
                } else {
                    credits
                }
            }
            .frame(height: 150).clipped()
            Button("Acknowledgements…") { acknowledgements = true }.padding(.bottom, 12)
        }
        .frame(width: 360)
        .sheet(isPresented: $acknowledgements) { Acknowledgements() }
        .onAppear { startListening() }
        .onDisappear { stopListening() }
        .task {
            // The core is asked off the main actor; the line fills in when it answers.
            guard let core = app.host?.core else { return }
            qemuVersion = await Task.detached { core.engines().first(where: { $0.id == "qemu" })?.version }.value
        }
    }

    private var credits: some View {
        VStack(spacing: 6) {
            Text("Origami").font(.title.weight(.semibold))
            Text(version).font(.caption).foregroundStyle(.secondary)
            Text("Released under the BSD 3-Clause licence.").font(.caption).foregroundStyle(.secondary)
            Text("All trademarks are the property of their respective owners.")
                .font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
        }
        .padding(16)
    }

    /// While About is up, typing h-i-n-v shows the host inventory over the credits; Escape closes it.
    private func startListening() {
        guard keys == nil else { return }
        // A Shift-click on the cube steps through the scenes.
        clicks = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { e in
            guard e.window?.title == "About Origami", e.modifierFlags.contains(.shift), let v = cube.view,
                  v.bounds.contains(v.convert(e.locationInWindow, from: nil)) else { return e }
            v.stepScene()
            return nil
        }
        keys = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { e in
            guard e.window?.title == "About Origami" else { return e }
            if e.keyCode == 53, hinv != nil { withAnimation { hinv = nil }; return nil }
            // The right arrow steps through the scenes, as in the GTK About window.
            if e.keyCode == 124, e.modifierFlags.intersection([.command, .control, .option]).isEmpty { cube.view?.stepScene(); return nil }
            if let c = e.charactersIgnoringModifiers?.first, e.modifierFlags.intersection([.command, .control]).isEmpty, egg.feed(c) {
                showHinv()
            }
            return e
        }
    }

    private func stopListening() {
        if let keys { NSEvent.removeMonitor(keys) }
        keys = nil
        if let clicks { NSEvent.removeMonitor(clicks) }
        clicks = nil
    }

    private func showHinv() {
        guard let core = app.host?.core else { return }
        Task {
            // The inventory reads the disk and may open the GPU: not on the main actor.
            let text = await Task.detached { core.hinv() }.value
            withAnimation { hinv = text }
        }
    }
}

struct Acknowledgements: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Acknowledgements").font(.headline)
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(0..<origami_cube_scene_count(), id: \.self) { i in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(sceneText(i, Int32(ORIGAMI_CUBE_SCENE_TITLE))).font(.caption.weight(.medium))
                            Text(sceneText(i, Int32(ORIGAMI_CUBE_SCENE_CREDIT))).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    Text("SGI cube model from martin-steen.de. The granite tile is from the IRIX desktop.")
                        .font(.caption).foregroundStyle(.secondary)
                    Text("Machines run on QEMU.").font(.caption).foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            HStack { Spacer(); Button("Done") { dismiss() }.keyboardShortcut(.defaultAction) }
        }
        .padding(16).frame(width: 420, height: 300)
    }
}
