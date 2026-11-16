//! Application state and the glue between the core's threads and GTK's main
//! loop. Everything that touches a widget runs on the main thread; the core's
//! observer, the engine's frame hook and worker threads post to one channel.

use crate::config::Config;
use crate::library::{self, Entry};
use crate::machine::{MachineUi, State};
use crate::prefs::Prefs;
use crate::window::Win;
use crate::{automation, commands, display, input, style, vk, window};
use adw::prelude::*;
use gtk::{gio, glib};
use origami_core::facade::{Config as CoreConfig, Core, Event, EventKind, Observer, StopKind};
use origami_engine::Control;
use origami_qemu::RunnerEngine;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// What the core's and engine's threads tell the main thread.
pub enum Msg {
    /// A machine has a new picture.
    Wake(Arc<str>),
    Event(Event),
    Auto(automation::Request),
    /// A GPU copy into a scanout image finished (or failed).
    CopyDone(PathBuf, vk::Pending, Result<(), String>),
}

struct Forward(async_channel::Sender<Msg>);

impl Observer for Forward {
    fn on_event(&self, event: Event) {
        let _ = self.0.try_send(Msg::Event(event));
    }
}

pub struct App {
    pub cfg: Config,
    pub core: Core,
    pub engine: Arc<RunnerEngine>,
    pub dev: Option<Arc<vk::Dev>>,
    pub gtk_mods: RefCell<Vec<u64>>,
    pub prefs: RefCell<Prefs>,
    pub library: RefCell<Vec<Entry>>,
    pub commands: Vec<commands::Command>,
    pub machines: RefCell<Vec<Rc<MachineUi>>>,
    pub windows: RefCell<Vec<Rc<Win>>>,
    pub tx: async_channel::Sender<Msg>,
    pub app: adw::Application,
}

pub fn run(cfg: Config) -> glib::ExitCode {
    let dev = if cfg.no_import {
        None
    } else {
        match vk::Dev::open(cfg.vk_device.as_deref()) {
            // A software rasteriser (lavapipe) cannot export memory GTK can
            // read, so importing into it only adds a copy; use the engine's.
            Ok(d) if d.dev_type == ash::vk::PhysicalDeviceType::CPU || !d.dma_buf_export => {
                eprintln!(
                    "origami-gtk: vulkan: {} cannot export dma-bufs; copying changed lines instead",
                    d.describe()
                );
                None
            }
            Ok(d) => {
                eprintln!("origami-gtk: vulkan: {}", d.describe());
                Some(d)
            }
            Err(e) => {
                eprintln!("origami-gtk: no GPU import ({e}); copying");
                None
            }
        }
    };
    if cfg.probe {
        std::process::exit(if dev.is_some() { 0 } else { 3 });
    }
    let started = (|| -> Result<_, String> {
        let runner = RunnerEngine::locate_runner().map_err(|e| e.to_string())?;
        let runtime = origami_qemu::Runtime::locate().map_err(|e| e.to_string())?;
        Ok(Arc::new(RunnerEngine::new(runtime, runner)))
    })();
    let engine = match started {
        Ok(e) => e,
        Err(e) => {
            eprintln!("origami-gtk: {e}");
            return glib::ExitCode::from(1);
        }
    };
    if let Some(d) = &dev {
        engine.set_importer(Arc::new(vk::VkImporter { dev: d.clone() }));
    }
    let (tx, rx) = async_channel::unbounded::<Msg>();
    let core = match Core::new(
        CoreConfig::new(&cfg.library),
        vec![engine.clone()],
        Some(Arc::new(Forward(tx.clone()))),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("origami-gtk: {e}");
            return glib::ExitCode::from(1);
        }
    };
    let gapp = adw::Application::builder()
        .application_id("fans.irix.Origami")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let prefs = Prefs::load(&cfg.config_dir);
    let app = Rc::new(App {
        commands: commands::table(&core),
        core,
        engine,
        dev,
        gtk_mods: RefCell::new(Vec::new()),
        prefs: RefCell::new(prefs),
        library: RefCell::new(Vec::new()),
        machines: RefCell::new(Vec::new()),
        windows: RefCell::new(Vec::new()),
        tx,
        app: gapp.clone(),
        cfg,
    });
    gapp.connect_startup(|_| style::install());
    let rx = RefCell::new(Some(rx));
    gapp.connect_activate(move |_| {
        if let Some(w) = app.windows.borrow().first() {
            w.win.present();
            return;
        }
        let win = window::build(&app, true);
        win.win.present();
        *app.gtk_mods.borrow_mut() = display::dmabuf_modifiers(&win.win);
        eprintln!(
            "origami-gtk: GDK imports XRGB8888 with modifiers {:x?}",
            app.gtk_mods.borrow()
        );
        rescan(&app);
        {
            // The first catalogue call may start the engine's program; do it
            // off the UI thread so opening a machine finds it cached.
            let core = app.core.clone();
            std::thread::spawn(move || {
                let _ = core.catalogue();
            });
        }
        if let Some(rx) = rx.borrow_mut().take() {
            let app = app.clone();
            glib::spawn_future_local(async move {
                while let Ok(m) = rx.recv().await {
                    let mut batch = vec![m];
                    while let Ok(m) = rx.try_recv() {
                        batch.push(m);
                    }
                    for m in batch {
                        handle(&app, m);
                    }
                }
            });
        }
        // The one state that needs a clock: no picture after a wait.
        let tick = app.clone();
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let waiting: Vec<_> = tick
                .machines
                .borrow()
                .iter()
                .filter(|m| m.state.get() == State::Starting)
                .cloned()
                .collect();
            for m in waiting {
                m.apply_state();
            }
            if tick
                .machines
                .borrow()
                .iter()
                .any(|m| m.state.get() == State::Starting)
            {
                window::refresh(&tick);
            }
            glib::ControlFlow::Continue
        });
        if let Some(sock) = app.cfg.automation.clone() {
            automation::serve(&app, sock);
        }
        for name in app.cfg.start.clone() {
            match find_entry(&app, &name) {
                Some(e) => {
                    if let Some(m) = open(&app, &e) {
                        start(&app, &m);
                    }
                }
                None => eprintln!(
                    "origami-gtk: --start {name}: no such machine in {}",
                    app.cfg.library.display()
                ),
            }
        }
    });
    gapp.run_with_args::<&str>(&[])
}

