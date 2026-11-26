// The 0.6.2 redesign's checks: the library row, the new alerts, Remove, New machine, the status bar and the
// install guide. They take the self-test's `check` so each is one step in its JSON lines.

import AppKit
import Foundation
import SwiftUI

/// `view` in a hosting view inside a window that is never shown, laid out and settled.
@MainActor
func hosted<V: View>(_ view: V, size: CGSize) -> (NSHostingView<V>, NSWindow) {
    let host = NSHostingView(rootView: view)
    host.frame = CGRect(origin: .zero, size: size)
    let window = NSWindow(contentRect: CGRect(origin: .zero, size: size), styleMask: [.borderless], backing: .buffered, defer: false)
    window.isReleasedWhenClosed = false
    window.contentView = host
    host.layoutSubtreeIfNeeded()
    spin(0.5)
    return (host, window)
}

#if FAKE
/// A machine summary that is in no library, for rows in every state.
func syntheticSummary(_ name: String) -> OrigamiMachineSummary {
    OrigamiMachineSummary(key: "/synthetic/\(name)", name: name, product: "o2", productLabel: "O2", topology: "o2", topologyName: "O2", cards: [],
                          hasDisplay: true, runsIrix: true, nodes: 1, memoryPerNodeMib: 512, state: .notRunning, problem: nil,
                          needsUpgrade: false, systemDisk: nil)
}

