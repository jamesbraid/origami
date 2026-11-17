//! The SGI cube drawn offscreen by the core's renderer: GTK 4 hosts no native
//! child surface, so each frame is copied out as a texture.

use crate::granite::{self, tone};
use adw::prelude::*;
use gtk::{gdk, glib};
use origami_render::{scenes, CubeView, Env, Gpu, Style};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

/// The device is made once, on first use. A host with no GPU, or only a
/// software one, gets none: the cube would cost more CPU than it is worth.
fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| match Gpu::new() {
        Ok(g) if g.is_software() => {
            eprintln!("origami-gtk: no cube on a software GPU ({})", g.describe());
            None
        }
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("origami-gtk: no GPU for the cube: {e}");
            None
        }
    })
    .as_ref()
}

/// The start screen's cube: the studio with the machine's colour on its horizon,
/// over granite of the same hue at low chroma, turning at 10 rpm.
pub fn starting_style(chassis: u32) -> Style {
    Style {
        env: Env::Studio,
        horizon: Some(tone(chassis)),
        granite: granite::tinted(chassis),
        spin: true,
        ..Style::default()
    }
}

/// About's cube: the scene for the appearance, at rest.
pub fn about_style(dark: bool) -> Style {
    let (light, dark_tone) = if dark {
        (tone(0x282a30), tone(0x1f2126))
    } else {
        (tone(0xe2e4e8), tone(0xd6d8dd))
    };
    Style {
        env: Env::Scene(scenes::about(dark)),
        granite: [light, dark_tone],
        light_look: !dark,
        ..Style::default()
    }
}

struct Inner {
    view: CubeView,
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
        let gpu = gpu()?;
        let scale = 2;
        let px = (size * scale) as u32;
        let mut view = CubeView::offscreen(gpu, px, px, f64::from(scale)).ok()?;
        view.set_visible(false);
        view.set_style(gpu, reduced(style));
        // An image's size is its pixel size in points, whatever the texture's.
        let widget = gtk::Image::builder()
            .pixel_size(size)
            .halign(gtk::Align::Center)
            .focusable(false)
            .build();
        widget.set_accessible_role(gtk::AccessibleRole::Presentation);
        let inner = Rc::new(RefCell::new(Inner {
            view,
            tick: None,
            size,
        }));
        let cube = Cube { widget, inner };
        cube.wire();
        Some(cube)
    }

    /// Restyle the cube; it draws once for the change.
    pub fn set_style(&self, style: Style) {
        if let Some(gpu) = gpu() {
            self.inner.borrow_mut().view.set_style(gpu, reduced(style));
            kick(&self.widget, &self.inner);
        }
    }

    fn wire(&self) {
        let inner = self.inner.clone();
        self.widget.connect_map({
            let inner = inner.clone();
            move |w| {
                resize(w, &inner);
                inner.borrow_mut().view.set_visible(true);
                kick(w, &inner);
            }
        });
        self.widget.connect_unmap({
            let inner = inner.clone();
            move |_| {
                let mut i = inner.borrow_mut();
                i.view.set_visible(false);
                if let Some(id) = i.tick.take() {
                    id.remove();
                }
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

fn resize(w: &gtk::Image, inner: &Rc<RefCell<Inner>>) {
    let Some(gpu) = gpu() else { return };
    let mut i = inner.borrow_mut();
    let scale = w.scale_factor().max(1);
    let px = (i.size * scale) as u32;
    i.view.resize(gpu, px, px, f64::from(scale));
}

/// Draw on each frame the display clock gives, until the cube is at rest.
fn kick(w: &gtk::Image, inner: &Rc<RefCell<Inner>>) {
    if inner.borrow().tick.is_some() || !w.is_mapped() {
        return;
    }
    let inner2 = inner.clone();
    let id = w.add_tick_callback(move |w, _| {
        let Some(gpu) = gpu() else {
            return glib::ControlFlow::Break;
        };
        let mut i = inner2.borrow_mut();
        let (drawn, moving) = i.view.draw(gpu).unwrap_or_else(|e| {
            eprintln!("origami-gtk: the cube could not draw: {e}");
            (origami_render::Drawn::Skipped, false)
        });
        if drawn == origami_render::Drawn::Presented {
            if let Some((pw, ph, pixels)) = i.view.take_frame() {
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
