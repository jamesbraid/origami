// The SGI cube, drawn by the core into a CAMetalLayer we host. It draws on request
// only: the display link runs while the cube moves and stops at rest. No pointer input.

import AppKit
import OrigamiFrame
import QuartzCore
import SwiftUI

/// What a cube shows and where.
enum CubeLook: Equatable {
    /// Studio with the chassis colour on the horizon, over the window, turning.
    case starting(chassis: UInt32)
    /// The scene for the appearance, at rest, or the running machine's screen.
    case about
    /// The studio look, slowly turning, over the window's own colour so no square shows.
    case empty
}

func sceneText(_ index: UInt32, _ field: Int32) -> String {
    var buf = [CChar](repeating: 0, count: 600)
    guard origami_cube_scene_text(index, UInt32(field), &buf, buf.count) > 0 else { return "" }
    return String(cString: buf)
}

final class CubeNSView: NSView {
    private(set) var cube: OpaquePointer?
    private var link: CADisplayLink?
    private var look: CubeLook = .about
    private var dark = true
    /// About's scene, picked from the clock once per About, when first needed; an
    /// appearance change updates only the tones.
    private lazy var aboutScene: UInt32 = {
        let now = Calendar.current.dateComponents([.hour, .minute], from: Date())
        return origami_cube_scene_for_time(UInt32(now.hour ?? 12), UInt32(now.minute ?? 0))
    }()
    /// The machine view whose screen the chrome reflects; only one that is presenting.
    private weak var reflected: MachineView?
    private var visible = true
    private var detached = false
    /// Offscreen draws count as presents only for the self-test, which has no screen.
    var countsOffscreen = false
    private(set) var ticks = 0
    /// Cubes attached to the core right now, for the self-test.
    static private(set) var attachedCount = 0
    var linkPaused: Bool { link?.isPaused ?? true }
    private var observers: [NSObjectProtocol] = []
    private(set) var presents = 0
    private(set) var lastError = ""
    var metalLayer: CAMetalLayer { layer as! CAMetalLayer }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        metalLayer.contentsScale = 1
        metalLayer.isOpaque = false
        metalLayer.backgroundColor = NSColor.clear.cgColor
        // Reduce Motion can change while the view is up.
        motionObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification, object: nil, queue: .main) { [weak self] _ in
            self?.applyStyle(); self?.present()
        }
    }
    required init?(coder: NSCoder) { fatalError() }
    private var motionObserver: NSObjectProtocol?
    deinit {
        for o in observers { NotificationCenter.default.removeObserver(o) }
        if let m = motionObserver { NSWorkspace.shared.notificationCenter.removeObserver(m) }
        detach()
    }

    override func makeBackingLayer() -> CALayer { CAMetalLayer() }
    override var isFlipped: Bool { true }
    /// A layer-backed view copies this onto its layer, so the layer is non-opaque only if the view says so.
    override var isOpaque: Bool { false }
    override var acceptsFirstResponder: Bool { false }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    private var pixelSize: (w: UInt32, h: UInt32) {
        let s = metalLayer.contentsScale
        return (UInt32(max(1, bounds.width * s)), UInt32(max(1, bounds.height * s)))
    }

    // MARK: style

    /// Off for now: About shows the BC scenes only. The machine reflection below is kept.
    private static let reflectsMachine = false

    /// `machine` is reflected only if enabled and presenting; otherwise the scene stays.
    func configure(look: CubeLook, dark: Bool, machine: MachineView?) {
        let screen = (Self.reflectsMachine && (machine?.isPresenting ?? false)) ? machine : nil
        let changed = look != self.look || dark != self.dark || screen !== reflected
        self.look = look; self.dark = dark
        let old = reflected
        reflected = screen
        guard changed, !detached else { return }
        applyStyle()
        if screen !== old { reflect() }
        present()
    }

    /// Give the core the machine to copy from, then have that view present once so a
    /// still screen is copied.
    private func reflect() {
        guard let cube else { return }
        let status = origami_cube_set_machine(cube, reflected?.session.handle, 0)
        if status != 0 { lastError = "cube set_machine: \(status)" }
        reflected?.redraw()
    }

    private func applyStyle() {
        guard let cube else { return }
        var s = origami_cube_style()
        s.size = UInt32(MemoryLayout<origami_cube_style>.size)
        s.reduce_motion = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? 1 : 0
        s.horizon_rgb = UInt32.max
        // The cube composites over its host. Metal gives the layer no premultiplied alpha, so the core
        // fills the view with `surround_rgb`, the host's own colour. Only About draws the granite tile.
        s.backdrop = UInt32(ORIGAMI_CUBE_BACKDROP_NONE)
        s.surround_rgb = Self.windowColour(dark: dark)
        switch look {
        case .starting(let chassis):
            s.surround_rgb = Granite.neutral.1   // the start card's granite, flat
            s.env = UInt32(ORIGAMI_CUBE_ENV_STUDIO)
            s.horizon_rgb = chassis
            s.spin = 1
        case .empty:
            s.env = UInt32(ORIGAMI_CUBE_ENV_STUDIO)
            s.light_look = dark ? 0 : 1
            s.spin = 1   // slowly turning; the view takes no pointer input
        case .about:
            // The band's granite is drawn behind the view, so the cube can sit in it with room around it;
            // the flat tone is the fallback where the layer is not transparent.
            s.surround_rgb = (dark ? Granite.neutral : Granite.light).1
            s.light_look = dark ? 0 : 1
            if reflected != nil {
                s.env = UInt32(ORIGAMI_CUBE_ENV_MACHINE)
            } else {
                s.env = UInt32(ORIGAMI_CUBE_ENV_SCENE)
                s.scene = aboutScene
            }
        }
        origami_cube_set_style(cube, &s)
    }

    /// The window background as 0xRRGGBB in the given appearance.
    static func windowColour(dark: Bool) -> UInt32 {
        var rgb: UInt32 = 0
        NSAppearance(named: dark ? .darkAqua : .aqua)?.performAsCurrentDrawingAppearance {
            let c = NSColor.windowBackgroundColor.usingColorSpace(.sRGB) ?? .black
            rgb = UInt32(c.redComponent * 255) << 16 | UInt32(c.greenComponent * 255) << 8 | UInt32(c.blueComponent * 255)
        }
        return rgb
    }

    // MARK: attach and draw

    private func attach() -> Bool {
        let size = pixelSize
        var surface = origami_surface()
        surface.size = UInt32(MemoryLayout<origami_surface>.size)
        surface.kind = UInt32(ORIGAMI_SURFACE_CA_METAL_LAYER)
        surface.window = Unmanaged.passUnretained(metalLayer).toOpaque()
        var out: OpaquePointer?
        let st = origami_cube_attach(&surface, size.w, size.h, Double(metalLayer.contentsScale), &out)
        guard st == 0, let out else { lastError = "cube attach: \(st)"; return false }
        cube = out
        Self.attachedCount += 1
        applyStyle()
        if reflected != nil { reflect() }
        link = displayLink(target: self, selector: #selector(tick))
        link?.add(to: .main, forMode: .common)
        link?.isPaused = true
        return true
    }

    /// Draw if the cube changed or moves; keep the display link going only while it moves.
    @discardableResult
    func present() -> (drawn: UInt32, moving: Bool) {
        // Attach only once the view is in a window, so the swapchain has the right size,
        // scale and screen.
        guard visible, !detached, let window else { return (0, false) }
        // Keep the layer at the screen's backing scale (2x on Retina) so the cube is drawn in physical pixels.
        if metalLayer.contentsScale != window.backingScaleFactor { resize() }
        if cube == nil && !attach() { return (0, false) }
        guard let cube else { return (0, false) }
        var drawn: UInt32 = 0, moving: UInt32 = 0
        let st = origami_cube_present(cube, &drawn, &moving)
        if st != 0 { lastError = "cube present: \(st)" }
        if drawn == UInt32(ORIGAMI_DRAWN_PRESENTED) || (countsOffscreen && drawn == UInt32(ORIGAMI_DRAWN_OCCLUDED)) { presents += 1 }
        link?.isPaused = moving == 0
        return (drawn, moving != 0)
    }

    /// Fade to the next scene in the manifest's About order (time of day).
    func stepScene() {
        guard let cube, origami_cube_next_scene(cube) == 0 else { return }
        present()
    }

    /// The scene on show, or -1 for the studio or a machine's screen.
    var currentScene: Int { cube.map { Int(origami_cube_current_scene($0)) } ?? -1 }

    @objc private func tick() { ticks += 1; present() }

    func detach() {
        detached = true
        link?.invalidate(); link = nil
        if let c = cube {
            origami_cube_set_machine(c, nil, 0)   // stop the copying from the machine's view
            origami_cube_detach(c); cube = nil; Self.attachedCount -= 1
        }
    }

    override func setFrameSize(_ s: NSSize) { super.setFrameSize(s); resize() }
    override func viewDidChangeBackingProperties() { super.viewDidChangeBackingProperties(); resize() }

    private func resize() {
        metalLayer.isOpaque = false   // a layer sync from the view can reset it
        let scale = window?.backingScaleFactor ?? metalLayer.contentsScale
        metalLayer.contentsScale = scale
        let size = pixelSize
        metalLayer.drawableSize = CGSize(width: Double(size.w), height: Double(size.h))
        if let cube { origami_cube_resize(cube, size.w, size.h, Double(scale)); present() }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        for o in observers { NotificationCenter.default.removeObserver(o) }
        observers = []
        resize()
        guard let window else { return }
        // A move to a screen of another scale redraws at that scale.
        observers.append(NotificationCenter.default.addObserver(
            forName: NSWindow.didChangeScreenNotification, object: window, queue: .main) { [weak self] _ in self?.resize() })
        // Hidden, minimised or occluded: draw nothing and take nothing from the swapchain.
        observers.append(NotificationCenter.default.addObserver(
            forName: NSWindow.didChangeOcclusionStateNotification, object: window, queue: .main) { [weak self] _ in
            guard let self, let w = self.window else { return }
            self.visible = w.occlusionState.contains(.visible)
            if let c = self.cube { origami_cube_set_visible(c, self.visible ? 1 : 0) }
            if self.visible { self.present() } else { self.link?.isPaused = true }
        })
        present()
    }

    // No pointer interaction: the cube turns on its own at start and is otherwise at rest.

    // MARK: for self-tests

    func readback() -> (width: Int, height: Int, bgra: [UInt8])? {
        guard let cube else { return nil }
        var w: UInt32 = 0, h: UInt32 = 0, stride: UInt32 = 0
        _ = origami_cube_readback(cube, nil, 0, &w, &h, &stride)
        guard w > 0, h > 0 else { return nil }
        var buf = [UInt8](repeating: 0, count: Int(stride) * Int(h))
        guard origami_cube_readback(cube, &buf, buf.count, &w, &h, &stride) == 0 else { return nil }
        return (Int(w), Int(h), buf)
    }

    func stats() -> origami_view_counters? {
        guard let cube else { return nil }
        var c = origami_view_counters()
        c.size = UInt32(MemoryLayout<origami_view_counters>.size)
        return origami_cube_stats(cube, &c) == 0 ? c : nil
    }
}

struct CubeHost: NSViewRepresentable {
    let look: CubeLook
    var machine: MachineModel?
    /// Told the view once it is made, for a host that steps the scene.
    var made: ((CubeNSView) -> Void)?
    @Environment(\.colorScheme) private var scheme

    func makeNSView(context: Context) -> CubeNSView {
        let v = CubeNSView(frame: NSRect(x: 0, y: 0, width: 160, height: 160))
        made?(v)
        v.configure(look: look, dark: scheme == .dark, machine: shown)
        return v
    }
    func updateNSView(_ v: CubeNSView, context: Context) {
        v.configure(look: look, dark: scheme == .dark, machine: shown)
    }
    private var shown: MachineView? { machine?.state == .running ? machine?.view : nil }
    static func dismantleNSView(_ v: CubeNSView, coordinator: ()) { v.detach() }
}
