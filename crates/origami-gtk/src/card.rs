//! The page a machine that cannot open shows in the content area: made by an
//! older Origami (Upgrade) or unreadable (Show in Files). The words are the core's.

use crate::app::{self, App};
use crate::library::Entry;
use crate::window::{self, Win};
use adw::prelude::*;
use gtk::{gio, glib};
use origami_core::facade::{
    machine_card, ActionKind, CardAction, CoreError, ErrorKind, Platform, ShownState, ToastKind,
    UiString,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

thread_local! {
    /// Machines being upgraded now, so a second click cannot start a second upgrade.
    static RUNNING: RefCell<HashSet<PathBuf>> = RefCell::default();
}

/// Show `entry`'s card in `w`. `error` is why an upgrade or an open failed.
pub fn show(app: &Rc<App>, w: &Rc<Win>, entry: &Entry, error: Option<CoreError>) {
    let state = entry.shown();
    let state = if matches!(state, ShownState::NeedsUpgrade | ShownState::CantOpen) {
        state
    } else {
        ShownState::CantOpen
    };
    let shown_error = error.clone().or_else(|| {
        entry
            .error
            .as_ref()
            .map(|e| CoreError::new(ErrorKind::Invalid, e.clone()))
    });
    let base = machine_card(
        state,
        if state == ShownState::CantOpen {
            shown_error.as_ref()
        } else {
            None
        },
        &entry.name,
        Platform::Linux,
    );
    let Some(card) = base else { return };
    let page = adw::StatusPage::builder()
        .title(&card.title)
        .description(&card.body)
        .build();
    page.set_widget_name("card.page");
    page.add_css_class("machine-art");
    match entry.art.and_then(crate::art::hero_paintable) {
        Some(p) => page.set_paintable(Some(&p)),
        None => page.set_icon_name(Some("computer-symbolic")),
    }
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .halign(gtk::Align::Center)
        .build();
    let busy = RUNNING.with_borrow(|r| r.contains(&entry.dir));
    let mut actions = card.actions.clone();
    let mut details = card.details.clone();
    // A failed upgrade: the core's sentence, its fix, then Try again.
    if let (ShownState::NeedsUpgrade, Some(e)) = (state, &error) {
        page.set_description(Some(&e.message));
        details = e.detail.clone();
        let failed = machine_card(ShownState::Stopped, Some(e), &entry.name, Platform::Linux);
        let again: Vec<CardAction> = failed.map(|c| c.actions).unwrap_or_default();
        actions = again
            .into_iter()
            .map(|a| match a.kind {
                ActionKind::TryAgain => CardAction {
                    kind: ActionKind::Upgrade,
                    ..a
                },
                _ => a,
            })
            .chain(
                card.actions
                    .into_iter()
                    .filter(|a| a.kind == ActionKind::ShowInFolder),
            )
            .collect();
    }
    let buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    for (i, a) in actions.iter().enumerate() {
        let b = gtk::Button::builder()
            .label(&a.label)
            .sensitive(!busy)
            .build();
        b.set_widget_name(&format!("card.{}", action_name(a.kind)));
        if i == 0 {
            b.add_css_class("suggested-action");
            b.add_css_class("pill");
        }
        let (app, w, entry, kind) = (app.clone(), Rc::downgrade(w), entry.clone(), a.kind);
        b.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                act(&app, &w, &entry, kind);
            }
        });
        buttons.append(&b);
    }
    body.append(&buttons);
    if busy {
        body.append(&gtk::Spinner::builder().spinning(true).build());
    }
    if let Some(text) = details {
        body.append(&details_row(&text));
    }
    page.set_child(Some(&body));
    if let Some(old) = w.stack.child_by_name("card") {
        w.stack.remove(&old);
    }
    w.stack.add_named(&page, Some("card"));
    w.stack.set_visible_child_name("card");
    *w.card_for.borrow_mut() = Some(entry.dir.clone());
    // The highlighted row is the machine whose card this is.
    let row = w
        .rows
        .borrow()
        .iter()
        .find(|r| r.dir == entry.dir)
        .map(|r| r.row.clone());
    if let Some(row) = row.filter(|r| w.list.selected_row().as_ref() != Some(r)) {
        w.list.select_row(Some(&row));
    }
    window::refresh(app);
}

