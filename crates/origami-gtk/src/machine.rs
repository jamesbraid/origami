//! One machine as the user sees it: a tab that is a hero card while the
//! machine is stopped, a start screen while it comes up, and its screen once
//! it runs. The tab's state and the words for it come from the engine's
//! facts, never from what the UI last asked for.

use crate::app::App;
use crate::crisp::{Crisp, Place};
use crate::library::Entry;
use crate::prefs::PointerMode;
use crate::view_scale::{self, Layout, ViewScale};
use crate::vk;
use adw::prelude::*;
use gtk::{gdk, glib};
use origami_core::facade::{HookId, Machine, MachineState, Operation, OperationState};
use origami_engine::ImportedUpdate;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a machine may run without a picture before the tab says so.
const NO_PICTURE_AFTER: Duration = Duration::from_secs(25);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Stopped,
    Starting,
    Running,
    NoPicture,
    Paused,
    Stopping,
    Crashed,
    /// A runner from an earlier session is still up.
    LeftRunning,
}

impl State {
    /// The pill's words and colour. A colour always comes with a word.
    pub fn pill(self) -> (&'static str, &'static str) {
        match self {
            State::Stopped => ("Stopped", "pill-grey"),
            State::Starting => ("Starting", "pill-amber"),
            State::Running => ("Running", "pill-green"),
            State::NoPicture => ("No picture", "pill-red"),
            State::Paused => ("Paused", "pill-amber"),
            State::Stopping => ("Saving", "pill-amber"),
            State::Crashed => ("Stopped unexpectedly", "pill-red"),
            State::LeftRunning => ("Left running", "pill-amber"),
        }
    }

    /// The tab's dot, the pill's colour.
    fn dot(self) -> u32 {
        match self.pill().1 {
            "pill-green" => 0x33d17a,
            "pill-amber" => 0xe5a50a,
            "pill-red" => 0xe01b24,
            _ => 0x8d8d93,
        }
    }

    pub fn is_up(self) -> bool {
        matches!(
            self,
            State::Starting
                | State::Running
                | State::NoPicture
                | State::Paused
                | State::LeftRunning
                | State::Stopping
        )
    }
}

#[derive(Default)]
pub struct FrameStats {
    pub imported: u64,
    pub copied_rows: u64,
    pub retries: u64,
    pub copy_us: Vec<u32>,
    pub pump_us: Vec<u32>,
    pub first_picture: Option<Instant>,
    pub presents_while_hidden: u64,
    /// Frame wake-ups the engine posted, and pictures shown.
    pub wakes: u64,
    pub presents: u64,
}

pub struct MachineUi {
    pub dir: PathBuf,
    pub name: String,
    pub entry: RefCell<Entry>,
    pub machine: Machine,
    pub state: Cell<State>,
    pub started_at: Cell<Option<Instant>>,
    /// The start or stop in flight, to read its failure when it ends.
    pub op: RefCell<Option<Operation>>,
    pub stop_requested: Cell<bool>,
    /// A runner from an earlier session is still up and not yet attached.
    pub left_running: Cell<bool>,
    /// Set while a frame wake is queued, so the engine thread posts at most one.
    pub wake_pending: Arc<AtomicBool>,
    pub hook: Cell<Option<HookId>>,
    /// Called when the console has news; the Console window sets it.
    pub console_notify: RefCell<Option<Box<dyn Fn()>>>,
    /// A headless machine's console page, and its own update hook.
    pub console_tab: RefCell<Option<crate::console::Open>>,
    pub console_tab_notify: RefCell<Option<Box<dyn Fn()>>>,
    pub error: RefCell<Option<String>>,

