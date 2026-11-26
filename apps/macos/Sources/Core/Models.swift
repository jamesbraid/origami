// What the views observe: a MachineModel per library machine and the AppModel
// that owns them. The core's observer reports changes, so nothing polls; its
// blocking calls (start, stop) run on a background queue.

import AppKit
import Combine
import SwiftUI

enum MachineState: Equatable {
    case stopped, starting, running, noPicture, stopping
    case stoppedUnexpectedly, leftRunning, paused
    /// Made by an older Origami; it cannot start until it is upgraded.
    case needsUpgrade

    var isActive: Bool { self != .stopped && self != .stoppedUnexpectedly && self != .needsUpgrade }

    /// What the core calls this state, before the install guide and unreadable machines are told apart.
    var shown: OrigamiShownState {
        switch self {
        case .stopped: return .stopped
        case .starting: return .starting
        case .running: return .running
        case .noPicture: return .noPicture
        case .stopping: return .saving
        case .stoppedUnexpectedly: return .exited
        case .leftRunning: return .leftRunning
        case .paused: return .paused
        case .needsUpgrade: return .needsUpgrade
        }
    }

    /// The core's word for the state.
    var word: String { origamiStateLabel(state: shown).word }
}

/// A core error that reached the app as an operation's failure rather than a thrown one.
struct InfoError: Error { let info: OrigamiErrorInfo }

enum PointerMode: String { case follow, capture }

/// How long a running machine may show nothing before the view says so.
let noPictureTimeout: TimeInterval = 90
/// How long a running machine may show no display head before its console is shown instead.
let headlessGrace: TimeInterval = 4

@MainActor
final class MachineModel: ObservableObject, Identifiable {
    private(set) var record: MachineRecord
    private let host: CoreHost
    var coreHost: CoreHost { host }
    /// Opened on first use; nil until then.
    private(set) var session: MachineSession?
    @Published private(set) var state: MachineState = .stopped
    /// Set while a power action runs ("Starting…", "Saving…", "Resetting…").
    @Published private(set) var busy: String?
    @Published private(set) var picture: (width: Int, height: Int)?
    @Published var pointerMode: PointerMode { didSet { if oldValue != pointerMode { refresh() } } }
    @Published private(set) var captured = false
    /// The capture hint shows until this moment.
    @Published private(set) var hudUntil: Date?
    @Published private(set) var failure: String?
    /// Why the last start failed, in the core's words, until the next start.
    @Published private(set) var startError: OrigamiErrorInfo?
    /// The install guide runs for this machine; the install model sets it.
    @Published var installing = false
    /// The install shows this machine's serial console in place of its screen; the install model sets it.
    @Published var installConsole = false
    /// The bar under the picture, from the core; nil unless the machine runs.
    @Published private(set) var statusBar: OrigamiStatusBar?
    @Published private(set) var hasPicture = false
    /// Running with a console and no display: shown as its console. This is read from the run (no head after
    /// a few seconds) and is replaced by the core's `display` flag once it reports one.
    @Published private(set) var headless = false
    /// The emulator is up (the first starting stage).
    @Published private(set) var emulatorUp = false

    weak var view: MachineView?
    /// Sizes the window for this machine's picture; the View menu's scales go through it.
    weak var fitter: FitView?
    /// The serial console model, made when its window first opens.
    private(set) var consoleModel: ConsoleModel?
    func console() -> ConsoleModel {
        if let c = consoleModel { return c }
        let c = ConsoleModel(machine: self); consoleModel = c; return c
    }
    func openSession() throws -> MachineSession { try open() }
    private(set) var startedAt: Date?
    private var expectedStop = false
    private var noPictureTimer: DispatchWorkItem?
    var id: String { record.key }
    var name: String { record.name }

