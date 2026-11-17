//! The Diagnostics window: one machine's state as the core and the display
//! code report it, as a monospaced report with a Copy Report button. It
//! refreshes every second while open. Anything not available is not shown.

use crate::app::App;
use crate::machine::MachineUi;
use adw::prelude::*;
use gtk::glib;
use origami_core::facade::MachineView;
use std::rc::Rc;
use std::time::Duration;

#[derive(Clone)]
pub struct Window {
    pub win: adw::Window,
    pub report: gtk::Label,
    pub copy: gtk::Button,
}

/// The report text. `state` is the tab's word for the machine; `wakes` and
/// `presents` are this app's counts of frame wake-ups and shown pictures.
pub fn report(v: &MachineView, state: &str, wakes: u64, presents: u64) -> String {
    // The tab's word and the core's state, once when they are the same.
    let core = format!("{:?}", v.state);
    let state = if state == core {
        core
    } else {
        format!("{state} ({core})")
    };
    let mut s = format!(
        "Machine:      {}\nEngine:       {}\nState:        {state}\nReason:       {}\nLast error:   {}\nHeads:\n",
        v.name,
        v.engine,
        v.reason,
        v.last_error.as_deref().unwrap_or("none"),
    );
    if v.heads.is_empty() {
        s.push_str("  none\n");
    }
    for h in &v.heads {
        s.push_str(&format!(
            "  #{}  {}x{}  picture {}  sequence {}\n",
            h.index,
            h.width,
            h.height,
            if h.active { "yes" } else { "no" },
            h.seq
        ));
    }
    let c = &v.console;
    let link = match (c.connected, c.down_reason.as_deref()) {
        (true, _) => "connected".to_string(),
        (false, Some(why)) => format!("not connected ({why})"),
        (false, None) => "not connected".to_string(),
    };
    s.push_str(&format!("Console:      {link}, {} bytes\n", c.total));
    s.push_str(&format!(
        "Screen:       {wakes} wake-ups, {presents} presents\n"
    ));
    s
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
    w.report.set_label(&report(&v, word, wakes, presents));
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
    let copy = gtk::Button::builder().label("Copy Report").build();
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

#[cfg(test)]
mod tests {
    use super::*;
    use origami_core::facade::{ConsoleView, HeadView, MachineState};

    fn view() -> MachineView {
        MachineView {
            key: "/lib/o2".into(),
            name: "o2".into(),
            product: "o2".into(),
            engine: "qemu".into(),
            state: MachineState::Running,
            exit_code: None,
            exit_reason: None,
            exit_detail: None,
            last_error: None,
            reason: "The machine is running.".into(),
            heads: vec![HeadView {
                index: 0,
                width: 1280,
                height: 1024,
                seq: 42,
                active: true,
            }],
            console: ConsoleView {
                connected: false,
                down_reason: Some("no serial port".into()),
                total: 7,
            },
            operation: None,
        }
    }

    #[test]
    fn the_report_names_every_field() {
        let r = report(&view(), "Running", 5, 4);
        for want in [
            "Machine:      o2",
            "Engine:       qemu",
            "State:        Running\n",
            "Last error:   none",
            "  #0  1280x1024  picture yes  sequence 42",
            "Console:      not connected (no serial port), 7 bytes",
            "Screen:       5 wake-ups, 4 presents",
        ] {
            assert!(r.contains(want), "{want:?} in {r}");
        }
    }

    #[test]
    fn a_differing_tab_word_is_shown_with_the_core_state() {
        let mut v = view();
        v.state = MachineState::NotRunning;
        assert!(report(&v, "Stopped", 0, 0).contains("State:        Stopped (NotRunning)\n"));
    }

    #[test]
    fn a_machine_with_no_heads_says_so() {
        let mut v = view();
        v.heads.clear();
        v.last_error = Some("QEMU exited".into());
        let r = report(&v, "Stopped", 0, 0);
        assert!(r.contains("Heads:\n  none\n") && r.contains("Last error:   QEMU exited"));
    }
}