    // widgets
    pub root: gtk::Stack,
    pub hero: gtk::Box,
    pub starting_line: gtk::Label,
    /// The start page, its spinner, and the cube that replaces the spinner
    /// while the machine is starting and the GPU allows one.
    starting: gtk::Box,
    spinner: gtk::Widget,
    cube: RefCell<Option<crate::cube::Cube>>,
    pub screen: gtk::Overlay,
    pub picture: gtk::Picture,
    offload: gtk::GraphicsOffload,
    /// A run's picture has appeared and the window has not been sized for it yet.
    first_fit: Cell<bool>,
    /// The window was sized for this run's picture; cleared when the run ends.
    sized: Cell<bool>,
    /// The run whose picture is on screen, for `Machine::keep_picture`.
    pub run: Cell<u64>,
    /// The guest pixel last sent as the pointer's place, for tests.
    pub pointer_at: Cell<(i32, i32)>,
    place: Cell<Place>,
    pub status: gtk::Label,
    pub hud: gtk::Label,
    pub cpu: crate::cpu_panel::Panel,
    pub page: RefCell<Option<adw::TabPage>>,
    /// The state the hero card was last built for.
    pub hero_state: Cell<Option<State>>,

    // display
    pub scanout: RefCell<Option<vk::Scanout>>,
    pub last: RefCell<Option<ImportedUpdate>>,
    pub shown_slot: Cell<Option<usize>>,
    pub size: Cell<(u32, u32)>,
    pub seq: Cell<u64>,
    pub last_frame: Cell<Instant>,
    pub fallback: RefCell<Vec<u8>>,
    pub pixels: RefCell<Vec<u8>>,
    pub stats: RefCell<FrameStats>,
    pub retry_pending: Cell<bool>,

    // input
    pub pointer: Cell<PointerMode>,
    pub captured: Cell<bool>,
    pub last_pos: Cell<Option<(f64, f64)>>,
    /// The window system's lock on the pointer while captured.
    pub lock: RefCell<Option<crate::pointer_lock::PointerLock>>,
    /// Motion smaller than a count, carried to the next event.
    pub frac: Cell<(f64, f64)>,
    /// Relative counts sent since the app started, for tests.
    pub motion_total: Cell<(i64, i64)>,
    pub ctrl_alt: Cell<(bool, bool)>,
    pub inputs: Cell<u64>,
    pub hud_timer: RefCell<Option<glib::SourceId>>,
    /// A control call is in flight on a worker; no second one starts until it ends.
    pub busy: Cell<bool>,
    pub console: RefCell<Option<crate::console::Open>>,
    pub diagnostics: RefCell<Option<crate::diagnostics::Window>>,
}

/// A window size from the monitor, chrome (window minus picture view), guest and scale factor.
type WindowFor = dyn Fn((i32, i32), (i32, i32), (u32, u32), f64) -> Option<(i32, i32)>;