    /// The state a person sees: the core's list of them, with the guide and unreadable machines told apart.
    var shown: OrigamiShownState {
        if state == .needsUpgrade { return .needsUpgrade }
        if record.problem != nil { return .cantOpen }
        if installing && (state == .starting || state == .running) { return .installing }
        return state.shown
    }
    var word: String { origamiStateLabel(state: shown).word }
    var tone: OrigamiTone { origamiStateLabel(state: shown).tone }
    /// The library row's second line.
    var line: OrigamiLibraryLine { origamiLibraryLineFor(state: shown, model: record.modelName) }
    /// The row's tooltip and accessibility label.
    var rowDescription: String {
        origamiRowDescriptionFor(state: shown, name: name, model: record.modelName, os: record.systemDisk)
    }

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
        if summary.needsUpgrade { set(.needsUpgrade); return }
        if state == .needsUpgrade { set(.stopped) }
        if session == nil {
            // The core lists a machine another process runs as left running.
            if summary.state == .leftRunning { set(.leftRunning) }
            else if state == .leftRunning {
                // A machine we found running died while nobody was watching: not a clean stop.
                failure = "The machine stopped while Origami was not watching it."
                set(.stoppedUnexpectedly)
            } else if state != .stoppedUnexpectedly { set(.stopped) }
        }
    }

    private var refreshing = false
    private var refreshAgain = false
    /// The core's view as of the last refresh (Diagnostics and quit read this, never the core itself).
    private(set) var coreView: OrigamiMachineView?
    var isRefreshing: Bool { refreshing }

    /// Ask the core for the machine's state. The core's `view()` can query a live emulator, so it is
    /// read on a background queue and the result applied here; overlapping requests coalesce.
    func refresh() {
        guard let machine = session?.machine else { return }
        if refreshing { refreshAgain = true; return }
        refreshing = true
        let (follows, captured) = (pointerMode == .follow, self.captured)
        DispatchQueue.global(qos: .utility).async {
            let view = machine.view()
            let bar = machine.statusBar(follows: follows, captured: captured, platform: thisPlatform)
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.refreshing = false
                    self.apply(view)
                    if self.statusBar != bar { self.statusBar = bar }
                    if self.refreshAgain { self.refreshAgain = false; self.refresh() }
                }
            }
        }
    }

    /// Take in one reading of the core's view.
    func apply(_ view: OrigamiMachineView) {
        coreView = view
        session?.setHead(from: view)
        let head = view.heads.first
        let active = head?.active ?? false
        if hasPicture != active { hasPicture = active }
        if active, let head, picture?.width != Int(head.width) || picture?.height != Int(head.height) {
            picture = (Int(head.width), Int(head.height))
        }
        if let e = view.startError, startError != e { startError = e }
        let up = view.state == .running || view.state == .paused
        if emulatorUp != up { emulatorUp = up }
        let next: MachineState
        if busy == "Saving…" || busy == "Stopping…" || view.state == .stopping {
            next = .stopping
        } else if view.state == .paused && up {
            next = .paused
        } else if busy == "Starting…" || view.state == .starting {
            next = .starting
        } else if up {
            let waited = startedAt.map { Date().timeIntervalSince($0) } ?? .infinity
            let consoleOnly = !active && view.heads.isEmpty && view.console.connected && waited > headlessGrace
            if headless != consoleOnly { headless = consoleOnly }
            if active || consoleOnly { next = .running }
            else if waited > noPictureTimeout { next = .noPicture }
            else { next = .starting; scheduleNoPictureCheck() }
        } else if state == .stoppedUnexpectedly && !expectedStop && !Self.endedCleanly(view) {
            next = state   // sticky: a start error or a crash stays until the next start
        } else if [.starting, .running, .noPicture, .leftRunning, .paused].contains(state) && !expectedStop && busy == nil {
            // It ended on its own, and the core says how: a guest that powered itself off, or a stop
            // something asked for, is clean; a crash or an engine that left no word is not.
            switch view.exitReason {
            case .guestPoweredOff, .hostStopped:
                next = .stopped
            case nil where view.state == .exited:
                // The core has the machine ended but not yet why: look again rather than guess.
                next = state
                scheduleNoPictureCheck()
            default:
                failure = Self.exitSentence(view)
                next = .stoppedUnexpectedly
            }
        } else {
            next = .stopped
        }
        if next == .stopped && Self.endedCleanly(view) { failure = nil }   // an earlier guess that it crashed no longer stands
        set(next)
    }

    /// The core says the machine ended on the guest's or the host's own account, whatever was thought before.
    nonisolated static func endedCleanly(_ v: OrigamiMachineView) -> Bool {
        guard v.state == .exited, v.lastError == nil else { return false }
        return v.exitReason == .guestPoweredOff || v.exitReason == .hostStopped
    }

    /// What to tell the person about an emulator that ended badly, in the core's words where it has them.
    nonisolated static func exitSentence(_ v: OrigamiMachineView) -> String {
        if let e = v.lastError, !e.isEmpty { return e }
        let how = v.exitDetail.map { " (\($0))" } ?? ""
        switch v.exitReason {
        case .crashed?: return "The emulator crashed\(how)."
        case .runnerError?: return "The emulator's runner ended without saying why\(how)."
        default: return v.exitCode.map { "The emulator exited with code \($0)." } ?? "The emulator stopped without being asked."
        }
    }

    private func set(_ next: MachineState) { if state != next { state = next } }

    /// Look again in a second: the head, the headless grace and the no-picture timeout all pass with time.
    private func scheduleNoPictureCheck() {
        guard noPictureTimer == nil else { return }
        let item = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.noPictureTimer = nil; self?.refresh() }
        }
        noPictureTimer = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 1, execute: item)
    }

    // MARK: upgrade

    @Published private(set) var upgrading = false
    @Published private(set) var upgradeError: OrigamiErrorInfo?

    /// Run the core's upgrade off the main thread. The core's library event makes the listing show the result;
    /// `prom` and `ioProm` are firmware files the person chose instead of a download.
    func upgrade(prom: String? = nil, ioProm: String? = nil, done: (@MainActor (Bool) -> Void)? = nil) {
        guard record.needsUpgrade, !upgrading else { return }
        upgrading = true
        upgradeError = nil
        let (host, key) = (host, record.key)
        DispatchQueue.global(qos: .userInitiated).async {
            let result = Result { try host.core.upgradeMachineWith(key: key, prom: prom, ioProm: ioProm) }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.upgrading = false
                    switch result {
                    case .success: done?(true)
                    case .failure(let e): self.upgradeError = Self.info(of: e); done?(false)
                    }
                }
            }
        }
    }

    /// An error as the core's value: its plain sentence, technical detail and fix.
    nonisolated static func info(of error: Error) -> OrigamiErrorInfo {
        if let c = error as? CoreError { return origamiErrorInfo(error: c) }
        if let i = error as? InfoError { return i.info }
        return OrigamiErrorInfo(kind: .internal, message: error.localizedDescription, detail: nil, fix: OrigamiErrorFix.none)
    }

    // MARK: power

    var canStart: Bool { state == .stopped || state == .stoppedUnexpectedly || state == .leftRunning }
    var canRemove: Bool { state == .stopped || state == .stoppedUnexpectedly || state == .needsUpgrade }
    var canStop: Bool { [.starting, .running, .noPicture, .leftRunning, .paused].contains(state) && busy == nil }
    var isRunning: Bool { [.starting, .running, .noPicture, .stopping, .leftRunning, .paused].contains(state) }

    /// For tests: a start that failed with `message`.
    func openForTest() { _ = try? open() }

    func failStartForTest(_ message: String) {
        busy = "Starting…"; set(.starting)
        finished(NSError(domain: "origami", code: 1, userInfo: [NSLocalizedDescriptionKey: message]), failureState: .stoppedUnexpectedly, done: nil)
    }

    /// For tests: a start that failed with the core's own error, fix and detail included.
    func failStartForTest(_ info: OrigamiErrorInfo) {
        busy = "Starting…"; set(.starting)
        finished(InfoError(info: info), failureState: .stoppedUnexpectedly, done: nil)
    }

    /// For tests: pretend the UI is stale.
    func injectStaleState(_ s: MachineState) { set(s) }

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
        // Stopping a machine that has already ended is not a failure.
        if let e = error as? CoreError, case .NotRunning = e { expectedStop = false; refresh(); done?(nil); return }
        if let error {
            let info = Self.info(of: error)
            failure = info.message
            startError = failureState == nil ? startError : info
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
            throw InfoError(info: op.failure() ?? OrigamiErrorInfo(kind: .engine, message: message, detail: nil, fix: OrigamiErrorFix.none))
        }
    }

    func start(done: (@MainActor (Error?) -> Void)? = nil) {
        guard canStart, busy == nil else { done?(nil); return }
        failure = nil
        startError = nil
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

    func pause(done: (@MainActor (Error?) -> Void)? = nil) {
        guard state == .running, busy == nil else { done?(nil); return }
        background("Pausing…", { s in try s.machine.control(control: .pause) }, done: done)
    }

    func resume(done: (@MainActor (Error?) -> Void)? = nil) {
        guard state == .paused, busy == nil else { done?(nil); return }
        background("Resuming…", { s in try s.machine.control(control: .resume) }, done: done)
    }

    /// Save the machine's screen to `url` as a PNG.
    func screenshot(to url: URL, done: @escaping @MainActor (Error?) -> Void) {
        guard let s = session, state == .running || state == .paused else { done(ScreenshotError.noPicture); return }
        let ppm = FileManager.default.temporaryDirectory.appendingPathComponent("origami-\(UUID().uuidString).ppm")
        DispatchQueue.global(qos: .userInitiated).async {
            var failure: Error?
            do {
                try s.machine.screendump(head: 0, path: ppm.path)
                try Screenshot.writePNG(fromPPM: ppm, to: url)
            } catch let e { failure = e }
            try? FileManager.default.removeItem(at: ppm)
            DispatchQueue.main.async { MainActor.assumeIsolated { done(failure) } }
        }
    }

    // MARK: pointer

    /// Called by the display view whenever it captures or releases.
    func captureChanged(_ now: Bool) {
        captured = now
        hudUntil = now ? Date().addingTimeInterval(3) : nil
        refresh()
    }

    func capture() { view?.beginCapture() }
    func release() { view?.endCapture() }
}


