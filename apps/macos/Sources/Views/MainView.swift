// The main window: the library in the sidebar, machine tabs, one machine's
// tab, a toolbar and a status bar.

import SwiftUI

struct MainView: View {
    @EnvironmentObject var app: AppModel
    @State private var settingsFor: MachineModel?
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        if let error = app.launchError {
            Card(symbol: "exclamationmark.triangle.fill", tint: .red, title: "Origami cannot start its core",
                 detail: error, actions: [("Quit", { NSApp.terminate(nil) })])
                .frame(minWidth: 560, minHeight: 360)
        } else {
            content
        }
    }

    private var visibility: Binding<NavigationSplitViewVisibility> {
        Binding(get: { app.sidebarVisible ? .all : .detailOnly },
                set: { app.sidebarVisible = ($0 != .detailOnly) })
    }

    /// The library, or the install guide once an install has started and the person has not switched back.
    @ViewBuilder private var sidebar: some View {
        if app.sidebarGuide, let key = app.guideKey, let install = app.installModel(for: key) {
            InstallGuideSidebar(model: install)
        } else {
            LibrarySidebar()
        }
    }

    private var content: some View {
        NavigationSplitView(columnVisibility: visibility) {
            sidebar
                .navigationSplitViewColumnWidth(min: 200, ideal: 240, max: 340)
        } detail: {
            VStack(spacing: 0) {
                if app.showsTabBar { TabBar() }
                if let model = app.current {
                    MachineTab(model: model).id(model.id)
                } else {
                    EmptyDetail()
                }
                if app.showsCpu, let model = app.current, model.state == .running { CpuPanelHost(model: model) }
                if let model = app.current { StatusBar(model: model) }
            }
            .overlay(alignment: .bottom) { ToastView().padding(.bottom, 40) }
            .navigationTitle(app.current?.name ?? ui(.origami))
            .navigationSubtitle(app.current?.word ?? "")
            .toolbar { MachineToolbar() }
        }
        .toolbar(removing: .sidebarToggle)
        .frame(minWidth: 820, minHeight: 560)
        .background(ToolbarTooltips().frame(width: 0, height: 0))
        .onAppear {
            app.openWindow = { openWindow(id: $0) }
            app.openConsole = { openWindow(id: "console", value: $0) }
        }
        .onChange(of: app.wantSettings) { _, want in
            if want { settingsFor = app.current; app.wantSettings = false }
        }
        .sheet(item: $settingsFor, onDismiss: { app.settingsPane = .general }) { model in MachineSettings(model: model, pane: app.settingsPane) }
        .sheet(isPresented: $app.wantNewMachine, onDismiss: { app.droppedDisk = nil }) { NewMachineSheet(app: app) }
        .sheet(isPresented: Binding(get: { app.installSheet != nil }, set: { if !$0 { app.installSheet = nil } })) {
            if let key = app.installSheet, let install = app.installModel(for: key) { InstallSetupSheet(model: install) }
        }
    }
}

