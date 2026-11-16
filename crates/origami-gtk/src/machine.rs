//! One machine as the user sees it: a tab that is a hero card while the
//! machine is stopped, a start screen while it comes up, and its screen once
//! it runs. The tab's state and the words for it come from the engine's
//! facts, never from what the UI last asked for.

use crate::app::App;
use crate::library::Entry;
use crate::prefs::PointerMode;
use crate::vk;
use adw::prelude::*;
use gtk::glib;
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
    pub error: RefCell<Option<String>>,

    // widgets
    pub root: gtk::Stack,
    pub hero: gtk::Box,
    pub starting_line: gtk::Label,
    pub screen: gtk::Overlay,
    pub picture: gtk::Picture,
    pub status: gtk::Label,
    pub hud: gtk::Label,
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
    pub ctrl_alt: Cell<(bool, bool)>,
    pub inputs: Cell<u64>,
    pub hud_timer: RefCell<Option<glib::SourceId>>,
    pub console: RefCell<Option<adw::Window>>,
}

impl MachineUi {
    pub fn new(app: &Rc<App>, entry: Entry, machine: Machine) -> Rc<MachineUi> {
        let picture = gtk::Picture::builder()
            .content_fit(if app.cfg.one_to_one {
                gtk::ContentFit::ScaleDown
            } else {
                gtk::ContentFit::Contain
            })
            .can_shrink(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        let offload = gtk::GraphicsOffload::new(Some(&picture));
        offload.set_enabled(gtk::GraphicsOffloadEnabled::Enabled);
        offload.set_property("black-background", true);
        let status = gtk::Label::builder()
            .wrap(true)
            .max_width_chars(60)
            .justify(gtk::Justification::Center)
            .css_classes(["title-3"])
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
        let screen = gtk::Overlay::builder()
            .child(&offload)
            .focusable(true)
            .focus_on_click(true)
            .hexpand(true)
            .vexpand(true)
            .css_classes(["screen-surround"])
            .build();
        screen.add_overlay(&status);
        screen.add_overlay(&hud);
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
            .css_classes(["screen-surround"])
            .hexpand(true)
            .vexpand(true)
            .build();
        let spinner = gtk::Spinner::builder()
            .spinning(true)
            .width_request(48)
            .height_request(48)
            .build();
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
        root.add_named(&starting, Some("starting"));
        root.add_named(&screen, Some("screen"));

        let pointer = app.prefs.borrow().pointer_for(&entry.dir);
        Rc::new(MachineUi {
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
            error: RefCell::new(None),
            root,
            hero,
            starting_line,
            screen,
            picture,
            status,
            hud,
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
            ctrl_alt: Cell::new((false, false)),
            inputs: Cell::new(0),
            hud_timer: RefCell::new(None),
            console: RefCell::new(None),
        })
    }

    /// The state the core reports right now.
    pub fn compute_state(&self) -> State {
        let v = self.machine.view();
        match v.state {
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

    /// Show the page for the current state and the words that belong on it.
    pub fn apply_state(&self) {
        let state = self.compute_state();
        let before = self.state.replace(state);
        // The first picture takes the keyboard, so typing works without a
        // click first (and in a compositor with no pointer to click with).
        if state == State::Running && before != State::Running && self.root.is_mapped() {
            self.screen.grab_focus();
        }
        self.read_operation();
        let page = match state {
            State::Stopped | State::Crashed | State::LeftRunning | State::Stopping => "hero",
            State::Starting => "starting",
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
            let warn = gtk::gio::ThemedIcon::new("dialog-warning-symbolic");
            p.set_indicator_icon(if matches!(state, State::NoPicture | State::Crashed) {
                Some(&warn)
            } else {
                None
            });
        }
        if matches!(state, State::Stopped | State::Crashed) {
            self.size.set((0, 0));
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
