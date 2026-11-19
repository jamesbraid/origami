//! The SGI cube drawn offscreen by the core's renderer: GTK 4 hosts no native
//! child surface, so each frame is copied out as a texture.

use crate::granite::tone;
use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};
use origami_render::{scenes, Backdrop, CubeView, Env, Gpu, Pointer, Style};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::OnceLock;

/// The device lives only while a cube is on screen: it is held weakly here and
/// made again when the next one maps. An idle Vulkan device costs the driver's
/// threads a few wake-ups a second, which a cube nobody sees should not.
fn gpu() -> Option<Rc<Gpu>> {
    thread_local!(static GPU: RefCell<Weak<Gpu>> = const { RefCell::new(Weak::new()) });
    if let Some(g) = GPU.with(|g| g.borrow().upgrade()) {
        return Some(g);
    }
    let g = match Gpu::new() {
        Ok(g) if g.is_software() => {
            eprintln!("origami-gtk: no cube on a software GPU ({})", g.describe());
            return None;
        }
        Ok(g) => Rc::new(g),
        Err(e) => {
            eprintln!("origami-gtk: no GPU for the cube: {e}");
            return None;
        }
    };
    GPU.with(|w| *w.borrow_mut() = Rc::downgrade(&g));
    Some(g)
}

/// Whether this host has a GPU the cube can use, found once; the probe's
/// device is dropped at once.
fn usable() -> bool {
    static USABLE: OnceLock<bool> = OnceLock::new();
    *USABLE.get_or_init(|| gpu().is_some())
}

/// The start screen's cube: the studio with the machine's colour on its horizon,
/// turning at 10 rpm over the page's own background.
pub fn starting_style(chassis: u32) -> Style {
    Style {
        env: Env::Studio,
        horizon: Some(tone(chassis)),
        backdrop: Backdrop::None,
        spin: true,
        ..Style::default()
    }
}

/// The scene for the user's local time, read once when About opens.
pub fn about_scene() -> usize {
    let now = glib::DateTime::now_local().ok();
    let (h, m) = now.map_or((12, 0), |d| (d.hour() as u32, d.minute() as u32));
    scenes::for_time(h, m)
}

/// About's cube: `scene` over a band the scene colours, at rest until dragged,
/// when it coasts and eases back; the tones follow `dark`.
pub fn about_style(dark: bool, scene: usize) -> Style {
    Style {
        env: Env::Scene(scene),
        backdrop: Backdrop::Band,
        light_look: !dark,
        settle: true,
        fill: 0.6,
        ..Style::default()
    }
}

mod imp {
    use super::*;

