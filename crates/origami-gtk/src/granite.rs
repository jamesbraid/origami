//! The IRIX granite as a surround: the shared two-colour tile, recoloured and
//! repeated with nearest-neighbour scaling, so a 2x display doubles its pixels.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use std::cell::RefCell;
use std::collections::HashMap;

const TILE: &[u8] = include_bytes!("../../../resources/identity/granite-tile.png");
/// The tile is 256 pixels square and is drawn 256 points square.
const SIZE: f32 = 256.0;

pub type Tones = [[f32; 3]; 2];

pub fn tone(hex: u32) -> [f32; 3] {
    [16, 8, 0].map(|s| ((hex >> s) & 0xff) as f32 / 255.0)
}

/// Neutral dark: the guest screen's letterbox.
pub fn neutral() -> Tones {
    [tone(0x282a30), tone(0x1f2126)]
}

/// The chassis hue at low chroma over neutral grey: the start screen.
pub fn tinted(chassis: u32) -> Tones {
    let c = tone(chassis);
    let grey = [0.13, 0.14, 0.16];
    let at = |k: f32| std::array::from_fn(|i| (0.22 * c[i] + 0.78 * grey[i]) * k);
    [at(1.0), at(0.8)]
}

/// The tile's mask: true where the bitmap has its first colour.
fn mask() -> Option<(Vec<bool>, i32, i32)> {
    let t = gdk::Texture::from_bytes(&glib::Bytes::from_static(TILE)).ok()?;
    let mut d = gdk::TextureDownloader::new(&t);
    d.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = d.download_bytes();
    let (w, h) = (t.width(), t.height());
    let m = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| bytes[y as usize * stride + x as usize * 4] == 56)
        .collect();
    Some((m, w, h))
}

fn tile(tones: Tones) -> Option<gdk::Texture> {
    thread_local! {
        static TILES: RefCell<HashMap<[u32; 6], gdk::Texture>> = RefCell::default();
    }
    let key: [u32; 6] = std::array::from_fn(|i| (tones[i / 3][i % 3] * 255.0).round() as u32);
    if let Some(t) = TILES.with(|c| c.borrow().get(&key).cloned()) {
        return Some(t);
    }
    let (mask, w, h) = mask()?;
    let rgba: Vec<u8> = mask
        .iter()
        .flat_map(|&first| {
            let c = if first { &key[..3] } else { &key[3..] };
            [c[0] as u8, c[1] as u8, c[2] as u8, 255]
        })
        .collect();
    let t = gdk::MemoryTexture::new(
        w,
        h,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(rgba),
        w as usize * 4,
    )
    .upcast::<gdk::Texture>();
    TILES.with(|c| c.borrow_mut().insert(key, t.clone()));
    Some(t)
}

mod imp {
    use super::*;
    #[derive(Default)]
    pub struct Granite {
        pub tile: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Granite {
        const NAME: &'static str = "OrigamiGranite";
        type Type = super::Granite;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for Granite {}
    impl BoxImpl for Granite {}
    impl WidgetImpl for Granite {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(tile) = self.tile.borrow().as_ref() {
                let obj = self.obj();
                let (w, h) = (obj.width() as f32, obj.height() as f32);
                snapshot.push_repeat(
                    &graphene::Rect::new(0.0, 0.0, w, h),
                    Some(&graphene::Rect::new(0.0, 0.0, SIZE, SIZE)),
                );
                snapshot.append_scaled_texture(
                    tile,
                    gsk::ScalingFilter::Nearest,
                    &graphene::Rect::new(0.0, 0.0, SIZE, SIZE),
                );
                snapshot.pop();
            }
            self.parent_snapshot(snapshot);
        }
    }
}

glib::wrapper! {
    /// A box that draws granite behind its children.
    pub struct Granite(ObjectSubclass<imp::Granite>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Orientable, gtk::Buildable, gtk::Accessible, gtk::ConstraintTarget;
}

impl Granite {
    pub fn new(tones: Tones) -> Granite {
        let g: Granite = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .build();
        *g.imp().tile.borrow_mut() = tile(tones);
        g
    }
}
