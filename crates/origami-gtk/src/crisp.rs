//! A texture painted with a chosen scaling filter. `GtkPicture` always
//! smooths, which blurs whole-number magnification of the guest's pixels.
//! Fractional steps above 1 stay linear, not sharp bilinear as on macOS and
//! Windows: that needs an in-frame path GTK does not have yet.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use std::cell::{Cell, RefCell};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Crisp {
        pub texture: RefCell<Option<gdk::Texture>>,
        pub place: Cell<super::Place>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Crisp {
        const NAME: &'static str = "OrigamiCrisp";
        type Type = super::Crisp;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Crisp {}

    impl PaintableImpl for Crisp {
        fn intrinsic_width(&self) -> i32 {
            self.texture.borrow().as_ref().map_or(0, |t| t.width())
        }

        fn intrinsic_height(&self) -> i32 {
            self.texture.borrow().as_ref().map_or(0, |t| t.height())
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let (Some(t), Some(s)) = (
                self.texture.borrow().clone(),
                snapshot.downcast_ref::<gtk::Snapshot>(),
            ) else {
                return;
            };
            let p = self.place.get();
            let filter = if p.nearest {
                gsk::ScalingFilter::Nearest
            } else {
                gsk::ScalingFilter::Linear
            };
            // Drawn in device pixels: a node whose own scale is 1 is smoothed
            // by the display's scale whatever filter it names.
            let f = p.factor as f32;
            let (w, h) = if p.size.0 > 0.0 {
                (p.size.0 as f32, p.size.1 as f32)
            } else {
                (width as f32 * f, height as f32 * f)
            };
            s.save();
            s.scale(1.0 / f, 1.0 / f);
            s.append_scaled_texture(
                &t,
                filter,
                &graphene::Rect::new(p.off.0 as f32, p.off.1 as f32, w, h),
            );
            s.restore();
        }
    }
}

glib::wrapper! {
    pub struct Crisp(ObjectSubclass<imp::Crisp>) @implements gdk::Paintable;
}

/// How the texture is drawn: the filter, the display's scale and the exact
/// device-pixel rectangle inside the widget (zero size means all of it).
#[derive(Clone, Copy, PartialEq)]
pub struct Place {
    pub nearest: bool,
    pub factor: f64,
    pub off: (f64, f64),
    pub size: (f64, f64),
}

impl Default for Place {
    fn default() -> Place {
        Place {
            nearest: true,
            factor: 1.0,
            off: (0.0, 0.0),
            size: (0.0, 0.0),
        }
    }
}

impl Crisp {
    pub fn new(texture: &gdk::Texture, place: Place) -> Crisp {
        let c: Crisp = glib::Object::new();
        *c.imp().texture.borrow_mut() = Some(texture.clone());
        c.imp().place.set(place);
        c
    }

    pub fn set_place(&self, place: Place) {
        if self.imp().place.replace(place) != place {
            self.invalidate_contents();
        }
    }

    pub fn texture(&self) -> Option<gdk::Texture> {
        self.imp().texture.borrow().clone()
    }
}