pub fn rescan(app: &Rc<App>) {
    *app.library.borrow_mut() = library::entries(&app.core);
    window::rebuild_library(app);
}

pub fn find_entry(app: &App, name: &str) -> Option<Entry> {
    app.library
        .borrow()
        .iter()
        .find(|e| e.name == name || e.dir.as_path() == std::path::Path::new(name))
        .cloned()
}

pub fn by_key(app: &App, key: &str) -> Option<Rc<MachineUi>> {
    app.machines
        .borrow()
        .iter()
        .find(|m| m.machine.key() == key)
        .cloned()
}

pub fn by_name(app: &App, name: &str) -> Option<Rc<MachineUi>> {
    app.machines
        .borrow()
        .iter()
        .find(|m| m.name == name)
        .cloned()
}

/// The machine's tab: made and selected if it was not open.
pub fn open(app: &Rc<App>, entry: &Entry) -> Option<Rc<MachineUi>> {
    if let Some(m) = app
        .machines
        .borrow()
        .iter()
        .find(|m| m.dir == entry.dir)
        .cloned()
    {
        window::select(app, &m);
        return Some(m);
    }
    let machine = match app.core.open_machine(&entry.dir.to_string_lossy()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("origami-gtk: {}: {e}", entry.name);
            if let Some(w) = window::active(app) {
                let d = adw::AlertDialog::new(
                    Some(&format!("Cannot open {}", entry.name)),
                    Some(&e.to_string()),
                );
                d.add_response("ok", "OK");
                d.present(Some(&w.win));
            }
            return None;
        }
    };
    let dir = PathBuf::from(machine.key());
    let m = MachineUi::new(app, entry.clone(), machine.clone());
    input::wire(app, &m);
    {
        let (a, mm) = (app.clone(), m.clone());
        m.root
            .connect_map(move |_| a.engine.set_visible(&mm.dir, true));
        let (a, mm) = (app.clone(), m.clone());
        m.root
            .connect_unmap(move |_| a.engine.set_visible(&mm.dir, false));
    }
    let hook = {
        let (tx, key, pending) = (
            app.tx.clone(),
            Arc::<str>::from(machine.key()),
            m.wake_pending.clone(),
        );
        machine.add_frame_hook(Arc::new(move || {
            if !pending.swap(true, Ordering::AcqRel) {
                let _ = tx.try_send(Msg::Wake(key.clone()));
            }
        }))
    };
    m.hook.set(Some(hook));
    // A runner from an earlier session may still be up; Start attaches to it.
    if origami_qemu::control::running(&dir)
        .ok()
        .flatten()
        .is_some()
    {
        m.left_running.set(true);
    }
    app.machines.borrow_mut().push(m.clone());
    window::add_tab(app, &m);
    m.apply_state();
    Some(m)
}