struct EmptyDetail: View {
    @EnvironmentObject var app: AppModel
    var body: some View {
        let e = origamiEmptyState(kind: app.models.isEmpty ? .noMachines : .noneSelected)
        ContentUnavailableView {
            Label(e.title, systemImage: "desktopcomputer")
        } description: {
            Text(e.sentence)
        } actions: {
            if let button = e.button { Button(button) { app.invoke(.newMachine) } }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// A brief message over the window, with an action that shows a file.
struct ToastView: View {
    @EnvironmentObject var app: AppModel
    var body: some View {
        if let n = app.notice {
            HStack(spacing: 12) {
                Text(n.text).lineLimit(1)
                if let url = n.reveal, let action = n.action {
                    Button(action) { NSWorkspace.shared.activateFileViewerSelecting([url]) }.buttonStyle(.link)
                }
            }
            .font(.callout)
            .padding(.horizontal, 14).padding(.vertical, 8)
            .background(.regularMaterial, in: Capsule())
            .shadow(color: .black.opacity(0.2), radius: 6, y: 2)
            .transition(.move(edge: .bottom).combined(with: .opacity))
            .accessibilityAddTraits(.updatesFrequently)
        }
    }
}

// MARK: library

struct LibrarySidebar: View {
    @EnvironmentObject var app: AppModel

    var body: some View {
        List(selection: Binding(get: { app.active }, set: { if let id = $0 { app.select(id) } })) {
            Section(Labels.sidebarHeading) {
                // Rows are keyed by the machine's path, never by product or art.
                ForEach(app.models) { model in
                    LibraryRow(model: model).tag(model.id)
                }
            }
        }
        .listStyle(.sidebar)
        // Double-click opens a tab. A tap gesture on the row would delay and drop plain clicks.
        .contextMenu(forSelectionType: String.self) { ids in
            if let id = ids.first, let model = app.models.first(where: { $0.id == id }) {
                Button(ui(.openInNewTab)) { app.openInNewTab(id) }
                Divider()
                Button(origamiShowInLabel(platform: thisPlatform)) { NSWorkspace.shared.activateFileViewerSelecting([app.root.appendingPathComponent(model.name)]) }
                Button(ui(.diagnostics)) { app.select(id); app.invoke(.diagnostics) }
                Divider()
                Button(CommandID.remove.title) { app.remove(id) }.disabled(!model.canRemove)
            }
        } primaryAction: { ids in
            if let id = ids.first { app.openInNewTab(id) }
        }
        .dropDestination(for: URL.self) { urls, _ in app.dropOnLibrary(urls) }
        .safeAreaInset(edge: .bottom) {
            HStack {
                Button { app.invoke(.newMachine) } label: { Label(ui(.newMachine), systemImage: "plus") }
                    .buttonStyle(.borderless).disabled(!app.isEnabled(.newMachine))
                    .help(CommandID.newMachine.tooltip ?? "")
                Spacer()
                Button { app.invoke(.chooseLibrary) } label: { Label("Choose library folder", systemImage: "folder") }
                    .buttonStyle(.borderless).labelStyle(.iconOnly)
                    .help("Choose library folder…")
            }
            .padding(.horizontal, 12).padding(.vertical, 8)
            .background(.bar)
        }
        .onAppear { app.rescan() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in app.rescan() }
    }
}

/// Name, then the model or the state as a dot and a word: nothing trails the line.
struct LibraryRow: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        let line = model.line
        HStack(spacing: 8) {
            MachineArt(product: model.record.product, pt: Int(Tokens.Size.artSidebar))
            VStack(alignment: .leading, spacing: 1) {
                Text(model.name).lineLimit(1).truncationMode(.middle)
                HStack(spacing: 4) {
                    if line.tone != .none { Circle().fill(line.tone.colour).frame(width: 6, height: 6).accessibilityHidden(true) }
                    Text(line.text).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .contentShape(Rectangle())
        .help(model.rowDescription)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(model.rowDescription)
    }
}

// MARK: tabs

struct TabBar: View {
    @EnvironmentObject var app: AppModel
    var body: some View {
        HStack(spacing: 4) {
            ForEach(app.tabs, id: \.self) { id in
                if let model = app.models.first(where: { $0.id == id }) {
                    TabItem(model: model, active: app.active == id)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .frame(height: Tokens.Size.tabBarHeight)
        .background(Color(nsColor: .windowBackgroundColor))
    }
}

struct TabItem: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    let active: Bool
    @State private var hover = false

    var body: some View {
        let chassis = Color(rgb: model.record.product.colour)
        HStack(spacing: 6) {
            MachineArt(product: model.record.product, pt: Int(Tokens.Size.artTab))
            Text(model.name).lineLimit(1).truncationMode(.middle).font(.callout)
            // A stopped machine has no tone, so no dot.
            if model.tone != .none { Circle().fill(model.tone.colour).frame(width: 6, height: 6).accessibilityHidden(true) }
            // The close button shows on the selected tab and on hover; the space stays reserved.
            Button { app.closeTabAsking(model.id) } label: { Image(systemName: "xmark").font(.system(size: 9, weight: .bold)) }
                .buttonStyle(.borderless).help(CommandID.closeTab.title).accessibilityLabel(CommandID.closeTab.title)
                .opacity(active || hover ? 1 : 0)
        }
        .padding(.horizontal, 8)
        .frame(height: Tokens.Size.tabHeight)
        .background(RoundedRectangle(cornerRadius: 6).fill(active ? chassis.opacity(Tokens.Chassis.tabSelectedFill) : (hover ? Color.primary.opacity(0.07) : .clear)))
        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(active ? chassis.opacity(Tokens.Chassis.tabSelectedStroke) : .clear, lineWidth: 1))
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture { app.active = model.id }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(active ? [.isSelected, .isButton] : .isButton)
    }
}

// MARK: toolbar and status bar

/// The toolbar buttons' tooltips are the core's command tooltips, ending with the shortcut.
enum ToolbarTip {
    private static func tip(_ id: String) -> String { origamiCommandTooltip(id: id, platform: thisPlatform) ?? "" }
    /// By the toolbar item's label.
    static var byLabel: [String: String] {
        [ui(.showOrHideTheLibrary): ui(.showOrHideTheLibrary), ui(.start): tip("machine.start"), ui(.powerOff): tip("machine.stop"),
         ui(.captureMouse): tip("machine.capture-mouse"), ui(.console): tip("window.console"), ui(.cpuLoad): tip("view.cpu-load"), ui(.cancelStart): tip("machine.cancel-start"),
         ui(.diagnostics): tip("window.diagnostics"), ui(.settings): tip("machine.settings"), ui(.installIrix): ui(.installIrix)]
    }
    static var all: [String] { Array(byLabel.values) }
}

/// SwiftUI's `.help` does not reach the NSToolbarItem, so the items get their tooltips here.
struct ToolbarTooltips: NSViewRepresentable {
    final class TipView: NSView {
        override func viewDidMoveToWindow() { super.viewDidMoveToWindow(); apply() }
        func apply() {
            // The items may not exist yet at the first call, so look again shortly after.
            for delay in [0.0, 0.5, 1.5] {
                DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
                    for item in self?.window?.toolbar?.items ?? [] {
                        if let tip = ToolbarTip.byLabel[item.label] { item.toolTip = tip }
                    }
                }
            }
        }
    }
    func makeNSView(context: Context) -> TipView { TipView() }
    // The toolbar rebuilds its items when the machine or its state changes.
    func updateNSView(_ view: TipView, context: Context) { view.apply() }
}

struct MachineToolbar: ToolbarContent {
    @EnvironmentObject var app: AppModel

    // One item each: a tooltip on a button inside a shared group is not reliably shown.
    var body: some ToolbarContent {
        // The library button stays whether the library is open or closed.
        ToolbarItem(placement: .navigation) {
            Button { app.invoke(.showLibrary) } label: { Label(ui(.showOrHideTheLibrary), systemImage: "sidebar.left") }
                .help(ui(.showOrHideTheLibrary))
        }
        // While an install guide runs, a second button swaps the sidebar between the guide and the library.
        if app.guideKey != nil {
            ToolbarItem(placement: .navigation) {
                Toggle(isOn: $app.sidebarGuide) { Label(ui(.installIrix), systemImage: "list.number") }
                    .toggleStyle(.button).help(ui(.installIrix))
            }
        }
        // Items that need a machine are left out with it, so no empty capsule shows.
        if let m = app.current {
            ToolbarItem(placement: .primaryAction) { PowerControl(model: m) }
            ToolbarItem(placement: .primaryAction) { PointerControl(model: m) }
        }
        ToolbarItem(placement: .primaryAction) {
            Button { app.invoke(.console) } label: { Label(ui(.console), systemImage: "terminal") }
                .disabled(!app.isEnabled(.console)).help(CommandID.console.tooltip ?? "")
        }
        ToolbarItem(placement: .primaryAction) {
            Toggle(isOn: $app.showsCpu) { Label(ui(.cpuLoad), systemImage: "cpu") }
                .toggleStyle(.button).help(CommandID.cpuLoad.tooltip ?? "")
        }
        ToolbarItem(placement: .primaryAction) {
            Button { app.invoke(.diagnostics) } label: { Label(ui(.diagnostics), systemImage: "waveform.path.ecg") }
                .disabled(!app.isEnabled(.diagnostics)).help(CommandID.diagnostics.tooltip ?? "")
        }
        ToolbarItem(placement: .primaryAction) {
            Button { app.invoke(.settings) } label: { Label(ui(.settings), systemImage: "gearshape") }
                .disabled(!app.isEnabled(.settings)).help(CommandID.settings.tooltip ?? "")
        }
    }
}

/// The panel for a running machine; it polls only while it is on screen.
struct CpuPanelHost: View {
    @ObservedObject var model: MachineModel
    @StateObject private var load = CpuLoadModel()
    var body: some View {
        CpuPanel(model: load)
            .onAppear { load.start(model) }
            .onDisappear { load.stop() }
    }
}

/// Start, which becomes the Power menu once the machine is up: Power off on the button, the rest in the menu.
struct PowerControl: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        if model.canStart {
            Button { app.invoke(.start) } label: { Label(ui(.start), systemImage: "play.fill") }
                .help(CommandID.start.tooltip ?? "")
        } else if model.state == .starting {
            // A launch cannot be aborted, so Power off is not offered yet: the stop waits for it.
            Button { app.invoke(.cancelStart) } label: { Label(ui(.cancelStart), systemImage: "xmark.circle") }
                .help(CommandID.cancelStart.tooltip ?? "")
        } else {
            let paused = model.state == .paused
            Menu {
                Button(paused ? CommandID.resume.title : CommandID.pause.title) { app.invoke(paused ? .resume : .pause) }
                    .disabled(!app.isEnabled(paused ? .resume : .pause))
                Button(CommandID.reset.title) { app.invoke(.reset) }.disabled(!app.isEnabled(.reset))
                Divider()
                Button(CommandID.forceQuit.title) { app.invoke(.forceQuit) }.disabled(!app.isEnabled(.forceQuit))
            } label: { Label(ui(.powerOff), systemImage: "power") } primaryAction: { app.invoke(.powerOff) }
            .disabled(!app.isEnabled(.powerOff) && !app.isEnabled(.forceQuit))
            .help(CommandID.powerOff.tooltip ?? "")
        }
    }
}

/// One button: on, the mouse is captured on click; off, it follows the pointer.
struct PointerControl: View {
    @ObservedObject var model: MachineModel
    var body: some View {
        Toggle(isOn: Binding(get: { model.pointerMode == .capture }, set: { model.pointerMode = $0 ? .capture : .follow })) {
            Label(ui(.captureMouse), systemImage: "cursorarrow.click.2")
        }
        .toggleStyle(.button)
        .help(CommandID.captureInput.tooltip ?? "")
    }
}

/// The picture's size and the pointer's words, from the core, while the machine runs. Never a card's text.
struct StatusBar: View {
    @ObservedObject var model: MachineModel
    var body: some View {
        if let bar = model.statusBar, model.state == .running || model.state == .paused {
            HStack(spacing: 14) {
                if let picture = bar.picture { Text(picture) }
                Spacer()
                if let pointer = bar.pointer {
                    Label(pointer, systemImage: model.captured ? "cursorarrow.motionlines" : "cursorarrow")
                }
            }
            .font(.caption).foregroundStyle(.secondary)
            .padding(.horizontal, 12).frame(height: 24).background(.bar)
            .overlay(alignment: .top) { Divider() }
        }
    }
}

/// Help > Keyboard shortcuts: the core's groups, the release keys first.
struct ShortcutsView: View {
    var body: some View {
        Form {
            ForEach(origamiShortcutGroups(platform: thisPlatform), id: \.title) { group in
                Section(group.title) {
                    ForEach(group.items, id: \.label) { item in
                        LabeledContent(item.label) { Text(item.keys).font(.body.monospaced()) }
                    }
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 440, height: 520)
    }
}
