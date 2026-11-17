//! Machine art: the shared product stills, picked by topology or product, in
//! the light or dark treatment that matches the appearance.

use adw::prelude::*;
use gtk::{gdk, glib};
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::HashMap;

const MANIFEST: &str = include_str!("../../../resources/identity/stills/manifest.json");

#[derive(Deserialize)]
struct Manifest {
    topologies: HashMap<String, String>,
    products: HashMap<String, String>,
    stills: HashMap<String, Still>,
}

#[derive(Deserialize)]
struct Still {
    colour: String,
}

fn manifest() -> &'static Manifest {
    static M: std::sync::OnceLock<Manifest> = std::sync::OnceLock::new();
    M.get_or_init(|| serde_json::from_str(MANIFEST).expect("the stills manifest parses"))
}

/// The still for a machine: its topology's, else its product's.
pub fn of(product: &str, topology: Option<&str>) -> Option<&'static str> {
    let m = manifest();
    let id = topology
        .and_then(|t| m.topologies.get(t))
        .or_else(|| m.products.get(product))?;
    m.stills.get_key_value(id).map(|(k, _)| k.as_str())
}

/// The still's measured body colour, `0xRRGGBB`.
pub fn colour(id: &str) -> u32 {
    let hex = &manifest().stills[id].colour;
    u32::from_str_radix(hex.trim_start_matches('#'), 16).expect("a hex colour")
}

/// Every still's id and measured colour.
pub fn colours() -> impl Iterator<Item = (&'static str, u32)> {
    manifest().stills.keys().map(|id| (id.as_str(), colour(id)))
}

macro_rules! stills {
    ($($id:literal)*) => {
        #[cfg(test)]
        const IDS: &[&str] = &[$($id),*];
        fn bytes(id: &str, file: &str) -> Option<&'static [u8]> {
            macro_rules! f {
                ($id2:literal, $name:literal) => {
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"), "/../../resources/identity/stills/",
                        $id2, "/", $name, ".png"
                    )) as &[u8]
                };
            }
            Some(match (id, file) { $(
                ($id, "20-light@2x") => f!($id, "20-light@2x"),
                ($id, "20-dark@2x") => f!($id, "20-dark@2x"),
                ($id, "34-light@2x") => f!($id, "34-light@2x"),
                ($id, "34-dark@2x") => f!($id, "34-dark@2x"),
                ($id, "card@2x") => f!($id, "card@2x"),
                ($id, "hero@2x") => f!($id, "hero@2x"),
            )* _ => return None })
        }
    };
}

stills!(
    "fuel" "octane" "octane2" "o2" "indy" "indigo2-impact" "tezro-tower"
    "origin200-tower" "origin2000-deskside" "origin2000-rack" "onyx2-deskside"
    "onyx2-rack" "origin300" "origin350" "onyx350" "origin3000" "origin3200"
    "origin3900" "onyx3000"
);

type Listener = Box<dyn Fn(bool) -> bool>;

thread_local! {
    static CONNECTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TEXTURES: RefCell<HashMap<(String, String), gdk::Texture>> = RefCell::default();
    static LISTENERS: RefCell<Vec<Listener>> = const { RefCell::new(Vec::new()) };
}

fn texture(id: &str, file: &str) -> Option<gdk::Texture> {
    let key = (id.to_owned(), file.to_owned());
    if let Some(t) = TEXTURES.with(|c| c.borrow().get(&key).cloned()) {
        return Some(t);
    }
    let t = gdk::Texture::from_bytes(&glib::Bytes::from_static(bytes(id, file)?)).ok()?;
    TEXTURES.with(|c| c.borrow_mut().insert(key, t.clone()));
    Some(t)
}

/// The small-size treatment (`20` or `34` pt) for the appearance.
fn small(id: &str, pt: i32, dark: bool) -> Option<gdk::Texture> {
    texture(
        id,
        &format!("{pt}-{}@2x", if dark { "dark" } else { "light" }),
    )
}

/// Run `f(dark)` now and whenever the appearance changes, until it returns false.
pub fn follow_appearance(f: impl Fn(bool) -> bool + 'static) {
    let dark = adw::StyleManager::default().is_dark();
    if !f(dark) {
        return;
    }
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(f)));
    if !CONNECTED.replace(true) {
        adw::StyleManager::default().connect_dark_notify(|sm| {
            let dark = sm.is_dark();
            let listeners = LISTENERS.with(|l| std::mem::take(&mut *l.borrow_mut()));
            let kept: Vec<_> = listeners.into_iter().filter(|f| f(dark)).collect();
            LISTENERS.with(|l| {
                let mut l = l.borrow_mut();
                l.splice(0..0, kept);
            });
        });
    }
}

