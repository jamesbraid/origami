//! The Diagnostics window: one machine's state as the core and the display
//! code report it, as a monospaced report with a Copy Report button. It
//! refreshes every second while open. Anything not available is not shown.

use crate::app::App;
use crate::machine::MachineUi;
use adw::prelude::*;
use gtk::glib;
use std::rc::Rc;
use std::time::Duration;

#[derive(Clone)]
pub struct Window {
    pub win: adw::Window,
    pub report: gtk::Label,
    pub copy: gtk::Button,
}

fn refresh(m: &MachineUi, w: &Window, name: &gtk::Label, pill: &gtk::Label) {
    let (word, class) = m.state.get().pill();
    let v = m.machine.view();
    let (wakes, presents) = {
        let st = m.stats.borrow();
        (st.wakes, st.presents)
    };
    name.set_label(&v.name);
    for c in ["pill-grey", "pill-green", "pill-amber", "pill-red"] {
        pill.remove_css_class(c);
    }
    pill.add_css_class(class);
    pill.set_label(word);
    w.report
        .set_label(&m.machine.diagnostics_report(word, wakes, presents));
}

/// Open the machine's Diagnostics window, or raise it if it is open.
pub fn present(app: &Rc<App>, m: &Rc<MachineUi>) {
    if let Some(w) = m.diagnostics.borrow().as_ref() {
        w.win.present();
        return;
    }
    let report = gtk::Label::builder()
        .xalign(0.0)
        .yalign(0.0)
        .selectable(true)
        .focusable(false)
        .css_classes(["monospace"])
        .margin_top(8)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    let copy = gtk::Button::builder().label("Copy report").build();
    let name = gtk::Label::builder().css_classes(["heading"]).build();
    let pill = gtk::Label::builder()
        .css_classes(["pill", "pill-grey"])
        .valign(gtk::Align::Center)
        .build();
    let title = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    title.append(&name);
    title.append(&pill);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    header.pack_end(&copy);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&gtk::ScrolledWindow::builder().child(&report).build()));
    let win = adw::Window::builder()
        .title(format!("Diagnostics - {}", m.name))
        .default_width(720)
        .default_height(460)
        .content(&tv)
        .build();
    if let Some(p) = app.windows.borrow().first() {
        win.set_transient_for(Some(&p.win));
    }
    let w = Window { win, report, copy };
    {
        let (report, win) = (w.report.clone(), w.win.clone());
        w.copy.connect_clicked(move |_| {
            win.clipboard().set_text(&report.label());
        });
    }
    {
        let m = m.clone();
        w.win.connect_close_request(move |_| {
            m.diagnostics.borrow_mut().take();
            glib::Propagation::Proceed
        });
    }
    refresh(m, &w, &name, &pill);
    *m.diagnostics.borrow_mut() = Some(w.clone());
    w.win.present();
    let m = m.clone();
    glib::timeout_add_local(Duration::from_secs(1), move || {
        if m.diagnostics.borrow().is_none() {
            return glib::ControlFlow::Break;
        }
        refresh(&m, &w, &name, &pill);
        glib::ControlFlow::Continue
    });
}
