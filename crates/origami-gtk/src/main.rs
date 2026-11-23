//! Origami for Linux: a GTK 4 and libadwaita front end.
//!
//! The machine's picture goes from the engine's imported memory through a GPU
//! copy of the changed lines into a ring of dma-buf images, shown as a
//! `GdkDmabufTexture` inside a `GtkGraphicsOffload`. Without a usable Vulkan
//! device, or with `--no-import`, the engine copies the changed lines and the
//! picture is a memory texture.

mod app;
mod art;
mod automation;
mod commands;
mod config;
mod console;
mod cpu_panel;
mod credits;
mod crisp;
mod cube;
mod diagnostics;
mod disks;
mod display;
mod granite;
mod input;
mod install;
mod keys;
mod library;
mod machine;
mod pointer_lock;
mod prefs;
mod prefs_dialog;
mod settings;
mod style;
mod upgrade;
mod view_scale;
mod vk;
mod window;

use gtk::glib;

fn main() -> glib::ExitCode {
    let cfg = match config::Config::from_args(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("origami-gtk: {e}");
            return glib::ExitCode::from(2);
        }
    };
    app::run(cfg)
}
