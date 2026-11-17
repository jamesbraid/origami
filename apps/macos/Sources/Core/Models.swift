// What the views observe: a MachineModel per library machine and the AppModel
// that owns them. The core's observer reports changes, so nothing polls; its
// blocking calls (start, stop) run on a background queue.

import AppKit
import Combine
import SwiftUI

enum MachineState: Equatable {
    case stopped, starting, running, noPicture, stopping
    case stoppedUnexpectedly, leftRunning

    /// The pill: one or two words. Colour never stands without them.
    var pill: String {
        switch self {
        case .stopped: return "Stopped"
        case .starting: return "Starting"
        case .running: return "Running"
        case .noPicture: return "No picture"
        case .stopping: return "Saving"
        case .stoppedUnexpectedly: return "Stopped unexpectedly"
        case .leftRunning: return "Left running"
        }
    }

    var tone: Tone {
        switch self {
        case .stopped: return .grey
        case .starting, .stopping, .leftRunning: return .amber
        case .running: return .green
        case .noPicture, .stoppedUnexpectedly: return .red
        }
    }
    enum Tone { case grey, amber, green, red }

    var isActive: Bool { self != .stopped && self != .stoppedUnexpectedly }
}

enum PointerMode: String { case follow, capture }

/// How long a running machine may show nothing before the view says so.
let noPictureTimeout: TimeInterval = 90

@MainActor
final class MachineModel: ObservableObject, Identifiable {
    private(set) var record: MachineRecord
    private let host: CoreHost
    /// Opened on first use; nil until then.
    private(set) var session: MachineSession?
    @Published private(set) var state: MachineState = .stopped
    /// Set while a power action runs ("Starting…", "Saving…", "Resetting…").
    @Published private(set) var busy: String?
    @Published private(set) var picture: (width: Int, height: Int)?
    @Published var pointerMode: PointerMode
    @Published private(set) var captured = false
    /// The capture hint shows until this moment.
    @Published private(set) var hudUntil: Date?
    @Published private(set) var failure: String?
    @Published private(set) var hasPicture = false
    /// The emulator is up (the first starting stage).
    @Published private(set) var emulatorUp = false

    weak var view: MachineView?
    private(set) var startedAt: Date?
    private var expectedStop = false
    private var noPictureTimer: DispatchWorkItem?
    var id: String { record.key }
    var name: String { record.name }

    init(summary: OrigamiMachineSummary, host: CoreHost, defaultPointer: PointerMode) {
        record = MachineRecord(summary)
        self.host = host
        pointerMode = defaultPointer
        refresh(summary: summary)
    }

    // MARK: state

    /// Take in a library summary: what the machine was before this app opened it.
    func refresh(summary: OrigamiMachineSummary) {
        record = MachineRecord(summary)
        if session == nil {
            let up = summary.state == .running || summary.state == .starting
            set(up ? .leftRunning : .stopped)
        }
    }

    /// Pull the machine's state from the core. Called on an event.
    func refresh() {
        guard let machine = session?.machine else { return }
        let view = machine.view()
        session?.updateHead()
        let head = view.heads.first
        let active = head?.active ?? false
        if hasPicture != active { hasPicture = active }
        if active, let head, picture?.width != Int(head.width) || picture?.height != Int(head.height) {
            picture = (Int(head.width), Int(head.height))
        }
        let up = view.state == .running || view.state == .paused
        if emulatorUp != up { emulatorUp = up }
        let next: MachineState
        if busy == "Saving…" || busy == "Stopping…" || view.state == .stopping {
            next = .stopping
        } else if busy == "Starting…" || view.state == .starting {
            next = .starting
        } else if up {
            if active { next = .running }
            else if let startedAt, Date().timeIntervalSince(startedAt) > noPictureTimeout { next = .noPicture }
            else { next = .starting; scheduleNoPictureCheck() }
        } else if view.state == .exited && !expectedStop {
            failure = view.lastError ?? "The emulator exited."
            next = .stoppedUnexpectedly
        } else if state == .stoppedUnexpectedly && !expectedStop {
            next = state
        } else {
            next = .stopped
        }
        set(next)
    }

    private func set(_ next: MachineState) { if state != next { state = next } }