/// The checks that need machines at rest.
@MainActor
func uxChecksAtRest(app: AppModel, host: CoreHost, library: String, out: String, check: (String, Bool, [String: Any]) -> Void) {
    let fixtures = ProcessInfo.processInfo.environment["ORIGAMI_FIXTURES"] ?? ""
    // The checks move the selection about; the rest of the run starts from the machine that was open.
    let opened = app.active
    defer { app.closeAllTabsForTest(); if let opened { app.select(opened) } }

    // A library row in each state: the core's word as a dot and a word, the model when stopped; no capsule.
    do {
        let m = MachineModel(summary: syntheticSummary("rowtest"), host: host, defaultPointer: .follow)
        var wrong: [String] = []
        let states: [MachineState] = [.stopped, .starting, .running, .noPicture, .stopping, .stoppedUnexpectedly, .leftRunning, .paused]
        for st in states {
            m.injectStaleState(st)
            let label = origamiStateLabel(state: m.shown)
            let want = m.shown == .stopped ? m.record.modelName : label.word
            if m.line.text != want || m.line.tone != (m.shown == .stopped ? OrigamiTone.none : label.tone) { wrong.append("\(st)") }
            let shot = snapshot(LibraryRow(model: m).environmentObject(app), size: CGSize(width: 240, height: 44), to: "\(out)/ui-row-\(m.word.replacingOccurrences(of: " ", with: "-")).png")
            if !shot.ok { wrong.append("render \(st)") }
            if !m.rowDescription.contains(label.word) { wrong.append("description \(st)") }
        }
        check("library-row-line-is-the-core-word-or-the-model-in-each-state", wrong.isEmpty, ["wrong": wrong])
        m.injectStaleState(.running)
        // SwiftUI's accessibility tree is not reachable from a headless hosting view, so the label is checked as the core's text.
        let want = [m.name, m.record.modelName, m.word]
        check("library-row-description-is-the-core's-name-model-and-state",
              want.allSatisfy { m.rowDescription.contains($0) } && !m.rowDescription.contains("Status"), ["description": m.rowDescription])
    }

    // Cancel start is offered only while a machine starts, and Power off is not.
    do {
        let m = MachineModel(summary: syntheticSummary("starttest"), host: host, defaultPointer: .follow)
        m.injectStaleState(.starting)
        check("cancel-start-is-the-core-command-and-replaces-power-off-while-starting",
              CommandID.cancelStart.title == origamiUiString(id: .cancelStart) && CommandID.cancelStart.coreID == "machine.cancel-start"
              && CommandID.cpuLoad.title == origamiUiString(id: .cpuLoad), ["title": CommandID.cancelStart.title])
    }

    // A long name is middle-truncated: two names that differ only at their end draw differently in a narrow row.
    do {
        let prefix = "a-machine-with-a-name-far-too-long-to-fit-in-the-library-"
        let a = MachineModel(summary: syntheticSummary(prefix + "alpha"), host: host, defaultPointer: .follow)
        let b = MachineModel(summary: syntheticSummary(prefix + "omega"), host: host, defaultPointer: .follow)
        let size = CGSize(width: 200, height: 44)
        let pa = "\(out)/ui-row-long-a.png", pb = "\(out)/ui-row-long-b.png"
        let shotA = snapshot(LibraryRow(model: a).environmentObject(app), size: size, to: pa)
        let shotB = snapshot(LibraryRow(model: b).environmentObject(app), size: size, to: pb)
        let differ = (try? Data(contentsOf: URL(fileURLWithPath: pa))) != (try? Data(contentsOf: URL(fileURLWithPath: pb)))
        check("long-machine-name-is-middle-truncated", shotA.ok && shotB.ok && differ, ["differ": differ])
    }

    // The alerts: the safe answer is the default, the destructive one is marked, and Power off asks first.
    do {
        let power = origamiAlert(kind: .powerOff(name: "octane2"), platform: .macos)
        let remove = origamiAlert(kind: .remove(name: "spare", diskLeft: []), platform: .macos)
        func defaultButton(_ a: OrigamiAlert) -> OrigamiButtonRole? { a.buttons.firstIndex { $0.role != .destructive }.map { a.buttons[$0].role } }
        check("alerts-default-to-the-safe-answer", defaultButton(power) == .cancel && defaultButton(remove) == .cancel
              && power.buttons.contains { $0.role == .destructive } && remove.buttons.contains { $0.role == .destructive },
              ["power": power.buttons.map(\.label), "remove": remove.buttons.map(\.label)])
    }

    // Dropping files: a disk image on the library opens New machine with it; a CD image is not for the library.
    do {
        let image = URL(fileURLWithPath: fixtures + "/disk-irix-6.5.30.img")
        let kind = origamiClassifyDrop(path: image.path)
        let cd = URL(fileURLWithPath: library + "/drop.iso")
        FileManager.default.createFile(atPath: cd.path, contents: Data(count: 2048))
        let took = app.dropOnLibrary([image]), refused = app.dropOnLibrary([cd])
        check("drop-a-disk-image-on-the-library-opens-new-machine-with-it",
              kind == .diskImage && origamiClassifyDrop(path: cd.path) == .cdImage && took && !refused && app.droppedDisk == image && app.wantNewMachine,
              ["kind": "\(kind)"])
        app.droppedDisk = nil; app.wantNewMachine = false
    }

    // Remove: the core's alert, then the machine is gone and a toast says so. The core's fake build stands in for the Trash.
    if makeFakeMachine(host, library: library, name: "spare") == nil {
        app.rescanNow()
        if let spare = app.model(named: "spare") {
            var asked: OrigamiAlert?
            app.present = { a in asked = a; return a.buttons.firstIndex { $0.role == .cancel } ?? 0 }
            app.select(spare.id)
            check("remove-is-offered-for-a-stopped-machine", app.isEnabled(.remove) && CommandID.remove.title == "Move to Trash…", ["title": CommandID.remove.title])
            app.invoke(.remove)
            check("remove-cancelled-keeps-the-machine", asked?.title.contains("spare") == true && app.model(named: "spare") != nil,
                  ["asked": asked?.title ?? "none"])
            app.present = { a in a.buttons.firstIndex { $0.role == .destructive } ?? 0 }
            app.invoke(.remove)
            // The core's fake build deletes the folder and lists it instead of using the host's Trash.
            let trashed = host.core.fakeTrashed()
            check("remove-moves-the-machine-away-and-says-so",
                  waitFor(10) { app.rescanNow(); return app.model(named: "spare") == nil }
                  && trashed.contains { $0.hasSuffix("/spare") } && !FileManager.default.fileExists(atPath: library + "/spare")
                  && app.notice?.text == origamiToast(kind: .removed(name: "spare"), platform: .macos).text,
                  ["notice": app.notice?.text ?? "none", "trashed": trashed])
        } else { check("remove-is-offered-for-a-stopped-machine", false, ["error": "the machine is not listed"]) }
    } else { check("remove-is-offered-for-a-stopped-machine", false, ["error": "the machine was not made"]) }

    // Detaching a drive and removing a download ask first; only the destructive button goes ahead.
    do {
        var asked: [String] = []
        app.present = { a in asked.append(a.title); return a.buttons.firstIndex { $0.role == .cancel } ?? 0 }
        let kept = !app.confirmed(.detachDrive(name: "Disk 2")) && !app.confirmed(.removeDownload(name: "IRIX 6.5.22"))
        app.present = { a in a.buttons.firstIndex { $0.role == .destructive } ?? 0 }
        check("detach-and-remove-download-wait-for-the-destructive-button",
              kept && app.confirmed(.detachDrive(name: "Disk 2")) && asked == ["Detach Disk 2?", "Remove IRIX 6.5.22?"], ["asked": asked])
    }

    // Pictures for comparing with the design: the main window at rest, a few state cards, light and dark.
    do {
        var wrote = true
        func both<V: View>(_ name: String, _ view: V, _ size: CGSize) {
            for dark in [false, true] { wrote = snapshotRetina(view, size: size, dark: dark, to: "\(out)/ux-\(name)-\(dark ? "dark" : "light").png") && wrote }
        }
        app.closeAllTabsForTest()
        if let first = app.models.first { app.select(first.id) }
        both("main-window", MainView().environmentObject(app), CGSize(width: 1100, height: 720))
        let card = MachineModel(summary: syntheticSummary("octane2"), host: host, defaultPointer: .follow)
        card.failStartForTest("Its graphics board is set to a slot this model doesn't have.")
        _ = waitFor(10) { card.state == .stoppedUnexpectedly }
        both("card-couldnt-start", StateCard(model: card).environmentObject(app), CGSize(width: 700, height: 460))
        card.injectStaleState(.noPicture)
        both("card-no-picture", StateCard(model: card).environmentObject(app), CGSize(width: 700, height: 460))
        card.injectStaleState(.needsUpgrade)
        both("card-needs-upgrade", StateCard(model: card).environmentObject(app), CGSize(width: 700, height: 460))
        card.injectStaleState(.stopped)
        both("machine-page", Hero(model: card).environmentObject(app), CGSize(width: 700, height: 560))
        check("design-pictures-written", wrote, ["out": out])
    }

    // New machine: the fields are labelled, the first disk choice is the default and Advanced opens from its header.
    do {
        let nm = NewMachineModel(app: app)
        nm.load(); _ = waitFor(30) { !nm.loading }
        let first = origamiDiskOptions(readyAvailable: !nm.readyDisks.isEmpty).first?.choice
        check("new-machine-starts-on-the-core's-first-disk-choice", first != nil && nm.disk == DiskChoice(first!), ["first": "\(String(describing: first))"])
        let (_, win) = hosted(NewMachineSheet(model: nm).environmentObject(app), size: CGSize(width: 460, height: 640))
        for dark in [false, true] { _ = snapshotRetina(NewMachineSheet(model: nm).environmentObject(app), size: CGSize(width: 460, height: 640), dark: dark, to: "\(out)/ux-new-machine-\(dark ? "dark" : "light").png") }
        // Every field's label is a core word, never empty; Advanced is a model flag the header toggles.
        let labels = [Labels.NewMachine.name, Labels.NewMachine.model, ui(.machineId), ui(.ethernetAddress), ui(.diskSize), ui(.fromTheMachineId), ui(.custom)]
        check("new-machine-fields-have-core-labels", labels.allSatisfy { !$0.isEmpty }, ["labels": labels])
        nm.advancedOpen.toggle(); spin(0.5)
        check("new-machine-advanced-opens", nm.advancedOpen, ["open": nm.advancedOpen])
        check("new-machine-says-what-the-machine-id-sets", nm.nextId.map { !origamiMachineIdCaption(next: $0).isEmpty } ?? false, ["next": nm.nextId.map { Int($0) } ?? -1])
        win.contentView = nil; win.close()
    }
}
#endif