fn action_name(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::Upgrade => "upgrade",
        ActionKind::ShowInFolder => "show",
        ActionKind::ChooseFirmware => "firmware",
        _ => "other",
    }
}

fn act(app: &Rc<App>, w: &Rc<Win>, entry: &Entry, kind: ActionKind) {
    match kind {
        ActionKind::ShowInFolder => window::show_in_files(&w.win, entry.dir.join("machine.toml")),
        ActionKind::Upgrade => upgrade(app, w, entry, None),
        ActionKind::ChooseFirmware => {
            let (app, w, entry) = (app.clone(), w.clone(), entry.clone());
            let win = w.win.clone();
            crate::disks::pick_path(&win.clone(), "Choose the boot PROM", move |prom| {
                let (app, w, entry) = (app.clone(), w.clone(), entry.clone());
                crate::disks::pick_path(&win, "Choose the IO PROM", move |io| {
                    upgrade(&app, &w, &entry, Some((prom.clone(), io)));
                });
            });
        }
        _ => {}
    }
}

/// The error's detail behind Show details, in monospace, with Copy.
pub fn details_row(text: &str) -> adw::PreferencesGroup {
    let label = gtk::Label::builder()
        .label(text)
        .selectable(true)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["monospace"])
        .build();
    let copy = gtk::Button::builder()
        .label(UiString::Copy.text())
        .halign(gtk::Align::End)
        .build();
    let text = text.to_string();
    copy.connect_clicked(move |b| b.clipboard().set_text(&text));
    let inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    inner.append(&label);
    inner.append(&copy);
    let row = adw::ExpanderRow::builder()
        .title(UiString::ShowDetails.text())
        .build();
    row.set_widget_name("card.details");
    row.add_row(&inner);
    let group = adw::PreferencesGroup::new();
    group.add(&row);
    group
}

/// Upgrade `entry` (with chosen firmware files, when the download failed), then open it.
pub fn upgrade(app: &Rc<App>, w: &Rc<Win>, entry: &Entry, firmware: Option<(PathBuf, PathBuf)>) {
    if RUNNING.with_borrow_mut(|r| !r.insert(entry.dir.clone())) {
        return;
    }
    show(app, w, entry, None);
    let (app, w, entry) = (app.clone(), w.clone(), entry.clone());
    glib::spawn_future_local(async move {
        let (core, key) = (app.core.clone(), entry.dir.to_string_lossy().into_owned());
        let done = gio::spawn_blocking(move || match &firmware {
            Some((prom, io)) => core.upgrade_machine_with(&key, Some(prom), Some(io)),
            None => core.upgrade_machine(&key),
        })
        .await
        .unwrap_or_else(|_| {
            Err(CoreError::new(
                ErrorKind::Internal,
                "The upgrade stopped unexpectedly.",
            ))
        });
        RUNNING.with_borrow_mut(|r| r.remove(&entry.dir));
        match done {
            Ok(_) => {
                app::rescan(&app);
                window::clear_card(&w);
                let fresh = app
                    .library
                    .borrow()
                    .iter()
                    .find(|e| e.dir == entry.dir)
                    .cloned();
                let _ = fresh.and_then(|e| app::open(&app, &e));
                window::toast_for(
                    &app,
                    &ToastKind::Upgraded {
                        name: entry.name.clone(),
                    },
                    || {},
                );
            }
            Err(e) => {
                let stale = w.card_for.borrow().as_ref() == Some(&entry.dir);
                if stale {
                    show(&app, &w, &entry, Some(e));
                }
            }
        }
    });
}