@MainActor
final class AppModel: ObservableObject {
    let host: CoreHost?
    let launchError: String?
    @Published private(set) var root: URL
    @Published private(set) var models: [MachineModel] = []
    /// Machines open as tabs, and which one shows.
    @Published private(set) var tabs: [String] = []
    @Published var active: String?
    @Published var sidebarVisible = true
    /// Window-opening requests the scene turns into windows.
    @Published var wantSettings = false
    @Published var wantNewMachine = false
    /// The Settings pane to open on, once; Choose a disk opens Drives.
    var settingsPane: MachineSettings.Pane = .general
    /// The machine whose install set-up sheet shows; the guide takes over the sidebar once the install starts.
    @Published var installSheet: String?
    /// The machine whose install guide is running, once its set-up is done.
    @Published var guideKey: String?
    /// The sidebar shows that guide in place of the library.
    @Published var sidebarGuide = false
    /// A disk image dropped on the library, for New machine to start from.
    @Published var droppedDisk: URL?
    /// Shows one of the core's alerts and returns the index of the button pressed. Tests answer it themselves.
    var present: (_ alert: OrigamiAlert) -> Int = { Alerts.run($0) }
    /// The core's prompt for a running install, with its own text for a quit or a tab close; true to stop it.
    /// Return keeps installing, Escape too. Tests answer it themselves.
    var confirmStopInstall: (_ quitting: Bool) -> Bool = { quitting in
        let t = origamiInstallTexts()
        let alert = NSAlert()
        alert.messageText = t.quitHeading
        alert.informativeText = quitting ? t.quitText : t.tabText
        alert.addButton(withTitle: quitting ? t.stopAndQuit : t.stopAndClose)
        alert.addButton(withTitle: t.keep)
        alert.buttons[0].keyEquivalent = ""
        alert.buttons[1].keyEquivalent = "\r"
        return alert.runModal() == .alertFirstButtonReturn
    }
    var openFolderPanel: () -> URL? = {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.message = ui(.chooseTheFolderThatHoldsYourMachines)
        return panel.runModal() == .OK ? panel.url : nil
    }
    /// Where screenshots go: the Pictures folder.
    var screenshotFolder = FileManager.default.urls(for: .picturesDirectory, in: .userDomainMask).first
        ?? FileManager.default.homeDirectoryForCurrentUser

