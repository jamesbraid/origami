//! A machine made by an older Origami opens as this dialog instead of a tab:
//! Upgrade runs the core's upgrade, and the machine opens once it is done.

use crate::app::{self, App};
use crate::library::Entry;
use crate::window;
use adw::prelude::*;
use gtk::{gio, glib};
use origami_core::{Kind, Report};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

thread_local! {
    /// Machines being upgraded now, so opening one again cannot start a second upgrade.
    static RUNNING: RefCell<HashSet<PathBuf>> = RefCell::default();
}

/// The library pill's words and colour.
pub const PILL: (&str, &str) = ("Needs upgrade", "pill-amber");
const EXPLANATION: &str = "This machine was made by an older Origami. Upgrade it once to use it. Flash, NVRAM, drives, network settings and the Ethernet address are kept.";

/// What a person is told: everything but the lines kept for the log.
fn shown(report: &Report) -> Vec<String> {
    report
        .lines
        .iter()
        .filter_map(|l| match l.kind {
            Kind::Result | Kind::Detail => None,
            Kind::GuestNote => Some(format!("{}; the guest sees this at its next boot", l.text)),
            Kind::Warning => Some(format!("Warning: {}", l.text)),
            Kind::Advice | Kind::Note => Some(l.text.clone()),
        })
        .collect()
}

/// The core's sentence, with the command for local firmware files when a download failed.
fn failure(error: &str, dir: &str) -> String {
    if error.to_lowercase().contains("download") {
        format!("{error} To use local firmware files, run: origami upgrade {dir} --prom FILE --io-prom FILE")
    } else {
        error.to_string()
    }
}

/// An alert whose text is a labelled child, so the automation can read it.
fn say(app: &App, heading: &str, text: &str, id: &str) {
    let Some(w) = window::active(app) else { return };
    let d = adw::AlertDialog::new(Some(heading), None);
    d.set_extra_child(Some(
        &gtk::Label::builder()
            .label(text)
            .wrap(true)
            .selectable(true)
            .name(id)
            .build(),
    ));
    d.add_response("ok", "OK");
    d.present(Some(&w.win));
}

pub fn present(app: &Rc<App>, entry: &Entry) {
    let Some(w) = window::active(app) else { return };
    let d = adw::AlertDialog::new(
        Some(&format!("{} is made by an older Origami", entry.name)),
        Some(EXPLANATION),
    );
    d.add_responses(&[
        ("close", "Close"),
        ("show", "Show in Files"),
        ("upgrade", "Upgrade"),
    ]);
    d.set_response_appearance("upgrade", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("upgrade"));
    d.set_close_response("close");
    d.set_response_enabled("upgrade", !RUNNING.with_borrow(|r| r.contains(&entry.dir)));
    d.set_widget_name("upgrade.dialog");
    let (app, entry, win) = (app.clone(), entry.clone(), w.win.clone());
    d.connect_response(None, move |_, id| match id {
        "show" => window::show_in_files(&win, entry.dir.join("machine.toml")),
        "upgrade" => {
            let (app, entry) = (app.clone(), entry.clone());
            RUNNING.with_borrow_mut(|r| r.insert(entry.dir.clone()));
            let (core, dir) = (app.core.clone(), entry.dir.to_string_lossy().into_owned());
            glib::spawn_future_local(async move {
                let key = dir.clone();
                let done = gio::spawn_blocking(move || {
                    core.upgrade_machine(&key).map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|_| Err("the upgrade panicked".into()));
                RUNNING.with_borrow_mut(|r| r.remove(&entry.dir));
                match done {
                    Ok(report) => {
                        app::rescan(&app);
                        let fresh = app
                            .library
                            .borrow()
                            .iter()
                            .find(|e| e.dir == entry.dir)
                            .cloned();
                        let _ = fresh.and_then(|e| app::open(&app, &e));
                        let notes = shown(&report).join("\n");
                        if !notes.is_empty() {
                            say(
                                &app,
                                &format!("{} upgraded", entry.name),
                                &notes,
                                "upgrade.notes",
                            );
                        }
                    }
                    Err(e) => say(
                        &app,
                        &format!("Cannot upgrade {}", entry.name),
                        &failure(&e, &dir),
                        "upgrade.error",
                    ),
                }
            });
        }
        _ => {}
    });
    d.present(Some(&w.win));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_is_told_the_notes_and_advice_not_the_log_lines() {
        let mut report = Report::default();
        report.result("upgraded x");
        report.push(Kind::GuestNote, "topology renamed");
        report.warning("careful");
        report.push(Kind::Advice, "set the host id");
        assert_eq!(
            shown(&report),
            [
                "topology renamed; the guest sees this at its next boot",
                "Warning: careful",
                "set the host id"
            ]
        );
        assert!(failure("the PROM download failed.", "/m")
            .ends_with("origami upgrade /m --prom FILE --io-prom FILE"));
        assert_eq!(
            failure("stop the machine first", "/m"),
            "stop the machine first"
        );
    }
}
