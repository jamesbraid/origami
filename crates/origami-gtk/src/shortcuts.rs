//! The Keyboard shortcuts window, filled from the core's groups. libadwaita's
//! shortcuts dialog needs 1.8, newer than this app builds against, so this is
//! the GTK one.

use gtk::prelude::*;
use origami_core::facade::{shortcut_groups, Platform};

/// A core shortcut as GTK writes it. A chord of modifiers alone, such as the
/// release keys, ends in the modifier's own key.
fn accel(keys: &str) -> String {
    let a = crate::commands::gtk_accel(keys);
    match a.strip_suffix("Alt") {
        Some(rest) if !rest.is_empty() => format!("{rest}Alt_L"),
        _ => a,
    }
}

pub fn present(parent: &impl IsA<gtk::Window>) {
    let section = gtk::ShortcutsSection::builder()
        .section_name("shortcuts")
        .build();
    for group in shortcut_groups(Platform::Linux) {
        let g = gtk::ShortcutsGroup::builder().title(&group.title).build();
        for item in group.items {
            g.add_shortcut(
                &gtk::ShortcutsShortcut::builder()
                    .title(&item.label)
                    .accelerator(accel(&item.keys))
                    .build(),
            );
        }
        section.add_group(&g);
    }
    let window = gtk::ShortcutsWindow::builder()
        .transient_for(parent)
        .modal(true)
        .build();
    window.set_widget_name("shortcuts.window");
    window.add_section(&section);
    window.present();
}
