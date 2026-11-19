//! The CPU panel under a running machine's picture: the core's activity view painted as
//! gr_osview-style strip bars. Polled at 2 Hz while shown, not at all while hidden.

use gtk::glib;
use gtk::prelude::*;
use origami_core::facade::{CpuLoadReport, CpuLoadSource, Osview};
use std::cell::RefCell;
use std::rc::Rc;

pub const TITLE: &str = origami_core::facade::OSVIEW_CAPTION;

const PAD: f64 = 8.0;
const HEADER: f64 = 18.0;
const BAR: f64 = 20.0;
const FRAME: f64 = 2.0;
const RULER: f64 = 5.0;
const GAP: f64 = 6.0;
const CAPTION: f64 = 16.0;

pub struct Panel {
    pub root: gtk::DrawingArea,
    scene: Rc<RefCell<Osview>>,
}

fn rgb(cr: &gtk::cairo::Context, c: u32) {
    let f = |shift: u32| f64::from((c >> shift) & 0xFF) / 255.0;
    cr.set_source_rgb(f(16), f(8), f(0));
}

fn height(v: &Osview) -> i32 {
    let bars = v.bars.len().max(1) as f64;
    (PAD * 2.0 + bars * (HEADER + BAR + RULER + GAP) + CAPTION) as i32
}

/// Paint `v` into a `w` wide area: per bar a title and legend, framed segments and a ruler.
fn paint(v: &Osview, cr: &gtk::cairo::Context, w: f64, scale: f64) {
    rgb(cr, v.background);
    let _ = cr.paint();
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_font_description(Some(&gtk::pango::FontDescription::from_string(
        "Sans Bold Italic 9",
    )));
    let text = |cr: &gtk::cairo::Context, s: &str, x: f64, y: f64| -> f64 {
        layout.set_text(s);
        cr.move_to(x, y);
        pangocairo::functions::show_layout(cr, &layout);
        f64::from(layout.pixel_size().0)
    };
    let mut y = PAD;
    for b in &v.bars {
        cr.set_source_rgb(0.0, 0.0, 0.0);
        let mut x = PAD + text(cr, &format!("{}:", b.title), PAD, y) + 6.0;
        for k in &b.legend {
            rgb(cr, k.rgb);
            x += text(cr, &k.label, x, y) + 6.0;
        }
        y += HEADER;
        let (bx, bw) = (PAD, w - PAD * 2.0);
        rgb(cr, v.bar_background);
        cr.rectangle(bx - FRAME, y - FRAME, bw + FRAME * 2.0, BAR + FRAME + RULER);
        let _ = cr.fill();
        let mut at = bx;
        for s in &b.segments {
            let sw = (bw * f64::from(s.fraction)).max(0.0);
            rgb(cr, s.rgb);
            cr.rectangle(at, y, sw, BAR);
            let _ = cr.fill();
            at += sw;
        }
        cr.set_source_rgb(0.0, 0.0, 0.0);
        for t in 1..v.ticks {
            // On whole device pixels, so every mark is the same width.
            let tx = ((bx + bw * f64::from(t) / f64::from(v.ticks)) * scale).round() / scale;
            cr.rectangle(tx, y + BAR - 5.0, 1.0, 5.0 + RULER);
            let _ = cr.fill();
        }
        y += BAR + RULER + GAP;
    }
    if let Some(note) = &v.note {
        cr.set_source_rgb(0.0, 0.0, 0.0);
        text(cr, note, PAD, PAD);
        y = PAD + HEADER;
    }
    layout.set_font_description(Some(&gtk::pango::FontDescription::from_string("Sans 8")));
    cr.set_source_rgb(0.2, 0.2, 0.2);
    text(cr, &v.caption, PAD, y);
}

impl Panel {
    pub fn new() -> Panel {
        let empty = CpuLoadReport {
            source: CpuLoadSource::Host,
            cpus: vec![],
            unavailable: None,
        };
        let scene = Rc::new(RefCell::new(Osview::from_cpu_load(&empty, None)));
        let root = gtk::DrawingArea::builder()
            .hexpand(true)
            .can_target(false)
            .visible(false)
            .content_height(height(&scene.borrow()))
            .build();
        let drawn = scene.clone();
        root.set_draw_func(move |a, cr, w, _| {
            paint(
                &drawn.borrow(),
                cr,
                f64::from(w),
                f64::from(a.scale_factor()),
            )
        });
        Panel { root, scene }
    }

    /// What the bars show now, as (cpu, user %, system %, idle %), for the scenarios.
    pub fn shown(&self) -> Vec<(u32, f32, f32, f32)> {
        let f = |i: usize, b: &origami_core::facade::OsviewBar| {
            b.segments.get(i).map_or(0.0, |s| s.fraction * 100.0)
        };
        self.scene
            .borrow()
            .bars
            .iter()
            .enumerate()
            .map(|(n, b)| (n as u32, f(0, b), f(1, b), f(2, b)))
            .collect()
    }

    /// The legend of the first bar, for the scenarios.
    pub fn legend(&self) -> Vec<String> {
        self.scene
            .borrow()
            .bars
            .first()
            .map(|b| b.legend.iter().map(|k| k.label.clone()).collect())
            .unwrap_or_default()
    }

    pub fn note(&self) -> String {
        self.scene.borrow().note.clone().unwrap_or_default()
    }

    pub fn show(&self, next: Osview) {
        self.root.set_content_height(height(&next));
        *self.scene.borrow_mut() = next;
        self.root.queue_draw();
    }
}

/// Poll `m`'s load every half second while `on` says so, and show it.
pub fn poll(on: impl Fn() -> bool + 'static, m: std::rc::Rc<crate::machine::MachineUi>) {
    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
        if !on() {
            return glib::ControlFlow::Break;
        }
        m.cpu.show(m.machine.osview());
        glib::ControlFlow::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use origami_core::facade::CpuLoad;

    fn pixel(s: &mut gtk::cairo::ImageSurface, x: i32, y: i32) -> u32 {
        s.flush();
        let stride = s.stride() as usize;
        let d = s.data().unwrap();
        let o = y as usize * stride + x as usize * 4;
        u32::from_le_bytes([d[o], d[o + 1], d[o + 2], 0]) & 0xFF_FFFF
    }

    #[test]
    fn a_bar_is_painted_in_its_segment_colours() {
        let report = CpuLoadReport {
            source: CpuLoadSource::Host,
            cpus: vec![CpuLoad {
                cpu: 0,
                user_pct: 50.0,
                sys_pct: 0.0,
            }],
            unavailable: None,
        };
        let v = Osview::from_cpu_load(&report, None);
        let (w, h) = (400, height(&v));
        let mut s = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h).unwrap();
        paint(
            &v,
            &gtk::cairo::Context::new(&s).unwrap(),
            f64::from(w),
            1.0,
        );
        let y = (PAD + HEADER + 4.0) as i32;
        // Cairo's ARGB32 reads back as 0xRRGGBB in a little-endian word.
        assert_eq!(pixel(&mut s, 100, y), 0x0000FF, "user, blue");
        assert_eq!(pixel(&mut s, 300, y), 0x00FF00, "idle, green");
        assert_eq!(pixel(&mut s, 1, 1), 0xBFBFBF, "the panel");
    }
}
