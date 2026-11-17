//! The CPU panel under a running machine's picture: one bar per emulated CPU,
//! user time in blue then system time in red, with the total at the right. The
//! numbers are the host's CPU time for the CPU threads, polled at 2 Hz while
//! the panel is shown and not at all while it is hidden.

use adw::prelude::*;
use gtk::glib;
use origami_core::facade::{CpuLoad, CpuLoadReport};
use std::cell::RefCell;

pub const TITLE: &str = "Emulated CPU load (measured on the host)";

pub struct Panel {
    pub root: gtk::Box,
    rows: gtk::Box,
    note: gtk::Label,
    shown: RefCell<Vec<(u32, f32, f32)>>,
}

impl Panel {
    pub fn new() -> Panel {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .css_classes(["cpu-panel"])
            .can_target(false)
            .visible(false)
            .build();
        root.append(
            &gtk::Label::builder()
                .label(TITLE)
                .xalign(0.0)
                .css_classes(["caption-heading", "on-dark"])
                .build(),
        );
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 3);
        root.append(&rows);
        let note = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(34)
            .css_classes(["caption", "on-dark"])
            .build();
        root.append(&note);
        Panel {
            root,
            rows,
            note,
            shown: RefCell::new(Vec::new()),
        }
    }

    /// What the rows show now, as (cpu, user %, system %), for the scenarios.
    pub fn shown(&self) -> Vec<(u32, f32, f32)> {
        self.shown.borrow().clone()
    }

    pub fn note(&self) -> String {
        self.note.label().to_string()
    }

    pub fn show(&self, report: &CpuLoadReport) {
        while let Some(c) = self.rows.first_child() {
            self.rows.remove(&c);
        }
        self.note.set_visible(report.cpus.is_empty());
        self.note.set_label(&empty_text(report));
        for c in &report.cpus {
            self.rows.append(&row(c));
        }
        *self.shown.borrow_mut() = report
            .cpus
            .iter()
            .map(|c| (c.cpu, c.user_pct, c.sys_pct))
            .collect();
    }
}

/// What to say when there is nothing to draw.
fn empty_text(report: &CpuLoadReport) -> String {
    match (&report.cpus.is_empty(), &report.unavailable) {
        (true, Some(why)) => why.clone(),
        (true, None) => "measuring…".into(),
        _ => String::new(),
    }
}

fn row(c: &CpuLoad) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    b.append(
        &gtk::Label::builder()
            .label(format!("cpu {}", c.cpu))
            .xalign(0.0)
            .width_chars(6)
            .css_classes(["caption", "monospace", "on-dark"])
            .build(),
    );
    let (user, sys) = (f64::from(c.user_pct), f64::from(c.sys_pct));
    let bar = gtk::DrawingArea::builder()
        .content_width(140)
        .content_height(10)
        .valign(gtk::Align::Center)
        .build();
    bar.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        cr.set_source_rgb(0.12, 0.12, 0.14);
        cr.rectangle(0.0, 0.0, w, h);
        let _ = cr.fill();
        let u = w * (user / 100.0).clamp(0.0, 1.0);
        let s = (w * (sys / 100.0).clamp(0.0, 1.0)).min(w - u);
        cr.set_source_rgb(0.30, 0.52, 0.95);
        cr.rectangle(0.0, 0.0, u, h);
        let _ = cr.fill();
        cr.set_source_rgb(0.90, 0.25, 0.25);
        cr.rectangle(u, 0.0, s, h);
        let _ = cr.fill();
    });
    b.append(&bar);
    b.append(
        &gtk::Label::builder()
            .label(format!("{:.0}%", (c.user_pct + c.sys_pct).min(100.0)))
            .xalign(1.0)
            .width_chars(4)
            .css_classes(["caption", "monospace", "on-dark"])
            .build(),
    );
    b
}

/// Poll `m`'s load every half second while `on` says so, and show it.
pub fn poll(on: impl Fn() -> bool + 'static, m: std::rc::Rc<crate::machine::MachineUi>) {
    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
        if !on() {
            return glib::ControlFlow::Break;
        }
        m.cpu.show(&m.machine.cpu_load());
        glib::ControlFlow::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use origami_core::facade::CpuLoadSource;

    fn report(cpus: Vec<CpuLoad>, why: Option<&str>) -> CpuLoadReport {
        CpuLoadReport {
            source: CpuLoadSource::Host,
            cpus,
            unavailable: why.map(String::from),
        }
    }

    #[test]
    fn an_empty_report_says_why_or_that_it_is_measuring() {
        assert_eq!(empty_text(&report(vec![], None)), "measuring…");
        assert_eq!(
            empty_text(&report(vec![], Some("no sampler"))),
            "no sampler"
        );
        let one = CpuLoad {
            cpu: 0,
            user_pct: 30.0,
            sys_pct: 10.0,
        };
        assert_eq!(empty_text(&report(vec![one], None)), "");
    }
}