    /// A toast over the window, with an action that shows a file when there is one.
    struct Notice: Equatable { let text: String; let reveal: URL?; var action: String? }
    @Published private(set) var notice: Notice?

    func announce(_ text: String, reveal: URL? = nil, action: String? = nil) {
        let n = Notice(text: text, reveal: reveal, action: action)
        notice = n
        DispatchQueue.main.asyncAfter(deadline: .now() + 6) { MainActor.assumeIsolated { if self.notice == n { self.notice = nil } } }
    }

    /// One of the core's toasts.
    func toast(_ kind: OrigamiToastKind, reveal: URL? = nil) {
        let t = origamiToast(kind: kind, platform: thisPlatform)
        announce(t.text, reveal: reveal, action: reveal == nil ? nil : t.action)
    }

    func screenshotURL(for machine: String) -> URL {
        let f = DateFormatter(); f.dateFormat = "yyyy-MM-dd 'at' HH.mm.ss"; f.locale = Locale(identifier: "en_US_POSIX")
        return screenshotFolder.appendingPathComponent("\(machine) \(f.string(from: Date())).png")
    }
    var openWindow: ((String) -> Void)?
    var openConsole: ((String) -> Void)?
    /// Whether the CPU panel is shown; hidden, nothing is polled.
    @Published var showsCpu = false