    /// A widget of a fixed size in points that shows a texture of any pixel size.
    #[derive(Default)]
    pub struct CubeArea {
        pub texture: RefCell<Option<gdk::Texture>>,
        pub size: Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CubeArea {
        const NAME: &'static str = "OrigamiCubeArea";
        type Type = super::CubeArea;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for CubeArea {}
    impl WidgetImpl for CubeArea {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (w, h) = self.size.get();
            let n = if orientation == gtk::Orientation::Horizontal {
                w
            } else {
                h
            };
            (n, n, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(t) = self.texture.borrow().as_ref() {
                let o = self.obj();
                let rect = graphene::Rect::new(0.0, 0.0, o.width() as f32, o.height() as f32);
                snapshot.append_texture(t, &rect);
            }
        }
    }
}

glib::wrapper! {
    pub struct CubeArea(ObjectSubclass<imp::CubeArea>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CubeArea {
    fn new(w: i32, h: i32) -> CubeArea {
        let a: CubeArea = glib::Object::builder().build();
        a.imp().size.set((w, h));
        a.set_focusable(false);
        a.set_accessible_role(gtk::AccessibleRole::Presentation);
        a
    }

    fn show(&self, t: &gdk::Texture) {
        *self.imp().texture.borrow_mut() = Some(t.clone());
        self.queue_draw();
    }
}

/// A cube's renderer, made on map and dropped on unmap (the view before the
/// device it draws with).
struct Live {
    view: CubeView,
    gpu: Rc<Gpu>,
}

struct Inner {
    live: Option<Live>,
    style: Style,
    tick: Option<gtk::TickCallbackId>,
    size: (i32, i32),
}

/// A cube view `width` by `height` points.
pub struct Cube {
    pub widget: CubeArea,
    inner: Rc<RefCell<Inner>>,
}

impl Cube {
    /// None when the machine has no GPU the renderer can use.
    pub fn new(width: i32, height: i32, style: Style) -> Option<Cube> {
        if !usable() {
            return None;
        }
        let widget = CubeArea::new(width, height);
        widget.set_halign(gtk::Align::Center);
        let inner = Rc::new(RefCell::new(Inner {
            live: None,
            style,
            tick: None,
            size: (width, height),
        }));
        let cube = Cube { widget, inner };
        cube.wire();
        Some(cube)
    }

    /// Restyle the cube; it draws once for the change.
    pub fn set_style(&self, style: Style) {
        let mut i = self.inner.borrow_mut();
        i.style = style;
        if let Some(Live { view, gpu }) = i.live.as_mut() {
            view.set_style(gpu, reduced(style));
            drop(i);
            kick(&self.widget, &self.inner);
        }
    }

    /// A handle that steps to the next scene, holding the cube only weakly.
    pub fn stepper(&self) -> impl Fn() + 'static {
        let (widget, inner) = (self.widget.downgrade(), self.inner.clone());
        move || {
            let Some(w) = widget.upgrade() else { return };
            if let Some(Live { view, gpu }) = inner.borrow_mut().live.as_mut() {
                view.step_scene(gpu);
            }
            kick(&w, &inner);
        }
    }

    /// Dragging turns the cube and a double click sends it home. A press with
    /// Shift held is left to the host.
    pub fn enable_pointer(&self) {
        let drag = gtk::GestureDrag::new();
        let begun = Rc::new(Cell::new(None::<(f64, f64)>));
        let send = {
            let (widget, inner) = (self.widget.downgrade(), self.inner.clone());
            move |phase: Pointer, x: f64, y: f64, ms: u32| {
                let Some(w) = widget.upgrade() else { return };
                if let Some(Live { view, .. }) = inner.borrow_mut().live.as_mut() {
                    view.pointer(phase, x as f32, y as f32, u64::from(ms));
                }
                kick(&w, &inner);
            }
        };
        drag.connect_drag_begin({
            let (begun, send) = (begun.clone(), send.clone());
            move |g, x, y| {
                if g.current_event_state()
                    .contains(gdk::ModifierType::SHIFT_MASK)
                {
                    return;
                }
                begun.set(Some((x, y)));
                send(Pointer::Press, x, y, g.current_event_time());
            }
        });
        drag.connect_drag_update({
            let (begun, send) = (begun.clone(), send.clone());
            move |g, dx, dy| {
                if let Some((x, y)) = begun.get() {
                    send(Pointer::Move, x + dx, y + dy, g.current_event_time());
                }
            }
        });
        drag.connect_drag_end({
            let (begun, send) = (begun.clone(), send.clone());
            move |g, dx, dy| {
                if let Some((x, y)) = begun.take() {
                    send(Pointer::Release, x + dx, y + dy, g.current_event_time());
                }
            }
        });
        self.widget.add_controller(drag);
        let click = gtk::GestureClick::new();
        click.connect_pressed(move |g, n, x, y| {
            if n == 2 {
                send(Pointer::DoubleClick, x, y, g.current_event_time());
            }
        });
        self.widget.add_controller(click);
    }

    fn wire(&self) {
        let inner = self.inner.clone();
        self.widget.connect_map({
            let inner = inner.clone();
            move |w| {
                make(w, &inner);
                kick(w, &inner);
            }
        });
        self.widget.connect_unmap({
            let inner = inner.clone();
            move |_| {
                let mut i = inner.borrow_mut();
                if let Some(id) = i.tick.take() {
                    id.remove();
                }
                i.live = None;
            }
        });
        self.widget.connect_scale_factor_notify({
            let inner = inner.clone();
            move |w| {
                resize(w, &inner);
                kick(w, &inner);
            }
        });
    }
}

/// Reduced motion is the desktop's animations setting.
fn reduced(mut style: Style) -> Style {
    let animate = gtk::Settings::default().is_some_and(|s| s.is_gtk_enable_animations());
    style.reduce_motion = !animate;
    style
}

/// Make the renderer for a cube that has just mapped.
fn make(w: &CubeArea, inner: &Rc<RefCell<Inner>>) {
    let Some(gpu) = gpu() else { return };
    let mut i = inner.borrow_mut();
    let scale = w.scale_factor().max(1);
    let (pw, ph) = ((i.size.0 * scale) as u32, (i.size.1 * scale) as u32);
    let Ok(mut view) = CubeView::offscreen(&gpu, pw, ph, f64::from(scale)) else {
        return;
    };
    view.set_visible(true);
    view.set_style(&gpu, reduced(i.style));
    i.live = Some(Live { view, gpu });
}

fn resize(w: &CubeArea, inner: &Rc<RefCell<Inner>>) {
    let mut i = inner.borrow_mut();
    let scale = w.scale_factor().max(1);
    let (pw, ph) = ((i.size.0 * scale) as u32, (i.size.1 * scale) as u32);
    if let Some(Live { view, gpu }) = i.live.as_mut() {
        view.resize(gpu, pw, ph, f64::from(scale));
    }
}

/// Draw on each frame the display clock gives, until the cube is at rest.
fn kick(w: &CubeArea, inner: &Rc<RefCell<Inner>>) {
    if inner.borrow().tick.is_some() || !w.is_mapped() {
        return;
    }
    let inner2 = inner.clone();
    let id = w.add_tick_callback(move |w, _| {
        let mut i = inner2.borrow_mut();
        let Some(Live { view, gpu }) = i.live.as_mut() else {
            i.tick = None;
            return glib::ControlFlow::Break;
        };
        let (drawn, moving) = view.draw(gpu).unwrap_or_else(|e| {
            eprintln!("origami-gtk: the cube could not draw: {e}");
            (origami_render::Drawn::Skipped, false)
        });
        if drawn == origami_render::Drawn::Presented {
            if let Some((pw, ph, pixels)) = view.take_frame() {
                let bytes = glib::Bytes::from_owned(pixels);
                let tex = gdk::MemoryTexture::new(
                    pw as i32,
                    ph as i32,
                    // Not B8g8r8x8: cairo's software path took 3 times the CPU.
                    gdk::MemoryFormat::B8g8r8a8Premultiplied,
                    &bytes,
                    pw as usize * 4,
                );
                w.show(tex.upcast_ref());
            }
        }
        if moving {
            glib::ControlFlow::Continue
        } else {
            i.tick = None;
            glib::ControlFlow::Break
        }
    });
    inner.borrow_mut().tick = Some(id);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test: GTK may be initialised on one thread only. Needs a display;
    /// the GPU parts are skipped without a hardware one.
    #[test]
    fn the_widget_is_its_logical_size_and_is_freed_with_its_window() {
        if gtk::init().is_err() {
            eprintln!("skipping: no display");
            return;
        }
        // A texture of twice the size (a scale-2 frame) must not double the
        // widget: its size is in points.
        let area = CubeArea::new(96, 64);
        let bytes = glib::Bytes::from_owned(vec![0u8; 192 * 128 * 4]);
        area.show(
            gdk::MemoryTexture::new(192, 128, gdk::MemoryFormat::B8g8r8x8, &bytes, 192 * 4)
                .upcast_ref(),
        );
        let (min, nat, _, _) = area.measure(gtk::Orientation::Horizontal, -1);
        assert_eq!((min, nat), (96, 96));
        let (min, nat, _, _) = area.measure(gtk::Orientation::Vertical, -1);
        assert_eq!((min, nat), (64, 64));

        let Some(cube) = Cube::new(96, 96, starting_style(0x2a6574)) else {
            eprintln!("skipping the free check: no hardware GPU");
            return;
        };
        let window = gtk::Window::new();
        window.set_child(Some(&cube.widget));
        let weak = cube.widget.downgrade();
        drop(cube);
        window.present();
        let ctx = glib::MainContext::default();
        while ctx.iteration(false) {}
        window.destroy();
        drop(window);
        while ctx.iteration(false) {}
        assert!(weak.upgrade().is_none(), "the cube widget leaked");
    }
}