/// A still at `pt` points (20 or 34) that follows the appearance.
pub fn small_image(id: &'static str, pt: i32) -> gtk::Image {
    let image = gtk::Image::builder().pixel_size(pt).build();
    image.set_accessible_role(gtk::AccessibleRole::Presentation);
    let weak = image.downgrade();
    follow_appearance(move |dark| {
        let Some(image) = weak.upgrade() else {
            return false;
        };
        image.set_paintable(small(id, pt, dark).as_ref());
        true
    });
    image
}

/// Show a 20 pt still as a tab's icon, following the appearance.
pub fn tab_icon(page: &adw::TabPage, id: &'static str) {
    let weak = page.downgrade();
    follow_appearance(move |dark| {
        let Some(page) = weak.upgrade() else {
            return false;
        };
        page.set_icon(small(id, 20, dark).as_ref());
        true
    });
}

/// A texture that reports half its pixels as its size in points.
mod pt {
    use gtk::subclass::prelude::*;
    use gtk::{gdk, glib, prelude::*};
    use std::cell::RefCell;

    mod imp {
        use super::*;

        #[derive(Default)]
        pub struct Pt2x {
            pub texture: RefCell<Option<gdk::Texture>>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Pt2x {
            const NAME: &'static str = "OrigamiPt2x";
            type Type = super::Pt2x;
            type Interfaces = (gdk::Paintable,);
        }

        impl ObjectImpl for Pt2x {}
        impl PaintableImpl for Pt2x {
            fn flags(&self) -> gdk::PaintableFlags {
                gdk::PaintableFlags::STATIC_CONTENTS | gdk::PaintableFlags::STATIC_SIZE
            }
            fn intrinsic_width(&self) -> i32 {
                self.texture.borrow().as_ref().map_or(0, |t| t.width() / 2)
            }
            fn intrinsic_height(&self) -> i32 {
                self.texture.borrow().as_ref().map_or(0, |t| t.height() / 2)
            }
            fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
                if let Some(t) = self.texture.borrow().as_ref() {
                    t.snapshot(snapshot, width, height);
                }
            }
        }
    }

    glib::wrapper! {
        pub struct Pt2x(ObjectSubclass<imp::Pt2x>) @implements gdk::Paintable;
    }

    impl Pt2x {
        pub fn new(texture: &gdk::Texture) -> Pt2x {
            let p: Pt2x = glib::Object::new();
            *p.imp().texture.borrow_mut() = Some(texture.clone());
            p
        }
    }
}

/// The framed still, `card` (120 by 90 pt) or `hero` (240 by 180 pt), at that
/// size and no larger. A picture alone stretches to the room it is offered.
pub fn framed(id: &str, name: &str) -> Option<gtk::AspectFrame> {
    let t = texture(id, &format!("{name}@2x"))?;
    let (w, h) = (t.width() / 2, t.height() / 2);
    let picture = gtk::Picture::for_paintable(&pt::Pt2x::new(&t));
    picture.set_can_shrink(true);
    picture.set_accessible_role(gtk::AccessibleRole::Presentation);
    Some(
        gtk::AspectFrame::builder()
            .ratio(w as f32 / h as f32)
            .obey_child(false)
            .width_request(w)
            .height_request(h)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .child(&picture)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_stills_are_the_manifest_stills() {
        let mut ids: Vec<_> = manifest().stills.keys().map(String::as_str).collect();
        ids.sort_unstable();
        let mut have = IDS.to_vec();
        have.sort_unstable();
        assert_eq!(have, ids);
        for id in IDS {
            for file in ["20-light@2x", "34-dark@2x", "card@2x", "hero@2x"] {
                assert!(bytes(id, file).is_some(), "{id} {file}");
            }
        }
    }

    #[test]
    fn topology_beats_product() {
        assert_eq!(of("onyx2", Some("onyx2-rack")), Some("onyx2-rack"));
        assert_eq!(of("onyx2", None), Some("onyx2-deskside"));
        assert_eq!(of("octane2", Some("unknown")), Some("octane2"));
        assert_eq!(of("mystery", None), None);
        assert_eq!(colour("octane2"), 0x2e354e);
    }
}