pub fn start(app: &Rc<App>, m: &Rc<MachineUi>) {
    if m.state.get().is_up() {
        return;
    }
    m.left_running.set(false);
    m.stop_requested.set(false);
    *m.error.borrow_mut() = None;
    m.started_at.set(Some(std::time::Instant::now()));
    match m.machine.start() {
        Ok(op) => *m.op.borrow_mut() = Some(op),
        Err(e) => *m.error.borrow_mut() = Some(e.to_string()),
    }
    m.apply_state();
    window::refresh(app);
}

pub fn stop(app: &Rc<App>, m: &Rc<MachineUi>, how: StopKind) {
    match m.machine.stop(how) {
        Ok(op) => {
            m.stop_requested.set(true);
            *m.op.borrow_mut() = Some(op);
        }
        Err(e) => *m.error.borrow_mut() = Some(e.to_string()),
    }
    m.apply_state();
    window::refresh(app);
}

pub fn control(app: &Rc<App>, m: &Rc<MachineUi>, c: Control) {
    if let Err(e) = m.machine.control(c) {
        *m.error.borrow_mut() = Some(e.to_string());
    }
    m.apply_state();
    window::refresh(app);
}

fn handle(app: &Rc<App>, msg: Msg) {
    match msg {
        Msg::Wake(key) => {
            if let Some(m) = by_key(app, &key) {
                let t0 = std::time::Instant::now();
                let before = m.state.get();
                display::pump(app, &m);
                m.apply_state();
                if m.state.get() != before {
                    window::refresh(app);
                }
                display::push_capped(
                    &mut m.stats.borrow_mut().pump_us,
                    t0.elapsed().as_micros() as u32,
                );
            }
        }
        Msg::Event(ev) => match ev.machine {
            None => {
                if ev.kinds.contains(&EventKind::Library) {
                    rescan(app);
                }
            }
            Some(key) => {
                let Some(m) = by_key(app, &key) else { return };
                if ev.kinds.contains(&EventKind::Console) {
                    if let Some(f) = m.console_notify.borrow().as_ref() {
                        f();
                    }
                }
                if ev
                    .kinds
                    .iter()
                    .any(|k| matches!(k, EventKind::State | EventKind::Operation))
                {
                    m.apply_state();
                    window::rebuild_hero(&m);
                    window::refresh(app);
                }
            }
        },
        Msg::Auto(req) => automation::handle(app, req),
        Msg::CopyDone(dir, p, r) => {
            if let Some(m) = app.machines.borrow().iter().find(|m| m.dir == dir).cloned() {
                display::copy_done(&m, p, r);
            }
        }
    }
}

/// Stop every machine this app has open, waiting for each, then quit.
pub fn quit_stopping(app: &Rc<App>) {
    let ops: Vec<_> = app
        .machines
        .borrow()
        .iter()
        .filter(|m| m.state.get().is_up())
        .filter_map(|m| {
            m.machine
                .stop(StopKind::Clean)
                .map_err(|e| eprintln!("origami-gtk: stopping {}: {e}", m.name))
                .ok()
        })
        .collect();
    let app = app.clone();
    glib::spawn_future_local(async move {
        let _ = gio::spawn_blocking(move || {
            for op in ops {
                op.wait(None);
            }
        })
        .await;
        app.app.quit();
    });
}

pub fn running_machines(app: &App) -> Vec<Rc<MachineUi>> {
    app.machines
        .borrow()
        .iter()
        .filter(|m| m.state.get().is_up())
        .cloned()
        .collect()
}

/// Quit, asking what to do with machines still running. They outlive the app
/// by design: "keep running" is a real answer.
pub fn request_quit(app: &Rc<App>) {
    let running = running_machines(app);
    let Some(win) = app.windows.borrow().first().map(|w| w.win.clone()) else {
        app.app.quit();
        return;
    };
    if running.is_empty() {
        app.app.quit();
        return;
    }
    let names: Vec<_> = running.iter().map(|m| m.name.clone()).collect();
    let dialog = adw::AlertDialog::new(
        Some("Quit with machines running?"),
        Some(&format!(
            "Still running: {}. They keep running without the app unless you power them off.",
            names.join(", ")
        )),
    );
    dialog.add_responses(&[
        ("cancel", "Cancel"),
        ("keep", "Keep Running"),
        ("stop", "Power Off All"),
    ]);
    dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("keep"));
    dialog.set_close_response("cancel");
    let app = app.clone();
    glib::spawn_future_local(async move {
        match dialog.choose_future(Some(&win)).await.as_str() {
            "keep" => app.app.quit(),
            "stop" => quit_stopping(&app),
            _ => {}
        }
    });
}