    /// One check after the timeout, not a poll.
    private func scheduleNoPictureCheck() {
        guard noPictureTimer == nil, let startedAt else { return }
        let item = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.noPictureTimer = nil; self?.refresh() }
        }
        noPictureTimer = item
        let wait = max(0.1, noPictureTimeout - Date().timeIntervalSince(startedAt) + 0.1)
        DispatchQueue.main.asyncAfter(deadline: .now() + wait, execute: item)
    }

    /// One sentence of what is happening.
    var statusLine: String {
        switch state {
        case .stopped: return "Not running."
        case .starting: return "Starting up the system…"
        case .running:
            if let p = picture { return "\(p.width) × \(p.height)" }
            return "Running."
        case .noPicture: return "The emulator is running but no picture has arrived."
        case .stopping: return "Saving changes to the disk image."
        case .stoppedUnexpectedly: return failure ?? "The emulator stopped."
        case .leftRunning: return "Running without Origami."
        }
    }

    // MARK: power

    var canStart: Bool { state == .stopped || state == .stoppedUnexpectedly || state == .leftRunning }
    var canStop: Bool { [.starting, .running, .noPicture, .leftRunning].contains(state) && busy == nil }
    var isRunning: Bool { [.starting, .running, .noPicture, .stopping, .leftRunning].contains(state) }

    /// Open the machine with the core and wire its frame plane.
    private func open() throws -> MachineSession {
        if let session { return session }
        let machine = try host.core.openMachine(key: record.key)
        let s = MachineSession(machine: machine)
        session = s
        return s
    }

    /// Run `work` off the main thread; `done` runs on it afterwards. A failure
    /// leaves `failureState` (a start that failed shows its error, not "Stopped").
    private func background(_ title: String, failureState: MachineState? = nil,
                            _ work: @escaping @Sendable (MachineSession) throws -> Void,
                            done: (@MainActor (Error?) -> Void)?) {
        busy = title
        do {
            let s = try open()
            DispatchQueue.global(qos: .userInitiated).async {
                var error: Error?
                do { try work(s) } catch let e { error = e }
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { self.finished(error, failureState: failureState, done: done) }
                }
            }
        } catch {
            finished(error, failureState: failureState, done: done)
        }
    }

    private func finished(_ error: Error?, failureState: MachineState?, done: (@MainActor (Error?) -> Void)?) {
        busy = nil
        if let error {
            failure = "\(error)"
            expectedStop = false
            if let failureState { set(failureState) }
        }
        refresh()
        done?(error)
        if let queued = pendingStop {
            pendingStop = nil
            stop(force: queued.force, done: { e in for d in queued.done { d?(e) } })
        }
    }

    /// Wait for an operation; a failure becomes the thrown error.
    private nonisolated static func finish(_ op: OrigamiOperation) throws {
        if case .failed(_, let message) = op.wait(timeoutMs: nil) {
            throw NSError(domain: "origami", code: 1, userInfo: [NSLocalizedDescriptionKey: message])
        }
    }

    func start(done: (@MainActor (Error?) -> Void)? = nil) {
        guard canStart, busy == nil else { done?(nil); return }
        failure = nil
        expectedStop = false
        startedAt = Date()
        noPictureTimer?.cancel(); noPictureTimer = nil
        set(.starting)
        background("Starting…", failureState: .stoppedUnexpectedly, { s in
            // A machine left running is already up: attach, don't start it again.
            let now = s.machine.view().state
            if now == .running || now == .starting { return }
            try Self.finish(try s.machine.start())
        }, done: done)
    }

    private var pendingStop: (force: Bool, done: [(@MainActor (Error?) -> Void)?])?

    /// Always calls `done`. A stop asked for while another action runs waits for
    /// it, then runs; a force stop does not wait.
    func stop(force: Bool = false, done: (@MainActor (Error?) -> Void)? = nil) {
        guard isRunning else { done?(nil); return }
        if let busy, !(force && busy != "Stopping…") {
            var queued = pendingStop ?? (force: force, done: [])
            queued.force = queued.force || force
            queued.done.append(done)
            pendingStop = queued
            return
        }
        expectedStop = true
        set(.stopping)
        background(force ? "Stopping…" : "Saving…", { s in
            try Self.finish(try s.machine.stop(how: force ? .force : .clean))
        }, done: done)
    }

    func reset(done: (@MainActor (Error?) -> Void)? = nil) {
        guard state == .running || state == .starting, busy == nil else { done?(nil); return }
        startedAt = Date()
        background("Resetting…", { s in try s.machine.control(control: .reset) }, done: done)
    }

    // MARK: pointer

    /// Called by the display view whenever it captures or releases.
    func captureChanged(_ now: Bool) {
        captured = now
        hudUntil = now ? Date().addingTimeInterval(3) : nil
    }

    func capture() { view?.beginCapture() }
    func release() { view?.endCapture() }
}


