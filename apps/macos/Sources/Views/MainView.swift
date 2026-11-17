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

    private var content: some View {
        NavigationSplitView(columnVisibility: visibility) {
            LibrarySidebar()
                .navigationSplitViewColumnWidth(min: 200, ideal: 240, max: 340)
        } detail: {
            VStack(spacing: 0) {
                if app.showsTabBar { TabBar() }
                if let model = app.current {
                    MachineTab(model: model).id(model.id)
                } else {
                    EmptyDetail()
                }
                if let model = app.current { StatusBar(model: model) }
            }
            .navigationTitle(app.current?.name ?? "Origami")
            .toolbar { MachineToolbar() }
        }
        .frame(minWidth: 820, minHeight: 560)
        .onAppear { app.openWindow = { openWindow(id: $0) } }
        .onChange(of: app.wantSettings) { _, want in
            if want { settingsFor = app.current; app.wantSettings = false }
        }
        .sheet(item: $settingsFor) { model in MachineSettings(model: model) }
        .sheet(isPresented: $app.wantNewMachine) { NewMachineSheet(app: app) }
    }
}

struct EmptyDetail: View {
    @EnvironmentObject var app: AppModel
    var body: some View {
        VStack(spacing: 12) {
            // At rest: it draws one frame and costs nothing more.
            CubeHost(look: .empty).frame(width: 140, height: 140).accessibilityHidden(true)
            Text(app.models.isEmpty ? "No machines yet" : "Choose a machine")
                .font(.title2.weight(.semibold))
            if app.models.isEmpty {
                Text("Origami looks for machines in \((app.root.path as NSString).abbreviatingWithTildeInPath).")
                    .foregroundStyle(.secondary).multilineTextAlignment(.center).frame(maxWidth: 420)
                Button("Choose Library Folder…") { app.invoke(.chooseLibrary) }
            } else {
                Text("Pick one in the library.").foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

// MARK: library

struct LibrarySidebar: View {
    @EnvironmentObject var app: AppModel

    var body: some View {
        List(selection: Binding(get: { app.active }, set: { if let id = $0 { app.select(id) } })) {
            Section("Library") {
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
                Button("Open in New Tab") { app.openInNewTab(id) }
                Divider()
                Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([app.root.appendingPathComponent(model.name)]) }
                Button("Diagnostics") { app.select(id); app.invoke(.diagnostics) }
            }
        } primaryAction: { ids in
            if let id = ids.first { app.openInNewTab(id) }
        }
        .safeAreaInset(edge: .bottom) {
            HStack {
                Button { app.invoke(.newMachine) } label: { Label("New Machine", systemImage: "plus") }
                    .buttonStyle(.borderless).disabled(!app.isEnabled(.newMachine))
                    .help("New Machine (⌘N)")
                Spacer()
                Button { app.invoke(.chooseLibrary) } label: { Label("Choose Library Folder", systemImage: "folder") }
                    .buttonStyle(.borderless).labelStyle(.iconOnly)
                    .help("Choose Library Folder…")
            }
            .padding(.horizontal, 12).padding(.vertical, 8)
            .background(.bar)
        }
        .onAppear { app.rescan() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in app.rescan() }
    }
}

struct LibraryRow: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        HStack(spacing: 8) {
            MachineArt(product: model.record.product, pt: Int(Tokens.Size.artSidebar))
            VStack(alignment: .leading, spacing: 1) {
                Text(model.name).lineLimit(1)
                Text(model.record.product.name).font(.caption).foregroundStyle(.secondary)
            }
            Spacer(minLength: 4)
            StatusPill(state: model.state)
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(model.name), \(model.record.product.name), \(model.state.pill)")
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
            Text(model.name).lineLimit(1).font(.callout)
            Circle().fill(model.state.tone.colour).frame(width: 6, height: 6).accessibilityHidden(true)
            // The close button shows on the selected tab and on hover; the space stays reserved.
            Button { app.closeTab(model.id) } label: { Image(systemName: "xmark").font(.system(size: 9, weight: .bold)) }
                .buttonStyle(.borderless).help("Close Tab").accessibilityLabel("Close \(model.name)")
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

struct MachineToolbar: ToolbarContent {
    @EnvironmentObject var app: AppModel

    var body: some ToolbarContent {
        ToolbarItem(placement: .principal) { if let m = app.current { PillItem(model: m) } }
        ToolbarItemGroup(placement: .primaryAction) {
            if let m = app.current {
                PowerControl(model: m)
                PointerControl(model: m)
            }
            Button { app.invoke(.diagnostics) } label: { Label("Diagnostics", systemImage: "waveform.path.ecg") }
                .disabled(!app.isEnabled(.diagnostics)).help("Diagnostics")
            Button { app.invoke(.settings) } label: { Label("Settings", systemImage: "gearshape") }
                .disabled(!app.isEnabled(.settings)).help("Settings")
        }
    }
}

struct PillItem: View {
    @ObservedObject var model: MachineModel
    var body: some View { StatusPill(state: model.state) }
}

struct PowerControl: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        if model.canStart {
            Button { app.invoke(.start) } label: { Label("Start", systemImage: "play.fill") }
                .help("Start (⌘R)")
        } else {
            Menu {
                Button("Power Off") { app.invoke(.powerOff) }.disabled(!app.isEnabled(.powerOff))
                Button("Pause") { app.invoke(.pause) }.disabled(!app.isEnabled(.pause))
                Button("Resume") { app.invoke(.resume) }.disabled(!app.isEnabled(.resume))
                Button("Save Screenshot") { app.invoke(.screenshot) }.disabled(!app.isEnabled(.screenshot))
                Button("Reset…") { app.invoke(.reset) }.disabled(!app.isEnabled(.reset))
                Divider()
                Button("Force Quit Emulator…") { app.invoke(.forceQuit) }.disabled(!app.isEnabled(.forceQuit))
            } label: { Label("Power", systemImage: "power") }
            .help("Power")
        }
    }
}

struct PointerControl: View {
    @ObservedObject var model: MachineModel
    var body: some View {
        Picker("Pointer", selection: $model.pointerMode) {
            Label("Follow my mouse", systemImage: "cursorarrow").tag(PointerMode.follow)
            Label("Capture on click", systemImage: "cursorarrow.click.2").tag(PointerMode.capture)
        }
        .pickerStyle(.segmented)
        .help(model.pointerMode == .follow ? "The guest's pointer follows your mouse" : "Click the screen to capture the mouse; ⌃⌥ releases")
        .accessibilityLabel("Pointer mode")
    }
}

struct StatusBar: View {
    @ObservedObject var model: MachineModel
    @EnvironmentObject var app: AppModel
    var body: some View {
        HStack(spacing: 14) {
            StatusPill(state: model.state)
            Text(model.statusLine).foregroundStyle(.secondary).lineLimit(1)
            if let n = app.notice {
                Text(n.text).lineLimit(1)
                if let url = n.reveal {
                    Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([url]) }.buttonStyle(.link)
                }
            }
            Spacer()
            if model.state == .running {
                Label(model.pointerMode == .follow ? "Follows mouse" : (model.captured ? "Captured" : "Click to capture"),
                      systemImage: model.captured ? "cursorarrow.motionlines" : "cursorarrow")
                    .help(model.pointerMode == .follow ? "The guest's pointer follows your mouse" : "Click the screen to capture the mouse; press ⌃⌥ to release")
            }
        }
        .font(.caption)
        .padding(.horizontal, 12).frame(height: 24).background(.bar)
        .overlay(alignment: .top) { Divider() }
    }
}
