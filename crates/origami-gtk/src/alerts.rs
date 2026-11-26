//! The core's alerts as `AdwAlertDialog`s: Cancel first, the safe answer as the
//! default and the destructive one marked.

use adw::prelude::*;
use origami_core::facade::{alert, AlertKind, ButtonRole, Platform};
use std::cell::Cell;

thread_local! {
    /// The self-test skips confirmations unless a scenario asks to see them.
    static SHOWN: Cell<bool> = const { Cell::new(false) };
}

pub fn show_in_automation(on: bool) {
    SHOWN.set(on);
}

/// Whether to skip the question: the automation socket is on and no scenario is looking at alerts.
pub fn skipped(app: &crate::app::App) -> bool {
    app.cfg.automation.is_some() && !SHOWN.get()
}

/// The dialog for `kind`, with each response's id its button's index in the core's list.
pub fn dialog(kind: &AlertKind) -> (adw::AlertDialog, Vec<ButtonRole>) {
    let a = alert(kind, Platform::Linux);
    let d = adw::AlertDialog::new(Some(&a.title), Some(&a.body));
    d.set_widget_name("alert.dialog");
    let mut order: Vec<usize> = (0..a.buttons.len()).collect();
    order.sort_by_key(|&i| a.buttons[i].role != ButtonRole::Cancel);
    for i in order {
        let b = &a.buttons[i];
        d.add_response(&i.to_string(), &b.label);
        if b.role == ButtonRole::Destructive {
            d.set_response_appearance(&i.to_string(), adw::ResponseAppearance::Destructive);
        }
    }
    let at = |role| a.buttons.iter().position(|b| b.role == role);
    let safe = at(ButtonRole::Safe).or(at(ButtonRole::Cancel));
    d.set_default_response(safe.map(|i| i.to_string()).as_deref());
    d.set_close_response(&at(ButtonRole::Cancel).map_or(String::new(), |i| i.to_string()));
    (d, a.buttons.iter().map(|b| b.role).collect())
}

/// Ask `kind` over `parent`; Escape and a closed dialog answer Cancel.
pub async fn ask(parent: &impl IsA<gtk::Widget>, kind: &AlertKind) -> ButtonRole {
    let (d, roles) = dialog(kind);
    let id = d.choose_future(Some(parent)).await;
    id.parse::<usize>()
        .ok()
        .and_then(|i| roles.get(i).copied())
        .unwrap_or(ButtonRole::Cancel)
}