@MainActor
final class AppModel: ObservableObject {
    let host: CoreHost?
    let launchError: String?
    let root: URL
    @Published private(set) var models: [MachineModel] = []
    /// Machines open as tabs, and which one shows.
    @Published private(set) var tabs: [String] = []
    @Published var active: String?
    @Published var sidebarVisible = true
    @Published var scaleMode: ScaleMode = .fit
    /// Window-opening requests the scene turns into windows.
    @Published var wantSettings = false

    /// Asked before an action that loses state. Tests answer it themselves.
    var confirm: (_ title: String, _ detail: String, _ action: String) -> Bool = { title, detail, action in
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = detail
        alert.addButton(withTitle: action)
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }
    var openFolderPanel: () -> URL? = {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.message = "Choose the folder that holds your machines"
        return panel.runModal() == .OK ? panel.url : nil
    }
    var openWindow: ((String) -> Void)?

    private var forwards: [AnyCancellable] = []
    static let defaults = UserDefaults.standard

    init(root: URL) {
        self.root = root
        var h: CoreHost?
        var err: String?
        do {
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
            h = try CoreHost(libraryRoot: root.path)
        } catch let e { err = "\(e)" }
        host = h
        launchError = err
        h?.onChange = { [weak self] keys, library in self?.changed(keys, library: library) }
        rescanNow()
    }

    var defaultPointer: PointerMode {
        PointerMode(rawValue: AppModel.defaults.string(forKey: "pointerMode") ?? "") ?? .follow
    }

    /// The tab bar shows whenever a tab is open, even one.
    var showsTabBar: Bool { !tabs.isEmpty }

    var current: MachineModel? { models.first { $0.id == active } }
    func model(named name: String) -> MachineModel? { models.first { $0.name == name } }

    // MARK: library and events

    /// Re-read the library on a background queue; the models change only if it did.
    func rescan() {
        guard let host else { return }
        scanIssued += 1
        let n = scanIssued
        DispatchQueue.global(qos: .utility).async {
            let found = try? host.core.listMachines()
            DispatchQueue.main.async { MainActor.assumeIsolated { if let found { self.applyListing(found, issued: n) } } }
        }
    }

    /// Listings are numbered when asked for; one that was asked for before the newest applied one is dropped.
    private var scanIssued = 0, scanApplied = 0
    private func applyListing(_ summaries: [OrigamiMachineSummary], issued: Int) {
        guard issued > scanApplied else { return }
        scanApplied = issued
        apply(summaries)
    }

    /// The same on the calling thread, for callers that need the answer now.
    func rescanNow() {
        if let host, let found = try? host.core.listMachines() { scanIssued += 1; applyListing(found, issued: scanIssued) }
    }

    private var lastSummaries: [OrigamiMachineSummary]?

