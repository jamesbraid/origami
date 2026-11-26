// The Install IRIX assistant's calls and operations. The core's OrigamiInstallFlow holds the choices and the step
// rules; this runs the work off the main actor and tells the views to redraw.

import AppKit
import Combine
import Foundation
import SwiftUI

@MainActor
final class InstallModel: ObservableObject {
    let machine: MachineModel
    private let app: AppModel
    private(set) var flow = OrigamiInstallFlow(plan: nil)
    let texts = origamiInstallTexts()
    @Published private(set) var folderResult: OrigamiMediaFolder?
    @Published private(set) var folderProblem: String?
    /// The system disk, once read; nil with `diskKnown` when the machine has none.
    @Published private(set) var disk: OrigamiInstallDisk?
    @Published private(set) var diskKnown = false
    @Published var full = false
    @Published private(set) var checkMessage: String?
    /// Warnings and advice from preparing the machine.
    @Published private(set) var notes: [String] = []
    @Published private(set) var guide: OrigamiInstallGuide?
    /// Guide steps the person marked done, for a graphics console that cannot confirm them.
    @Published private(set) var doneSteps = Set<Int>()
    /// How many leading guide steps the serial console has shown done.
    @Published private(set) var seen = 0
    private var askingSeen = false
    /// The setup the install runs, once prepared: the footer's words come from it.
    @Published private(set) var plan: OrigamiInstallPlan?
    /// The media server's latest line, and the core's sentence if it stopped.
    @Published private(set) var footer = ""
    @Published private(set) var serveFailed: String?
    @Published private(set) var planText: String?
    @Published private(set) var loaded = false
    @Published private(set) var working = false
    @Published private(set) var error: String?
    private(set) var serial = false
    private var serve: OrigamiOperation?
    private var checkOp: OrigamiOperation?
    private var applyOp: OrigamiOperation?
    /// The automated install's latest console line, how far along it says it is, and the core's sentence if it failed.
    @Published private(set) var applyLine = ""
    @Published private(set) var applyFraction: Double?
    @Published private(set) var applyMessage: String?
    private var cancelRequested = false
    private var sawRunning = false
    private(set) var closing = false
    private var asking = false
    private var generation = 0
    private var ticker: Timer?
    private var forward: AnyCancellable?
    /// The window closes through this; the view sets it.
    var onClose: (() -> Void)?
    /// The one place a file or folder is chosen. The self-test replaces it.
    var choosePath: (_ prompt: String, _ folder: Bool) -> URL? = { PathPicker.choose($0, folder: $1) }
    /// Asked with the core's heading, text, buttons and default: the index pressed. Tests answer it themselves.
    var ask: (_ title: String, _ detail: String, _ buttons: [String], _ defaultIndex: Int) -> Int = { title, detail, buttons, d in
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = detail
        for b in buttons { alert.addButton(withTitle: b) }
        // Return takes the core's default (never the destructive button); Escape takes the last.
        for (i, b) in alert.buttons.enumerated() { b.keyEquivalent = i == d ? "\r" : i == buttons.count - 1 ? "\u{1b}" : "" }
        return alert.runModal().rawValue - NSApplication.ModalResponse.alertFirstButtonReturn.rawValue
    }

