//! The SGI cube drawn offscreen by the core's renderer: GTK 4 hosts no native
//! child surface, so each frame is copied out as a texture.

use crate::granite::{self, tone};
use adw::prelude::*;
use gtk::{gdk, glib};
use origami_render::{scenes, Backdrop, CubeView, Env, Gpu, Style};
use std::cell::RefCell;
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
/// turning at 10 rpm over the page's own tinted granite (no tile of its own).
pub fn starting_style(chassis: u32) -> Style {
    Style {
        env: Env::Studio,
        horizon: Some(tone(chassis)),
        granite: granite::tinted(chassis),
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

/// About's cube: `scene`, at rest, on its own granite tile (the dialog has
/// none); the tones follow `dark`.
pub fn about_style(dark: bool, scene: usize) -> Style {
    let (light, dark_tone) = if dark {
        (tone(0x282a30), tone(0x1f2126))
    } else {
        (tone(0xe2e4e8), tone(0xd6d8dd))
    };
    Style {
        env: Env::Scene(scene),
        granite: [light, dark_tone],
        backdrop: Backdrop::Granite,
        light_look: !dark,
        ..Style::default()
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
    size: i32,
}

/// A cube `size` points square.
pub struct Cube {
    pub widget: gtk::Image,
    inner: Rc<RefCell<Inner>>,
}

impl Cube {
    /// None when the machine has no GPU the renderer can use.
    pub fn new(size: i32, style: Style) -> Option<Cube> {
        if !usable() {
            return None;
        }
        // An image's size is its pixel size in points, whatever the texture's.
        let widget = gtk::Image::builder()
            .pixel_size(size)
            .halign(gtk::Align::Center)
            .focusable(false)
            .build();
        widget.set_accessible_role(gtk::AccessibleRole::Presentation);
        let inner = Rc::new(RefCell::new(Inner {
            live: None,
            style,
            tick: None,
            size,
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
fn make(w: &gtk::Image, inner: &Rc<RefCell<Inner>>) {
    let Some(gpu) = gpu() else { return };
    let mut i = inner.borrow_mut();
    let scale = w.scale_factor().max(1);
    let px = (i.size * scale) as u32;
    let Ok(mut view) = CubeView::offscreen(&gpu, px, px, f64::from(scale)) else {
        return;
    };
    view.set_visible(true);
    view.set_style(&gpu, reduced(i.style));
    i.live = Some(Live { view, gpu });
}

fn resize(w: &gtk::Image, inner: &Rc<RefCell<Inner>>) {
    let mut i = inner.borrow_mut();
    let scale = w.scale_factor().max(1);
    let px = (i.size * scale) as u32;
    if let Some(Live { view, gpu }) = i.live.as_mut() {
        view.resize(gpu, px, px, f64::from(scale));
    }
}

/// Draw on each frame the display clock gives, until the cube is at rest.
fn kick(w: &gtk::Image, inner: &Rc<RefCell<Inner>>) {
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
                w.set_paintable(Some(&tex));
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
        // widget: its pixel size is in points.
        let image = gtk::Image::builder().pixel_size(96).build();
        let bytes = glib::Bytes::from_owned(vec![0u8; 192 * 192 * 4]);
        image.set_paintable(Some(&gdk::MemoryTexture::new(
            192,
            192,
            gdk::MemoryFormat::B8g8r8x8,
            &bytes,
            192 * 4,
        )));
        let (min, nat, _, _) = image.measure(gtk::Orientation::Horizontal, -1);
        assert_eq!((min, nat), (96, 96));

        let Some(cube) = Cube::new(96, starting_style(0x2a6574)) else {
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