impl MachineUi {
    pub fn new(app: &Rc<App>, entry: Entry, machine: Machine) -> Rc<MachineUi> {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Fill)
            .can_shrink(true)
            .build();
        let offload = gtk::GraphicsOffload::new(Some(&picture));
        offload.set_enabled(gtk::GraphicsOffloadEnabled::Enabled);
        offload.set_property("black-background", true);
        let status = gtk::Label::builder()
            .wrap(true)
            .max_width_chars(60)
            .justify(gtk::Justification::Center)
            .css_classes(["title-3", "plate", "on-dark"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let hud = gtk::Label::builder()
            .css_classes(["hud"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .margin_top(18)
            .visible(false)
            .can_target(false)
            .build();
        // The offload paints black over its whole area, so it is placed on the
        // picture's rectangle (see `view_scale`) and the letterbox stays granite.
        let screen = gtk::Overlay::builder()
            .focusable(true)
            .focus_on_click(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        let letterbox = crate::granite::Granite::new(crate::granite::neutral());
        letterbox.append(&screen);
        screen.add_overlay(&offload);
        screen.add_overlay(&status);
        screen.add_overlay(&hud);
        let cpu = crate::cpu_panel::Panel::new();
        screen.update_property(&[gtk::accessible::Property::Label(&format!(
            "{} screen",
            entry.name
        ))]);

        let hero = gtk::Box::new(gtk::Orientation::Vertical, 18);
        let starting_line = gtk::Label::builder()
            .css_classes(["dim-label", "monospace"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let starting = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(14)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .vexpand(true)
            .css_classes(["plate"])
            .build();
        let surround = gtk::Box::builder().css_classes(["start-surround"]).build();
        surround.append(&starting);
        let spinner: gtk::Widget = gtk::Spinner::builder()
            .spinning(true)
            .width_request(48)
            .height_request(48)
            .build()
            .upcast();
        let heading = gtk::Label::builder()
            .label("Starting up the system…")
            .css_classes(["title-2"])
            .build();
        starting.append(&spinner);
        starting.append(&heading);
        starting.append(&starting_line);
        for w in [&heading, &starting_line] {
            w.add_css_class("on-dark");
        }
        let root = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        root.add_named(
            &gtk::ScrolledWindow::builder()
                .child(&hero)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
            Some("hero"),
        );
        root.add_named(&surround, Some("starting"));
        // The CPU panel sits under the picture, not over it: a native view
        // cannot be drawn over on every platform.
        letterbox.set_vexpand(true);
        let with_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        with_panel.append(&letterbox);
        with_panel.append(&cpu.root);
        root.add_named(&with_panel, Some("screen"));

        let pointer = app.prefs.borrow().pointer_for(&entry.dir);
        let m = Rc::new(MachineUi {
            dir: entry.dir.clone(),
            name: entry.name.clone(),
            entry: RefCell::new(entry),
            machine,
            state: Cell::new(State::Stopped),
            started_at: Cell::new(None),
            op: RefCell::new(None),
            stop_requested: Cell::new(false),
            left_running: Cell::new(false),
            wake_pending: Arc::new(AtomicBool::new(false)),
            hook: Cell::new(None),
            console_notify: RefCell::new(None),
            console_tab: RefCell::new(None),
            console_tab_notify: RefCell::new(None),
            error: RefCell::new(None),
            root,
            hero,
            starting_line,
            starting: starting.clone(),
            spinner: spinner.clone(),
            cube: RefCell::new(None),
            screen,
            picture,
            offload,
            first_fit: Cell::new(false),
            sized: Cell::new(false),
            run: Cell::new(0),
            pointer_at: Cell::new((-1, -1)),
            place: Cell::new(Place::default()),
            status,
            hud,
            cpu,
            page: RefCell::new(None),
            hero_state: Cell::new(None),
            scanout: RefCell::new(None),
            last: RefCell::new(None),
            shown_slot: Cell::new(None),
            size: Cell::new((0, 0)),
            seq: Cell::new(0),
            last_frame: Cell::new(Instant::now()),
            fallback: RefCell::new(Vec::new()),
            pixels: RefCell::new(Vec::new()),
            stats: RefCell::new(FrameStats::default()),
            retry_pending: Cell::new(false),
            pointer: Cell::new(pointer),
            captured: Cell::new(false),
            last_pos: Cell::new(None),
            lock: RefCell::new(None),
            frac: Cell::new((0.0, 0.0)),
            motion_total: Cell::new((0, 0)),
            ctrl_alt: Cell::new((false, false)),
            inputs: Cell::new(0),
            hud_timer: RefCell::new(None),
            busy: Cell::new(false),
            console: RefCell::new(None),
            diagnostics: RefCell::new(None),
        });
        m.place_picture();
        m
    }

    /// The picture's rectangle in the screen widget, once there is a picture.
    pub fn layout(&self) -> Option<Layout> {
        view_scale::layout(
            (self.screen.width(), self.screen.height()),
            self.size.get(),
            self.scale_factor(),
        )
    }

    /// Device pixels per logical pixel, fractional where the window system says so.
    pub fn scale_factor(&self) -> f64 {
        self.screen
            .native()
            .and_then(|n| n.surface())
            .map_or_else(|| f64::from(self.screen.scale_factor()), |s| s.scale())
    }

    /// Show a new picture, smoothed or not as the current layout wants.
    pub fn set_texture(&self, tex: &gdk::Texture) {
        self.picture
            .set_paintable(Some(&Crisp::new(tex, self.place.get())));
    }

    /// The window is not to be sized for this run's picture.
    pub fn mark_sized(&self) {
        self.sized.set(true);
    }

    /// A run's picture just appeared: size the window for it once it is laid out.
    pub fn picture_appeared(&self) {
        if !self.sized.replace(true) {
            self.first_fit.set(true);
        }
        self.screen.queue_allocate();
    }

    /// Resize the window once to a View scale; Fit never resizes.
    pub fn resize_window(self: &Rc<Self>, scale: ViewScale) {
        self.size_window(
            Rc::new(move |mon, chrome, g, f| view_scale::scale_window(scale, mon, chrome, g, f)),
            (0, 0),
            false,
        );
    }

    /// Ask for the window `want` gives. `slack` is how far a default size has
    /// overshot the window's real size (X11 frames); measured once, then corrected.
    fn size_window(self: &Rc<Self>, want: Rc<WindowFor>, slack: (i32, i32), retried: bool) {
        let Some(win) = self.screen.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let monitor = win
            .surface()
            .and_then(|s| self.screen.display().monitor_at_surface(&s));
        let (Some(mon), false, false) = (monitor, win.is_maximized(), win.is_fullscreen()) else {
            return;
        };
        let g = mon.geometry();
        let chrome = (
            win.width() - self.screen.width(),
            win.height() - self.screen.height(),
        );
        let Some((w, h)) = want(
            (g.width(), g.height()),
            chrome,
            self.size.get(),
            self.scale_factor(),
        ) else {
            return;
        };
        let ask = (w + slack.0, h + slack.1);
        win.set_default_size(ask.0, ask.1);
        if slack == (0, 0) && !retried {
            let weak = Rc::downgrade(self);
            glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                let Some(m) = weak.upgrade() else { return };
                let Some(win) = m.screen.root().and_downcast::<gtk::Window>() else {
                    return;
                };
                let slack = (ask.0 - win.width(), ask.1 - win.height());
                // Correct a frame-sized shortfall (libadwaita on X11), not a tiling compositor.
                if (1..=16).contains(&slack.0) && (1..=16).contains(&slack.1) {
                    m.size_window(want, slack, true);
                    return;
                }
                // The sidebar grows with the window, so the picture's area came out smaller than asked
                // for: ask once more with the chrome as it is now.
                let short = (
                    w - chrome.0 - m.screen.width(),
                    h - chrome.1 - m.screen.height(),
                );
                if short.0.max(short.1) > 1
                    && short.0.min(short.1) >= 0
                    && short.0.max(short.1) <= 200
                {
                    m.size_window(want, (0, 0), true);
                }
            });
        }
    }

    /// Put the picture where `layout` says, and lay it out again when the
    /// display's scale changes.
    fn place_picture(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.screen.connect_get_child_position(move |_, child| {
            let m = weak.upgrade()?;
            if child != m.offload.upcast_ref::<gtk::Widget>() {
                return None;
            }
            let l = m.layout()?;
            let place = Place {
                nearest: l.nearest,
                factor: l.factor,
                off: l.offset(),
                size: (f64::from(l.dev.2), f64::from(l.dev.3)),
            };
            m.place.set(place);
            if m.first_fit.replace(false) {
                let w = Rc::downgrade(&m);
                glib::idle_add_local_once(move || {
                    let Some(m) = w.upgrade() else { return };
                    let Some(win) = m.screen.root().and_downcast::<gtk::Window>() else {
                        return;
                    };
                    let now = (win.width(), win.height());
                    m.size_window(
                        Rc::new(move |mon, chrome, g, _| {
                            view_scale::grow_only(now, view_scale::first_window(mon, chrome, g))
                        }),
                        (0, 0),
                        false,
                    );
                });
            }
            if let Some(c) = m
                .picture
                .paintable()
                .and_then(|p| p.downcast::<Crisp>().ok())
            {
                c.set_place(place);
            }
            let (x, y, w, h) = l.alloc();
            Some(gdk::Rectangle::new(x, y, w, h))
        });
        self.screen
            .connect_scale_factor_notify(|s| s.queue_allocate());
        let weak = Rc::downgrade(self);
        let hooked = Cell::new(false);
        self.screen.connect_map(move |s| {
            let (Some(m), Some(surface)) = (weak.upgrade(), s.native().and_then(|n| n.surface()))
            else {
                return;
            };
            if hooked.replace(true) {
                return;
            }
            let w = Rc::downgrade(&m);
            surface.connect_scale_notify(move |_| {
                if let Some(m) = w.upgrade() {
                    m.screen.queue_allocate();
                }
            });
        });
    }

    /// The state the core reports right now.
    pub fn compute_state(&self) -> State {
        let v = self.machine.view();
        match v.state {
            MachineState::LeftRunning => State::LeftRunning,
            MachineState::NotRunning if self.left_running.get() => State::LeftRunning,
            MachineState::NotRunning => State::Stopped,
            MachineState::Starting => State::Starting,
            MachineState::Paused => State::Paused,
            MachineState::Stopping => State::Stopping,
            MachineState::Exited => {
                if self.stop_requested.get() || v.exit_code == Some(0) {
                    State::Stopped
                } else {
                    State::Crashed
                }
            }
            // A machine with no display has no picture to wait for.
            MachineState::Running if !v.has_display => State::Running,
            MachineState::Running => {
                let active = v.heads.first().is_some_and(|h| h.active);
                if active && self.size.get() != (0, 0) {
                    State::Running
                } else if active {
                    State::Starting
                } else {
                    match self.started_at.get() {
                        Some(t) if t.elapsed() < NO_PICTURE_AFTER => State::Starting,
                        _ => State::NoPicture,
                    }
                }
            }
        }
    }

    /// The start page's cube exists only while the machine is starting, so a
    /// library of stopped machines holds no GPU memory.
    fn show_cube(&self, starting: bool) {
        let mut cube = self.cube.borrow_mut();
        match (starting, cube.is_some()) {
            (true, false) => {
                let style = crate::cube::starting_style(self.entry.borrow().colour());
                if let Some(c) = crate::cube::Cube::new(108, 108, style) {
                    self.spinner.set_visible(false);
                    self.starting.prepend(&c.widget);
                    *cube = Some(c);
                }
            }
            (false, true) => {
                if let Some(c) = cube.take() {
                    self.starting.remove(&c.widget);
                }
                self.spinner.set_visible(true);
            }
            _ => {}
        }
    }

    /// Show the page for the current state and the words that belong on it.
    pub fn apply_state(&self) {
        let state = self.compute_state();
        let before = self.state.replace(state);
        // The first picture takes the keyboard, so typing works without a
        // click first (and in a compositor with no pointer to click with).
        if state == State::Running && before != State::Running && self.root.is_mapped() {
            match self.console_tab.borrow().as_ref() {
                Some(c) => c.focus(),
                None => {
                    self.screen.grab_focus();
                }
            }
        }
        self.read_operation();
        self.show_cube(state == State::Starting);
        let page = match state {
            State::Stopped | State::Crashed | State::LeftRunning | State::Stopping => "hero",
            State::Starting => "starting",
            State::Running | State::NoPicture | State::Paused
                if !self.machine.view().has_display =>
            {
                "console"
            }
            State::Running | State::NoPicture | State::Paused => "screen",
        };
        // Stopping keeps the last picture's place; the hero says Saving.
        if self.root.visible_child_name().as_deref() != Some(page) {
            self.root.set_visible_child_name(page);
        }
        let reason = self.machine.view().reason;
        match state {
            State::NoPicture => {
                self.status.set_label(&reason);
                self.status.set_visible(true);
            }
            State::Running => self.status.set_visible(false),
            State::Starting => self.starting_line.set_label(&reason),
            _ => {}
        }
        if let Some(p) = self.page.borrow().as_ref() {
            p.set_title(&self.name);
            p.set_indicator_icon(Some(&crate::art::dot(state.dot())));
            // The dot is a colour; the tooltip says the word.
            p.set_tooltip(&format!(
                "{}\n{}",
                state.pill().0,
                glib::markup_escape_text(&self.dir.display().to_string())
            ));
        }
        if matches!(state, State::Stopped | State::Crashed) {
            self.size.set((0, 0));
            self.sized.set(false);
            self.release_input();
        }
    }

    /// The failure of the start or stop that just ended, as the hero shows it.
    fn read_operation(&self) {
        let mut op = self.op.borrow_mut();
        let Some(o) = op.as_ref() else { return };
        match o.state() {
            OperationState::Failed(e) => {
                *self.error.borrow_mut() = Some(e.to_string());
                *op = None;
            }
            OperationState::Running => {}
            _ => *op = None,
        }
    }
}
