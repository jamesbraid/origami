// Disk images in New Machine and Settings: choosing a file, where it lives, how a CHD keeps changes, and
// the OS badge. The core says what a file is; this holds what the person picked.

import AppKit
import Combine
import Foundation
import SwiftUI

/// The one place a file is chosen. The self-test queues answers because an open panel cannot be driven headless.
@MainActor
enum PathPicker {
    static var answers: [URL] = []

    static func choose(_ prompt: String) -> URL? {
        if !answers.isEmpty { return answers.removeFirst() }
        let panel = NSOpenPanel()
        panel.prompt = prompt
        return panel.runModal() == .OK ? panel.url : nil
    }

    static func chooseFolder(_ prompt: String) -> URL? {
        if !answers.isEmpty { return answers.removeFirst() }
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true; panel.canChooseFiles = false; panel.prompt = prompt
        return panel.runModal() == .OK ? panel.url : nil
    }
}

/// One image the person chose and the questions about it.
@MainActor
final class ImageChoice: ObservableObject {
    @Published private(set) var path: URL?
    @Published private(set) var check: OrigamiDiskImageCheck?
    /// What the disk holds, "Reading…" until the core says.
    @Published private(set) var badge = "Reading…"
    /// The core's sentence when the file could not be read at all.
    @Published private(set) var problem: String?
    @Published var copy = true
    /// A CHD keeps changes apart from the image unless told to save into it.
    @Published var apart = true
    private let core: @MainActor () -> OrigamiCore?
    private let machine: String?
    private var generation = 0

    /// `machine` is the machine the image is for, once it exists.
    init(core: @escaping @MainActor () -> OrigamiCore?, machine: String?) {
        self.core = core
        self.machine = machine
    }

    func choose() { if let url = PathPicker.choose("Choose a Disk Image") { set(url) } }

    func set(_ url: URL) {
        guard let core = core() else { return }
        path = url; check = nil; problem = nil; badge = "Reading…"
        generation += 1
        let mine = generation, machine = machine
        DispatchQueue.global(qos: .userInitiated).async {
            let result = Result { try core.checkDiskImage(path: url.path, machine: machine) }
            let os = (try? core.diskInfo(path: url.path)) ?? "Unknown"
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard mine == self.generation else { return }
                    switch result {
                    case .success(let c): self.check = c; self.badge = os
                    case .failure(let e): self.problem = SettingsModel.say(e)
                    }
                }
            }
        }
    }

    private var verdict: OrigamiImageVerdict? { check.map { origamiImageVerdict(check: $0, copy: copy) } }

    /// The line under the choices, and whether it is a warning; the core words it.
    var note: (text: String, bad: Bool)? {
        if let p = problem { return (p, true) }
        guard let v = verdict, let n = v.note else { return nil }
        return (n, v.warning)
    }

    /// Whether the image can be added as chosen.
    var ready: Bool { verdict?.ready ?? false }

    var overlay: Bool { apart && (check?.isChd ?? false) }

    /// The system disk for a new machine; nil until the image can be added.
    var system: OrigamiSystemDisk? {
        guard ready, let p = path else { return nil }
        return .image(path: p.path, copy: copy, overlay: overlay)
    }
}

/// A small grey capsule naming the OS on a disk.
struct DiskBadge: View {
    let text: String
    var body: some View {
        Text(text).font(.caption.weight(.medium))
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(Capsule().fill(Color.secondary.opacity(0.18)))
            .accessibilityLabel("Disk: \(text)")
    }
}

/// The questions about one chosen image, as New Machine and Add Disk Image both ask them.
struct ImageChoiceView: View {
    @ObservedObject var choice: ImageChoice
    private let labels = origamiDiskLabels()

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Button("Choose an Image…") { choice.choose() }
                if let p = choice.path {
                    Text(p.lastPathComponent).lineLimit(1).truncationMode(.middle)
                    if let c = choice.check { Text(origamiSizeLabel(bytes: c.bytes)).foregroundStyle(.secondary) }
                    if choice.problem == nil { DiskBadge(text: choice.badge) }
                }
            }
            if choice.check != nil {
                Picker("Where should the disk live?", selection: $choice.copy) {
                    Text(labels.place[0]).tag(true)
                    Text(labels.place[1]).tag(false)
                }.pickerStyle(.radioGroup)
                if choice.check?.isChd == true {
                    Picker("How should changes be kept?", selection: $choice.apart) {
                        Text(labels.keep[0]).tag(true)
                        Text(labels.keep[1]).tag(false)
                    }.pickerStyle(.radioGroup)
                }
            }
            if let n = choice.note {
                Text(n.text).font(.caption).foregroundStyle(n.bad ? Color.red : Color.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Text(labels.sameKind).font(.caption).foregroundStyle(.secondary)
        }
    }
}

/// A long operation's progress line and bar, with Cancel.
struct OperationProgress: View {
    let progress: (fraction: Double, text: String)
    let cancel: () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ProgressView(value: progress.fraction)
            HStack { Text(progress.text).font(.caption).foregroundStyle(.secondary); Spacer(); Button("Cancel", action: cancel) }
        }
    }
}