    init(machine: MachineModel, app: AppModel) {
        self.machine = machine
        self.app = app
        // The footer and Finish follow the machine's state.
        forward = machine.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() }
        load()
    }

    /// Change the flow, then redraw.
    func edit(_ change: (OrigamiInstallFlow) -> Void) { objectWillChange.send(); change(flow) }
    func addExtra(source: String, packages: String) { edit { _ = $0.addExtra(source: source, selections: packages) } }

    // MARK: reading what the machine has

    /// An existing setup opens the assistant at Disk; the boot console says whether Type It applies.
    private func load() {
        let host = machine.coreHost, key = machine.record.key
        background({ () throws -> (OrigamiInstallPlan?, Bool, Bool) in
            (try host.core.installPlan(key: key), try host.core.openMachine(key: key).bootConsole() == .serial,
             try host.core.installRestorePending(key: key))
        }) { result in
            self.loaded = true
            switch result {
            case .success(let (plan, serial, pending)):
                self.serial = serial
                // A machine left on the installation network offers to set it back on close.
                guard let plan else { self.edit { $0.setSerial(serial: serial); if pending { $0.setPrepared() } }; return }
                self.objectWillChange.send(); self.flow = OrigamiInstallFlow(plan: plan); self.flow.setSerial(serial: serial)
                if pending { self.flow.setPrepared() }
                self.planText = plan.summary
                self.entered()
            case .failure(let e): self.error = Self.say(e)
            }
        }
    }

    private func readDisk() {
        let host = machine.coreHost, key = machine.record.key
        background({ try host.core.installDisk(key: key) }) { result in
            switch result {
            case .success(let d):
                self.disk = d; self.diskKnown = true
                self.edit { $0.setDisk(disk: d.map { .present(disk: $0) } ?? OrigamiDiskState.none) }
            case .failure(let e): self.error = Self.say(e)
            }
        }
    }

    private func loadChoices() {
        let host = machine.coreHost, key = machine.record.key, profile = flow.profile()
        background({ try host.core.installAddonChoices(key: key, profile: profile) }) { result in
            switch result {
            case .success(let c): self.edit { $0.setChoices(choices: c) }
            case .failure(let e): self.error = Self.say(e)
            }
        }
    }

    private func loadGuide() {
        guard guide == nil else { return }
        let host = machine.coreHost, key = machine.record.key
        background({ try host.core.installGuide(key: key) }) { result in
            switch result {
            case .success(let g): self.guide = g
            case .failure(let e): self.error = Self.say(e)
            }
        }
    }

    // MARK: steps

    var step: OrigamiInstallStep { flow.step() }
    var canBack: Bool { flow.canBack() && !working && flow.check() != .running }
    var canContinue: Bool { !working && flow.canContinue(machineUp: machine.isRunning) }
    /// Why Continue is off, in the core's words; none while it is on.
    var continueReason: String? { flow.continueReason(machineUp: machine.isRunning) }

    func back() {
        guard canBack else { return }
        edit { $0.back() }
        entered()
    }

    func next() {
        guard canContinue else { return }
        if step == .finish { finish(); return }
        if step == .disk, disk == nil { createDisk(); return }
        edit { $0.next() }
        entered()
    }

    /// What each step does as it opens.
    private func entered() {
        error = nil
        switch step {
        case .media: if let f = flow.folder() { checkFolder(URL(fileURLWithPath: f)) }
        case .software: loadChoices()
        case .disk: readDisk()
        case .prepare: startServing(); loadGuide(); beginGuide()
        case .installer, .packages: loadGuide(); beginGuide()
        case .finish: beginGuide()
        case .release, .method, .check: break
        }
    }

    /// The pages that follow the machine's start: their steps are the guide in the sidebar.
    var inGuide: Bool { [.prepare, .installer, .packages, .finish].contains(step) }

    /// Set-up is done: the sheet gives way to the guide in the sidebar, beside the machine.
    private func beginGuide() {
        guard inGuide else { return }
        machine.installing = true
        machine.installConsole = serial
        app.installSheet = nil
        app.guideKey = machine.id
        app.sidebarGuide = true
        app.sidebarVisible = true
        app.select(machine.id)
    }

    func selectRelease(_ r: String) { folderResult = nil; edit { $0.selectRelease(release: r) } }
    func selectProfile(_ p: String) { folderResult = nil; edit { $0.selectProfile(profile: p) } }

    // MARK: media

    func chooseFolder() {
        guard let url = choosePath(ui(.chooseFolder), true) else { return }
        checkFolder(url)
    }

    /// Read the folder, and tell the flow what was in it.
    private func checkFolder(_ url: URL) {
        folderResult = nil; folderProblem = nil
        generation += 1
        let mine = generation, release = flow.release(), profile = flow.profile()
        background({ try origamiInstallMediaFolder(path: url.path, release: release, profile: profile) }) { result in
            guard mine == self.generation else { return }
            switch result {
            case .success(let m): self.folderResult = m; self.edit { $0.setFolder(path: url.path, found: m) }
            case .failure(let e): self.folderProblem = Self.say(e)
            }
        }
    }

    var folderVerdict: String? { folderResult?.verdict ?? folderProblem }

    /// green for a complete folder, amber for a partial one, red for none or an unreadable one.
    var folderTone: FolderTone {
        guard let m = folderResult else { return .red }
        return m.complete ? .green : m.found > 0 ? .amber : .red
    }
    enum FolderTone { case green, amber, red }

    // MARK: disk

    private func createDisk() {
        let host = machine.coreHost, key = machine.record.key
        let open = machine.session?.machine
        let mib = UInt64(max(1, flow.newDiskGb())) * 1024
        working = true; error = nil
        background({
            let m = try open ?? host.core.openMachine(key: key)
            try m.applyChange(change: .createDisk(sizeMib: mib))
            return try host.core.installDisk(key: key)
        }) { result in
            self.working = false
            switch result {
            case .success(let d):
                self.disk = d
                self.edit { $0.setDisk(disk: d.map { .present(disk: $0) } ?? OrigamiDiskState.none); $0.next() }
                self.entered()
            case .failure(let e): self.error = Self.say(e)
            }
        }
    }

    // MARK: check

    /// Prepare the machine (unless it was already) and then check the media; cancelling stops either.
    func runCheck() {
        guard flow.check() != .running else { return }
        edit { $0.setCheck(state: .running) }
        error = nil; checkMessage = nil; notes = []; cancelRequested = false
        let host = machine.coreHost, key = machine.record.key, full = self.full
        let prepare = flow.resumed() ? nil : flow.setup()
        DispatchQueue.global(qos: .userInitiated).async {
            var lines: [OrigamiReportLine] = []
            var outcome = OrigamiCheckState.ready
            var message: String?
            do {
                if let prepare { lines = try host.core.installPrepare(key: key, setup: prepare) }
                DispatchQueue.main.async { MainActor.assumeIsolated { if prepare != nil { self.flow.setPrepared() } } }
                let op = try host.core.installCheck(key: key, full: full)
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { self.checkOp = op; if self.cancelRequested { op.cancel() } }
                }
                switch op.wait(timeoutMs: nil) {
                case .succeeded: outcome = .passed
                case .failed(_, let m): outcome = .failed; message = m
                default: outcome = .ready
                }
            } catch { outcome = .failed; message = Self.say(error) }
            let shown = lines.filter { $0.kind == .warning || $0.kind == .advice }.compactMap(\.shown)
            let (result, said) = (outcome, message)
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.checkOp = nil; self.notes = shown; self.checkMessage = said
                    self.edit { $0.setCheck(state: result) }
                }
            }
        }
    }

    // MARK: automated install

    /// Type the package commands at Inst>; the operation ends when the machine has stopped or it failed.
    func install() {
        guard flow.canInstall(machineUp: machine.isRunning) else { return }
        edit { $0.setApply(state: .running) }
        applyLine = ""; applyFraction = nil; applyMessage = nil; error = nil
        let host = machine.coreHost, key = machine.record.key
        DispatchQueue.global(qos: .userInitiated).async {
            var state = OrigamiApplyState.running
            var message: String?
            do {
                let op = try host.core.installApply(key: key)
                DispatchQueue.main.async { MainActor.assumeIsolated { self.applyOp = op } }
                switch op.wait(timeoutMs: nil) {
                case .succeeded: state = .done
                case .failed(_, let m): state = .failed(kind: op.installFailure() ?? .setup); message = m
                default: state = .failed(kind: .cancelled)
                }
            } catch { state = .failed(kind: .setup); message = Self.say(error) }
            let (result, said) = (state, message)
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.applyOp = nil; self.applyMessage = said
                    self.edit { $0.setApply(state: result) }
                }
            }
        }
    }

    func cancelInstall() { applyOp?.cancel() }

    /// The person fixed it at Inst> and takes over from the guide's restart list.
    func fixed() { edit { $0.fixed() } }

    /// The last NN% in a console line, as 0 to 1.
    nonisolated static func percent(in line: String) -> Double? {
        guard let r = line.range(of: #"(\d{1,3})%(?!.*\d%)"#, options: .regularExpression),
              let n = Double(line[r].dropLast()) else { return nil }
        return min(n, 100) / 100
    }

    func cancelCheck() {
        cancelRequested = true
        checkOp?.cancel()
    }

    // MARK: serving

    var serving: Bool { serve?.state() == .running }
    /// The server or the automated install is running.
    var busyInstalling: Bool { serving || flow.apply() == .running }

    /// Serve the media, then start the machine. Going back to this step keeps a running server.
    private func startServing() {
        if serving { startMachine(); return }
        serveFailed = nil; footer = ""
        let host = machine.coreHost, key = machine.record.key
        background({ try host.core.installServe(key: key) }) { result in
            switch result {
            case .success(let op):
                self.serve = op
                self.loadPlan()
                self.ticker?.invalidate()
                self.ticker = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
                    MainActor.assumeIsolated { self?.tick() }
                }
                self.startMachine()
            case .failure(let e): self.serveFailed = Self.say(e)
            }
        }
    }

    func restartServe() { serve = nil; startServing() }

    private func loadPlan() {
        let host = machine.coreHost, key = machine.record.key
        background({ try host.core.installPlan(key: key) }) { result in
            if case .success(let p) = result { self.plan = p }
        }
    }

    /// The footer while the install files are served, in the core's words; it never shows an address.
    /// While the machine loads the installer it says how far.
    var installFooter: String? {
        guard serving, serveFailed == nil else { return nil }
        if serve?.fraction() != nil { return footer }
        return plan.map { origamiInstallFooter(plan: $0) }
    }

    func startMachine() {
        machine.start { e in if let e { self.error = Self.say(e) } }
    }

    private func tick() {
        if let s = serve {
            let line = s.progress()
            if footer != line { footer = line }
            if case .failed(_, let message) = s.state(), serveFailed != message { serveFailed = message }
        }
        if let op = applyOp {
            let line = op.progress()
            if applyLine != line { applyLine = line; applyFraction = Self.percent(in: line) ?? applyFraction }
        }
        if machine.state == .running { sawRunning = true }
        askConsole()
    }

    // MARK: guide progress

    /// The steps the list shows: all of them, or with the automatic method those before it takes over.
    var listedSteps: [OrigamiGuideStep] { guide.map { flow.automatic() ? $0.prepare + $0.installer : $0.steps } ?? [] }

    func isDone(_ i: Int) -> Bool { doneSteps.contains(i) || (serial && i < seen) }

    /// How many leading steps are done: confirmed from the serial console, or marked done by hand.
    var progress: Int {
        var n = 0
        while n < listedSteps.count, isDone(n) { n += 1 }
        return n
    }

    /// A graphics console cannot confirm a step, so the person does.
    func markDone(_ i: Int) { doneSteps.insert(i); advanceWithProgress() }

    /// Ask the core how far the serial console shows the guide done. Asked on each tick while the guide shows.
    private func askConsole() {
        guard serial, inGuide, let g = guide, !askingSeen, let m = machine.session?.machine else { return }
        askingSeen = true
        let steps = g.steps
        DispatchQueue.global(qos: .utility).async {
            let n = Int(m.guideSeen(steps: steps))
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.askingSeen = false
                    if n > self.seen { self.seen = n; self.advanceWithProgress() }
                }
            }
        }
    }

    /// The flow's pages follow the list: a page ends when its last step is done. The automatic method's
    /// install page ends when the install does, not by the list.
    private func advanceWithProgress() {
        while let end = pageEnd, progress >= end {
            edit { $0.next() }
            entered()
        }
    }

    /// The step number the current page ends at: its last step done ends it. None for a page the list does not end.
    private var pageEnd: Int? {
        guard let g = guide else { return nil }
        let prepare = g.prepare.count, installer = prepare + g.installer.count
        switch step {
        case .prepare: return prepare
        case .installer: return installer
        case .packages: return flow.automatic() ? nil : installer + g.packages.count + g.restart.count
        default: return nil
        }
    }

    /// Continue in the guide: the steps of this page count as done and the next page opens; Finish and the
    /// automatic install go by the flow's own rules.
    func continueGuide() {
        guard canContinue else { return }
        if let end = pageEnd { for i in 0..<end { doneSteps.insert(i) }; advanceWithProgress() } else { next() }
    }

    /// For the self-test: the console has shown `n` steps done.
    func markSeenForTest(_ n: Int, serial isSerial: Bool = true) { serial = isSerial; seen = n }

    /// The machine ended while the guide was being followed.
    var stoppedBySelf: Bool {
        [.prepare, .installer, .packages].contains(step) && sawRunning && !machine.isRunning && machine.busy == nil
            && !(flow.automatic() && flow.apply() != .idle)
    }

    // MARK: following the guide

    /// Type It: a serial PROM console on a running machine.
    var canTypeIt: Bool { serial && machine.isRunning }

    func typeIt(_ text: String) {
        guard let m = machine.session?.machine else { error = "The machine is not running."; return }
        DispatchQueue.global(qos: .userInitiated).async {
            if let e = (Result { try m.consoleSend(bytes: Data(text.utf8)) }).failure {
                DispatchQueue.main.async { MainActor.assumeIsolated { self.error = Self.say(e) } }
            }
        }
    }

    // MARK: finishing and leaving

    private func finish() {
        working = true; error = nil
        restoreNetwork { failure in
            self.working = false
            if let failure { self.error = failure; return }
            self.serve?.cancel()
            self.app.announce(self.texts.finished)
            self.close()
        }
    }

    /// Put the network back as it was; the core's advice and warnings are shown as Settings shows them.
    private func restoreNetwork(_ done: @escaping @MainActor (String?) -> Void) {
        let host = machine.coreHost, key = machine.record.key
        background({ try host.core.installRestoreNetwork(key: key) }) { result in
            if case .success(let lines) = result, let note = lines.compactMap(\.shown).first { self.app.announce(note) }
            done(result.failure.map { Self.say($0) })
        }
    }

    /// Stop the server, forget the setup and begin again from the release. A running machine refuses the reset.
    func startOver() {
        let host = machine.coreHost, key = machine.record.key
        serve?.cancel(); serve = nil; footer = ""
        working = true; error = nil
        background({ try host.core.installReset(key: key) }) { result in
            self.working = false
            if let e = result.failure { self.error = Self.say(e); return }
            self.applyMessage = nil; self.guide = nil; self.doneSteps = []; self.seen = 0; self.plan = nil; self.notes = []; self.planText = nil; self.disk = nil; self.diskKnown = false
            self.edit { $0.restart() }
            self.entered()
        }
    }

    /// The core's machine question, then the server, then its network question, then the window.
    func requestClose() {
        guard !asking, !closing else { return }
        asking = true
        guard let q = flow.closeMachineQuestion(machineUp: machine.isRunning) else { cancelServer(); closeNetwork(); return }
        switch pick(q) {
        case .stay: asking = false
        case .stopMachine: cancelServer(); machine.stop(force: true) { _ in self.closeNetwork() }
        default: cancelServer(); closeNetwork()
        }
    }

    private func cancelServer() { cancelCheck(); cancelInstall(); serve?.cancel() }

    /// Re-read the network, then ask whether to put it back.
    private func closeNetwork() {
        let host = machine.coreHost, key = machine.record.key
        background({ try host.core.openMachine(key: key).settings().network.mode == "private" }) { result in
            let priv = (try? result.get()) ?? false
            if let q = self.flow.closeNetworkQuestion(machineUp: self.machine.isRunning, networkPrivate: priv), self.pick(q) == .setBack {
                self.restoreNetwork { failure in
                    if let failure { self.app.announce("The network was not set back: \(failure)") }
                    self.close()
                }
            } else { self.close() }
        }
    }

    private func pick(_ q: OrigamiCloseQuestion) -> OrigamiCloseAction {
        let i = ask(q.heading, q.text, q.buttons.map(\.label), Int(q.defaultButton))
        return q.buttons.indices.contains(i) ? q.buttons[i].action : .stay
    }

    /// Quit or a tab close that stops the install: cancel the server and force-stop the machine.
    func stopInstall(done: @escaping @MainActor () -> Void) {
        cancelServer()
        machine.stop(force: true) { _ in self.close(); done() }
    }

    /// The window is gone or going: stop watching and let the app forget this assistant.
    func close(dismissing: Bool = true) {
        closing = true; asking = false
        ticker?.invalidate(); ticker = nil
        app.forgetInstall(machine.id)
        if dismissing { onClose?() }
    }

    // MARK: plumbing

    private func background<T>(_ work: @escaping @Sendable () throws -> T, done: @escaping @MainActor (Result<T, Error>) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let result = Result { try work() }
            DispatchQueue.main.async { MainActor.assumeIsolated { done(result) } }
        }
    }

    /// The core's sentence unchanged, with Busy told as what to do about it.
    nonisolated static func say(_ e: Error) -> String { e is CoreError ? SettingsModel.say(e) : e.localizedDescription }
}

private extension Result {
    var failure: Failure? { if case .failure(let e) = self { return e } else { return nil } }
}