    private var forwards: [AnyCancellable] = []
    private var watchdog: Timer?
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
        if let h { CoreCommands.load(h.core) }
        rescanNow()
        // A safety net for an exit the core does not announce: look at machines that should be
        // up every two seconds. Machines at rest cost nothing.
        watchdog = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.checkLiveness() }
        }
    }

    var defaultPointer: PointerMode {
        PointerMode(rawValue: AppModel.defaults.string(forKey: "pointerMode") ?? "") ?? .follow
    }

    /// The tab bar shows once a second machine is open.
    var showsTabBar: Bool { tabs.count > 1 }

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
    private func applyListing(_ summaries: [OrigamiMachineSummary], issued: Int, force: Bool = false) {
        guard issued > scanApplied else { return }
        scanApplied = issued
        apply(summaries, force: force)
    }

    /// The same on the calling thread, for callers that need the answer now.
    func rescanNow() {
        if let host, let found = try? host.core.listMachines() { scanIssued += 1; applyListing(found, issued: scanIssued) }
    }

    private var lastSummaries: [OrigamiMachineSummary]?

    private func apply(_ summaries: [OrigamiMachineSummary], force: Bool = false) {
        guard let host, force || summaries != lastSummaries else { return }
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

    /// Re-read every machine that should be running, and the library if one was left running.
    func checkLiveness() {
        for m in models where m.session != nil && m.state.isActive { m.refresh() }
        if models.contains(where: { $0.state == .leftRunning }) { rescan() }
    }

    /// The core said something changed; pull what we show.
    func changed(_ keys: Set<String>, library: Bool) {
        if library { rescan() }
        for k in keys { models.first { $0.id == k }?.refresh() }
    }

    func chooseLibrary(_ url: URL) {
        AppModel.defaults.set(url.path, forKey: "libraryRoot")
        useLibrary(url)
    }

    /// Switch the library now. Machines running from the old folder keep running in the core.
    func useLibrary(_ url: URL) {
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        host?.core.setLibraryRoot(libraryRoot: url.path)
        root = url
        rescanNow()
    }

    /// The Input preference changed: every machine follows it at once.
    func defaultPointerChanged() { for m in models { m.pointerMode = defaultPointer } }

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
        case .powerOff: return (m?.canStop ?? false) && m?.state != .starting
        case .cancelStart: return m?.state == .starting
        case .cpuLoad: return m?.state == .running
        case .forceQuit: return m?.isRunning ?? false
        case .reset: return (m?.state == .running)
        case .pause: return m?.state == .running
        case .resume: return m?.state == .paused
        case .screenshot: return (m?.state == .running || m?.state == .paused) && m?.busy == nil
        case .captureInput: return m?.state == .running && !(m?.captured ?? false)
        case .followMouse: return m != nil
        case .settings: return m != nil && m?.state != .needsUpgrade
        case .installIrix: return m?.record.runsIrix == true && (m?.state == .stopped || m?.state == .stoppedUnexpectedly)
        case .diagnostics, .showInFinder: return m != nil
        case .remove: return m.map { $0.canRemove } ?? false
        case .closeTab: return active != nil
        case .nextTab, .previousTab: return tabs.count > 1
        case .fit, .actualSize, .pointScale, .doubleScale: return m?.state == .running
        case .console: return m?.state == .running || m?.state == .paused
        case .newMachine: return host != nil
        case .pasteAsTyping: return false
        case .chooseLibrary, .showLibrary, .library, .keyboardShortcuts: return true
        }
    }

    /// Ask with the core's alert; true when the answer is a button that does the thing.
    func confirmed(_ kind: OrigamiAlertKind) -> Bool {
        let alert = origamiAlert(kind: kind, platform: thisPlatform)
        let i = present(alert)
        return alert.buttons.indices.contains(i) && alert.buttons[i].role == .destructive
    }

    func invoke(_ command: CommandID) {
        guard isEnabled(command) else { return }
        let m = current
        switch command {
        case .start: m?.start()
        case .powerOff:
            if let m, confirmed(.powerOff(name: m.name)) {
                m.stop { [weak self, weak m] e in if let e, let m { self?.stopFailed(m, e) } }
            }
        case .forceQuit:
            if let m, confirmed(.forceQuit(name: m.name)) { m.stop(force: true) }
        case .reset:
            if let m, confirmed(.reset(name: m.name)) { m.reset() }
        case .pause: m?.pause()
        case .resume: m?.resume()
        case .cancelStart: m?.stop()
        case .cpuLoad: showsCpu.toggle()
        case .screenshot:
            if let m {
                let url = screenshotURL(for: m.name)
                m.screenshot(to: url) { [weak self] e in
                    if let e { self?.announce("The screenshot was not saved: \(e.localizedDescription)") }
                    else { self?.toast(.screenshotSaved, reveal: url) }
                }
            }
        case .captureInput: m?.capture()
        case .followMouse:
            if let m {
                m.pointerMode = m.pointerMode == .follow ? .capture : .follow
                if m.pointerMode == .follow { m.release() }
            }
        case .diagnostics: openWindow?("diagnostics")
        case .console: if let m { if m.headless { select(m.id) } else { openConsole?(m.id) } }
        case .settings: wantSettings = true
        case .installIrix: if let m { installIrix(m.id) }
        case .showInFinder:
            if let m { NSWorkspace.shared.activateFileViewerSelecting([root.appendingPathComponent(m.name)]) }
        case .remove: if let m { remove(m.id) }
        case .closeTab: if let a = active { closeTabAsking(a) }
        case .nextTab: step(1)
        case .previousTab: step(-1)
        case .showLibrary: sidebarVisible.toggle()
        case .library: sidebarVisible = true; sidebarGuide = false
        case .fit: toast(.pictureSize(name: command.title))   // the picture always fills the window
        case .pointScale: m?.fitter?.show(pointsPerPixel: 1); toast(.pictureSize(name: command.title))
        case .doubleScale: m?.fitter?.show(pointsPerPixel: 2); toast(.pictureSize(name: command.title))
        case .actualSize:
            if let f = m?.fitter { f.show(pointsPerPixel: 1 / max(1, f.backing.rounded())); toast(.pictureSize(name: command.title)) }
        case .chooseLibrary: if let url = openFolderPanel() { chooseLibrary(url) }
        case .newMachine: wantNewMachine = true
        case .pasteAsTyping: break
        case .keyboardShortcuts: openWindow?("shortcuts")
        }
    }

    // MARK: remove

    /// Move a stopped machine to the Trash after the core's alert, then say so. Reports whether it went.
    @discardableResult
    func remove(_ key: String) -> Bool {
        guard let m = models.first(where: { $0.id == key }), m.canRemove else { return false }
        // The core says what would move and what would stay, before anything does.
        let plan: OrigamiRemoved
        do { plan = try host?.core.removePlan(key: key) ?? OrigamiRemoved(name: m.name, diskLeft: []) } catch {
            announce(Self.sentence(of: error))
            return false
        }
        guard confirmed(.remove(name: plan.name, diskLeft: plan.diskLeft)) else { return false }
        do {
            _ = try host?.core.removeMachine(key: key)
        } catch {
            announce(Self.sentence(of: error))
            return false
        }
        closeTab(key)
        rescanNow()
        toast(.removed(name: m.name))
        return true
    }

    nonisolated static func sentence(of error: Error) -> String { MachineModel.info(of: error).message }

    // MARK: drag and drop

    /// A CD image dropped on a machine goes into its CD drive. Reports whether the drop was one.
    @discardableResult
    func dropOnMachine(_ urls: [URL], _ model: MachineModel) -> Bool {
        guard let host, let url = urls.first(where: { origamiClassifyDrop(path: $0.path) == .cdImage }) else { return false }
        let key = model.record.key
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                let machine = try host.core.openMachine(key: key)
                guard let drive = try machine.settings().drives.first(where: { $0.kind == "cdrom" }) else { return }
                try machine.changeMedia(drive: drive.name, image: url.path)
            } catch {
                DispatchQueue.main.async { MainActor.assumeIsolated { self.announce(Self.sentence(of: error)) } }
            }
        }
        return true
    }

    /// A disk image dropped on the library opens New machine with that disk chosen.
    @discardableResult
    func dropOnLibrary(_ urls: [URL]) -> Bool {
        guard let url = urls.first(where: { origamiClassifyDrop(path: $0.path) == .diskImage }) else { return false }
        droppedDisk = url
        wantNewMachine = true
        return true
    }

    // MARK: install assistants

    /// One assistant per machine; a window takes its model from here, so a second request finds the first.
    private var installs: [String: InstallModel] = [:]

    func installModel(for key: String) -> InstallModel? {
        if let open = installs[key] { return open }
        guard let m = models.first(where: { $0.id == key }) else { return nil }
        let made = InstallModel(machine: m, app: self)
        installs[key] = made
        return made
    }

    func installIrix(_ key: String) {
        guard installModel(for: key) != nil else { return }
        installSheet = key
    }

    func forgetInstall(_ key: String) {
        installs[key] = nil
        if installSheet == key { installSheet = nil }
        if guideKey == key { guideKey = nil; sidebarGuide = false }
        models.first { $0.id == key }.map { $0.installing = false; $0.installConsole = false }
    }

    /// Closing a tab whose install server runs asks first, in the core's words; Keep Installing leaves the tab open.
    /// A running machine asks with the core's alert: closing the tab can leave it running.
    func closeTabAsking(_ id: String) {
        if let install = installs[id], installRunning(id) {
            if confirmStopInstall(false) { install.stopInstall { self.closeTab(id) } }
            return
        }
        guard let m = models.first(where: { $0.id == id }), m.isRunning else { closeTab(id); return }
        let alert = origamiAlert(kind: .closeTab(name: m.name), platform: thisPlatform)
        let i = present(alert)
        guard alert.buttons.indices.contains(i) else { return }
        switch alert.buttons[i].role {
        case .safe: closeTab(id)
        case .destructive: m.stop { [weak self] e in if let e { self?.stopFailed(m, e) } else { self?.closeTab(id) } }
        case .cancel: break
        }
    }

    /// Cancel every running server and force-stop its machine, for a quit that was confirmed.
    func stopInstalls(done: @escaping @MainActor () -> Void) {
        let running = installs.filter { installRunning($0.key) }.map { $0.value }
        var left = running.count
        if left == 0 { done(); return }
        for i in running { i.stopInstall { left -= 1; if left == 0 { done() } } }
    }

    /// Whether any assistant's media server is running, which quitting would stop.
    var installServing: Bool { installs.keys.contains { installRunning($0) } }

    /// Whether the machine's install is running and quitting or closing its tab would stop it.
    func installRunning(_ id: String) -> Bool { installs[id]?.busyInstalling ?? false }

    // MARK: quit

    /// The machines running now, from the live core and off the main thread: the library is
    /// re-read and every opened machine asked, so a stale entry for a stopped machine is not counted.
    func liveRunning(_ done: @escaping @MainActor ([MachineModel]) -> Void) {
        guard let host else { done([]); return }
        let opened = models.compactMap { m in m.session.map { (m.id, $0.machine) } }
        scanIssued += 1
        let issued = scanIssued
        DispatchQueue.global(qos: .userInitiated).async {
            let summaries = try? host.core.listMachines()
            var live = Set<String>()
            for (id, machine) in opened where [.starting, .running, .paused, .stopping].contains(machine.view().state) { live.insert(id) }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    if let summaries { self.applyListing(summaries, issued: issued, force: true) }
                    for m in self.models where m.session == nil && m.state == .leftRunning { live.insert(m.id) }
                    done(self.models.filter { live.contains($0.id) })
                }
            }
        }
    }

    /// What quitting does: nothing to ask when none runs; otherwise one prompt naming them.
    enum QuitPlan: Equatable { case quitNow, ask([String]) }
    func quitPlan(_ done: @escaping @MainActor (QuitPlan) -> Void) {
        liveRunning { up in done(up.isEmpty ? .quitNow : .ask(up.map(\.name))) }
    }

    /// Stop every running machine in turn. A machine that errors or takes longer
    /// than `bound` seconds is reported in `done` as failed, and the next one goes on.
    func stopAll(force: Bool = false, bound: TimeInterval = 60,
                 progress: ((String) -> Void)? = nil,
                 done: @escaping @MainActor (_ failed: [MachineModel]) -> Void) {
        liveRunning { list in self.stopEach(list, force: force, bound: bound, progress: progress, done: done) }
    }

    private func stopEach(_ list: [MachineModel], force: Bool, bound: TimeInterval, progress: ((String) -> Void)?,
                          done: @escaping @MainActor (_ failed: [MachineModel]) -> Void) {
        var failed: [MachineModel] = []
        func next(_ i: Int) {
            guard i < list.count else { done(failed); return }
            let m = list[i]
            progress?(origamiPowerOffProgress(name: m.name, force: force))
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

    /// Power Off did not finish: offer a force quit.
    func stopFailed(_ m: MachineModel, _ error: Error) {
        if confirmed(.powerOffFailed(names: [m.name])) { m.stop(force: true) }
    }
}