    private func apply(_ summaries: [OrigamiMachineSummary]) {
        guard let host, summaries != lastSummaries else { return }
        lastSummaries = summaries
        var next: [MachineModel] = []
        for s in summaries {
            if let existing = models.first(where: { $0.id == s.key }) {
                existing.refresh(summary: s)
                next.append(existing)
            } else {
                next.append(MachineModel(summary: s, host: host, defaultPointer: defaultPointer))
            }
        }
        if next.map(\.id) != models.map(\.id) {
            models = next
            // A change in any machine redraws what shows the app's state: menus, toolbar.
            forwards = models.map { m in m.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() } }
            let kept = tabs.filter { id in models.contains { $0.id == id } }
            if kept != tabs { tabs = kept }
        }
        if active == nil || !tabs.contains(active!) { active = tabs.first }
    }

    /// The core said something changed; pull what we show.
    func changed(_ keys: Set<String>, library: Bool) {
        if library { rescan() }
        for k in keys { models.first { $0.id == k }?.refresh() }
    }

    func chooseLibrary(_ url: URL) {
        AppModel.defaults.set(url.path, forKey: "libraryRoot")
        _ = confirm("Library changed", "Origami uses \(url.path) the next time it opens.", "OK")
    }

    // MARK: tabs

    /// A click in the library: show the machine in the current tab.
    func select(_ id: String) {
        if tabs.contains(id) { active = id; return }
        if let a = active, let i = tabs.firstIndex(of: a) {
            replaced = (slot: i, old: a, new: id)
            tabs[i] = id
        } else { tabs.append(id) }
        active = id
    }

    /// The last tab a click took over: (slot, machine it showed, machine now shown there).
    private var replaced: (slot: Int, old: String, new: String)?

    /// Open the machine in a tab of its own. If a plain click just took over another tab to show
    /// this machine (the first click of a double-click), give that tab its machine back.
    func openInNewTab(_ id: String) {
        if let r = replaced, r.new == id, r.slot < tabs.count, tabs[r.slot] == id, !tabs.contains(r.old) {
            tabs[r.slot] = r.old
            tabs.append(id)
        } else if !tabs.contains(id) {
            tabs.append(id)
        }
        replaced = nil
        active = id
    }

    func closeTab(_ id: String) {
        guard let i = tabs.firstIndex(of: id) else { return }
        // Closing a tab never ends a machine: the runner owns it.
        tabs.remove(at: i)
        if active == id { active = tabs.isEmpty ? nil : tabs[min(i, tabs.count - 1)] }
    }

    func closeAllTabsForTest() { tabs = []; active = nil; replaced = nil }

    func step(_ delta: Int) {
        guard tabs.count > 1, let a = active, let i = tabs.firstIndex(of: a) else { return }
        active = tabs[(i + delta + tabs.count) % tabs.count]
    }

    // MARK: commands

    func isEnabled(_ command: CommandID) -> Bool {
        let m = current
        switch command {
        case .start: return m?.canStart ?? false
        case .powerOff: return (m?.canStop ?? false)
        case .forceQuit: return m?.isRunning ?? false
        case .reset: return (m?.state == .running)
        case .captureInput: return m?.state == .running && !(m?.captured ?? false)
        case .followMouse: return m != nil
        case .diagnostics, .settings, .showInFinder: return m != nil
        case .closeTab: return active != nil
        case .nextTab, .previousTab: return tabs.count > 1
        case .fit, .actualSize, .pointScale, .doubleScale: return m?.state == .running
        case .newMachine, .pasteAsTyping, .console: return false
        case .chooseLibrary, .showLibrary, .library: return true
        }
    }

    func invoke(_ command: CommandID) {
        guard isEnabled(command) else { return }
        let m = current
        switch command {
        case .start: m?.start()
        case .powerOff:
            if let m { m.stop { [weak self, weak m] e in if let e, let m { self?.stopFailed(m, e) } } }
        case .forceQuit:
            if confirm("Force Quit the emulator?", "\(m?.name ?? "The machine") stops at once. Changes the guest has not written to its disk are lost.", "Force Quit") {
                m?.stop(force: true)
            }
        case .reset:
            if confirm("Reset \(m?.name ?? "the machine")?", "The machine restarts as if its reset button were pressed.", "Reset") {
                m?.reset()
            }
        case .captureInput: m?.capture()
        case .followMouse:
            if let m {
                m.pointerMode = m.pointerMode == .follow ? .capture : .follow
                if m.pointerMode == .follow { m.release() }
            }
        case .diagnostics: openWindow?("diagnostics")
        case .settings: wantSettings = true
        case .showInFinder:
            if let m { NSWorkspace.shared.activateFileViewerSelecting([root.appendingPathComponent(m.name)]) }
        case .closeTab: if let a = active { closeTab(a) }
        case .nextTab: step(1)
        case .previousTab: step(-1)
        case .showLibrary: sidebarVisible.toggle()
        case .library: sidebarVisible = true
        case .fit: scaleMode = .fit
        case .pointScale: scaleMode = .points
        case .doubleScale: scaleMode = .double
        case .actualSize: scaleMode = .actual
        case .chooseLibrary: if let url = openFolderPanel() { chooseLibrary(url) }
        case .newMachine, .pasteAsTyping, .console: break
        }
    }

    // MARK: quit

    /// Machines still running when the app quits.
    var running: [MachineModel] { models.filter { $0.isRunning } }

    /// Stop every running machine in turn. A machine that errors or takes longer
    /// than `bound` seconds is reported in `done` as failed, and the next one goes on.
    func stopAll(force: Bool = false, bound: TimeInterval = 60,
                 progress: ((String) -> Void)? = nil,
                 done: @escaping @MainActor (_ failed: [MachineModel]) -> Void) {
        let list = running
        var failed: [MachineModel] = []
        func next(_ i: Int) {
            guard i < list.count else { done(failed); return }
            let m = list[i]
            progress?(force ? "Force quitting \(m.name)…" : "Powering off \(m.name)…")
            var finished = false
            let advance: @MainActor (Bool) -> Void = { ok in
                guard !finished else { return }
                finished = true
                if !ok { failed.append(m) }
                next(i + 1)
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + bound) { MainActor.assumeIsolated { advance(false) } }
            m.stop(force: force) { e in advance(e == nil) }
        }
        next(0)
    }

    /// Power Off did not finish: say why, and offer a force quit.
    func stopFailed(_ m: MachineModel, _ error: Error) {
        if confirm("\(m.name) did not power off", "\(error.localizedDescription)\n\nForce Quit stops the emulator at once; changes the guest has not written to disk are lost.", "Force Quit") {
            m.stop(force: true)
        }
    }
}
