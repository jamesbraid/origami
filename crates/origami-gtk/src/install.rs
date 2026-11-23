//! The Install IRIX assistant window. The core's `InstallFlow` owns the steps and
//! their rules; this file builds widgets from it and runs the calls it needs.

use crate::app::{self, App};
use crate::machine::MachineUi;
use crate::{disks, library, window};
use adw::prelude::*;
use gtk::glib;
use origami_core::facade::UiString;
use origami_core::facade::{
    disk_labels, install_media_folder, install_releases, texts, ApplyState, BootConsole,
    CheckState, CloseAction, CloseQuestion, Core, CoreResult, DiskState, GuideStep, InstallFailure,
    InstallFlow, InstallGuide, InstallMethod, InstallPlan, InstallStep, MachineState, MediaFolder,
    Operation, OperationState, StopKind,
};
use origami_core::{Change, Kind};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    static OPEN: RefCell<Vec<Rc<Asst>>> = const { RefCell::new(Vec::new()) };
}

fn assistants() -> Vec<Rc<Asst>> {
    OPEN.with(|o| o.borrow().clone())
}

/// Whether an install is running on the machine `key` (any machine, when None).
pub fn install_running(key: Option<&str>) -> bool {
    assistants()
        .iter()
        .any(|a| key.is_none_or(|k| a.m.machine.key() == k) && a.server_running())
}

/// End the running installs (all, or the one for `key`): the server, then the machine.
pub fn stop_installs(app: &Rc<App>, key: Option<&str>) {
    for a in assistants() {
        if key.is_none_or(|k| a.m.machine.key() == k) && a.server_running() {
            a.stop_server();
            if a.m.state.get().is_up() {
                app::stop(app, &a.m, StopKind::Force);
            }
            if key.is_some() {
                a.dismiss();
            }
        }
    }
}

enum Server {
    Off,
    Running(Operation),
    Failed(String),
}

struct Asst {
    app: Rc<App>,
    m: Rc<MachineUi>,
    win: adw::Window,
    flow: RefCell<InstallFlow>,
    plan_line: RefCell<Option<String>>,
    /// The PROM prints on a serial line, so Type It makes sense.
    serial: bool,
    media: RefCell<Option<MediaFolder>>,
    guide: RefCell<Option<InstallGuide>>,
    ticks: RefCell<HashSet<(usize, usize)>>,
    type_buttons: RefCell<Vec<gtk::Button>>,
    server: RefCell<Server>,
    apply_op: RefCell<Option<Operation>>,
    apply_msg: RefCell<String>,
    /// Bumped on every step change, so a page's timer ends with its page.
    page: Cell<u32>,
    check_op: RefCell<Option<Operation>>,
    /// The machine's install setup is written, so the check need not write it again.
    set_up: Cell<bool>,
    seen_up: Cell<bool>,
    busy: Cell<bool>,
    closing: Cell<bool>,
    content: gtk::Box,
    rows: Vec<gtk::Label>,
    error: gtk::Label,
    banner: adw::Banner,
    footer: gtk::Label,
    restart: gtk::Button,
    start_over: gtk::Button,
    cancel: gtk::Button,
    back: gtk::Button,
    next: gtk::Button,
}

/// The last `NN%` in a console line.
fn last_percent(line: &str) -> Option<u32> {
    let head = &line[..line.rfind('%')?];
    let digits = head.len() - head.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    head[head.len() - digits..]
        .parse()
        .ok()
        .filter(|p| *p <= 100)
}

fn col(spacing: i32) -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(spacing)
        .build()
}

fn row() -> gtk::Box {
    gtk::Box::builder().spacing(8).build()
}

fn append(parent: &gtk::Box, widgets: &[&gtk::Widget]) {
    for w in widgets {
        parent.append(*w);
    }
}

fn margins(w: &impl IsA<gtk::Widget>, top: i32, bottom: i32, start: i32, end: i32) {
    w.set_margin_top(top);
    w.set_margin_bottom(bottom);
    w.set_margin_start(start);
    w.set_margin_end(end);
}

fn text(s: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(s)
        .wrap(true)
        .xalign(0.0)
        .build()
}

fn note(s: &str) -> gtk::Label {
    let l = text(s);
    l.add_css_class("dim-label");
    l.set_margin_start(28);
    l
}

fn named<W: IsA<gtk::Widget>>(w: W, name: &str) -> W {
    w.set_widget_name(name);
    w
}

fn button(label: &str, name: &str) -> gtk::Button {
    named(
        gtk::Button::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .build(),
        name,
    )
}

fn check(label: &str, name: &str, on: bool) -> gtk::CheckButton {
    named(
        gtk::CheckButton::builder().label(label).active(on).build(),
        name,
    )
}

/// A radio button joining `group`, which the first one starts.
fn radio(
    label: &str,
    name: &str,
    on: bool,
    group: &mut Option<gtk::CheckButton>,
) -> gtk::CheckButton {
    let b = check(label, name, on);
    match group {
        Some(g) => b.set_group(Some(g)),
        None => *group = Some(b.clone()),
    }
    b
}

fn action_id(a: CloseAction) -> &'static str {
    match a {
        CloseAction::Stay => "stay",
        CloseAction::StopMachine => "stop",
        CloseAction::LeaveRunning | CloseAction::Leave => "leave",
        CloseAction::SetBack => "set",
    }
}

/// An alert with these `(id, label)` buttons. Return picks `default`, Escape the last; the
/// answer is the chosen id.
pub async fn question(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    buttons: &[(&str, &str)],
    default: usize,
) -> String {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.set_widget_name("install.ask");
    for (id, label) in buttons {
        dialog.add_response(id, label);
        if matches!(*id, "quit" | "stop") {
            dialog.set_response_appearance(id, adw::ResponseAppearance::Destructive);
        }
    }
    dialog.set_default_response(buttons.get(default).map(|b| b.0));
    dialog.set_close_response(buttons.last().map_or("", |b| b.0));
    dialog.choose_future(Some(parent)).await.to_string()
}

/// The prompt for quitting or closing a tab while an install runs; true to stop it.
pub async fn stop_install(parent: &impl IsA<gtk::Widget>, tab: bool) -> bool {
    let (body, stop) = if tab {
        (texts::TAB_TEXT, texts::STOP_AND_CLOSE)
    } else {
        (texts::QUIT_TEXT, texts::STOP_AND_QUIT)
    };
    let buttons = [("keep", texts::KEEP), ("quit", stop)];
    question(parent, texts::QUIT_HEADING, body, &buttons, 0).await == "quit"
}

/// Open the assistant on `m`, or bring its window forward if it is open.
pub fn open(app: &Rc<App>, parent: &impl IsA<gtk::Window>, m: &Rc<MachineUi>) {
    let key = m.machine.key().to_string();
    let existing = assistants().into_iter().find(|a| a.m.machine.key() == key);
    if let Some(a) = existing {
        return a.win.present();
    }
    let (app, parent, m) = (
        app.clone(),
        parent.clone().upcast::<gtk::Window>(),
        m.clone(),
    );
    glib::spawn_future_local(async move {
        let (core, machine) = (app.core.clone(), m.machine.clone());
        let r = gtk::gio::spawn_blocking(move || {
            let plan = core.install_plan(&key).map_err(|e| e.to_string())?;
            let serial = machine.boot_console().map_err(|e| e.to_string())? == BootConsole::Serial;
            let pending = core
                .install_restore_pending(&key)
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((plan, serial, pending))
        })
        .await
        .unwrap_or_else(|_| Err("the machine could not be read".into()));
        match r {
            Ok((plan, serial, pending)) => {
                let key = m.machine.key();
                if assistants().iter().any(|a| a.m.machine.key() == key) {
                    return;
                }
                let a = Asst::new(&app, &parent, &m, plan, serial, pending);
                OPEN.with(|o| o.borrow_mut().push(a.clone()));
                a.win.present();
            }
            Err(e) => {
                let d = adw::AlertDialog::new(Some("Cannot start the assistant"), Some(&e));
                d.add_response("ok", "OK");
                d.present(Some(&parent));
            }
        }
    });
}

impl Asst {
    fn new(
        app: &Rc<App>,
        parent: &gtk::Window,
        m: &Rc<MachineUi>,
        plan: Option<InstallPlan>,
        serial: bool,
        pending: bool,
    ) -> Rc<Asst> {
        let mut flow = InstallFlow::new(install_releases(), plan.clone(), disk_labels().default_gb);
        // A machine left on the installation network offers to set it back on close.
        if pending {
            flow.set_prepared();
        }
        flow.set_serial(serial);
        let title = format!("Install IRIX: {}", m.name);
        let win = named(
            adw::Window::builder()
                .title(&title)
                .default_width(860)
                .default_height(600)
                .transient_for(parent)
                .build(),
            "install.window",
        );
        let list = col(2);
        let rows: Vec<gtk::Label> = flow
            .steps()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let l = gtk::Label::builder()
                    .label(format!("{}. {}", i + 1, s.title()))
                    .xalign(0.0)
                    .margin_top(4)
                    .margin_bottom(4)
                    .build();
                l.set_widget_name("install.step-row");
                list.append(&l);
                l
            })
            .collect();
        margins(&list, 18, 0, 18, 12);
        list.set_width_request(190);
        let content = col(12);
        margins(&content, 18, 12, 18, 18);
        let error = named(text(""), "install.error");
        error.add_css_class("error");
        error.set_visible(false);
        margins(&error, 0, 0, 18, 18);
        let banner = named(
            adw::Banner::builder()
                .title(UiString::TheMachineStopped.text())
                .button_label(UiString::StartAgain.text())
                .build(),
            "install.stopped",
        );
        let scroll = gtk::ScrolledWindow::builder()
            .child(&content)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let right = col(0);
        right.set_hexpand(true);
        append(
            &right,
            &[banner.upcast_ref(), error.upcast_ref(), scroll.upcast_ref()],
        );
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        append(
            &body,
            &[
                list.upcast_ref(),
                gtk::Separator::new(gtk::Orientation::Vertical).upcast_ref(),
                right.upcast_ref(),
            ],
        );
        let footer = named(
            gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .wrap(true)
                .build(),
            "install.footer",
        );
        let restart = button(UiString::Restart.text(), "install.restart");
        let start_over = button(UiString::StartOver.text(), "install.start-over");
        let cancel = button(UiString::Cancel.text(), "install.cancel");
        let back = button(UiString::Back.text(), "install.back");
        let next = button("Continue", "install.continue");
        next.add_css_class("suggested-action");
        let bar = row();
        margins(&bar, 8, 8, 18, 18);
        append(
            &bar,
            &[
                footer.upcast_ref(),
                restart.upcast_ref(),
                start_over.upcast_ref(),
                cancel.upcast_ref(),
                back.upcast_ref(),
                next.upcast_ref(),
            ],
        );
        let view = adw::ToolbarView::new();
        view.add_top_bar(
            &adw::HeaderBar::builder()
                .title_widget(&adw::WindowTitle::new(&title, ""))
                .build(),
        );
        view.set_content(Some(&body));
        view.add_bottom_bar(&bar);
        win.set_content(Some(&view));
        let a = Rc::new(Asst {
            app: app.clone(),
            m: m.clone(),
            win,
            set_up: Cell::new(plan.is_some()),
            plan_line: RefCell::new(plan.map(|p| p.summary)),
            flow: RefCell::new(flow),
            serial,
            media: RefCell::new(None),
            guide: RefCell::new(None),
            ticks: RefCell::new(HashSet::new()),
            type_buttons: RefCell::new(Vec::new()),
            server: RefCell::new(Server::Off),
            apply_op: RefCell::new(None),
            apply_msg: RefCell::new(String::new()),
            page: Cell::new(0),
            check_op: RefCell::new(None),
            seen_up: Cell::new(false),
            busy: Cell::new(false),
            closing: Cell::new(false),
            content,
            rows,
            error,
            banner,
            footer,
            restart,
            start_over,
            cancel,
            back,
            next,
        });
        a.wire();
        a.go();
        a
    }

    fn wire(self: &Rc<Self>) {
        let w = Rc::downgrade(self);
        let on = |b: &gtk::Button, f: fn(&Rc<Asst>)| {
            let w = w.clone();
            b.connect_clicked(move |_| {
                if let Some(a) = w.upgrade() {
                    f(&a);
                }
            });
        };
        on(&self.cancel, Asst::request_close);
        on(&self.back, Asst::back_clicked);
        on(&self.next, Asst::next_clicked);
        on(&self.restart, Asst::restart_server);
        on(&self.start_over, Asst::start_over);
        let w2 = w.clone();
        self.banner.connect_button_clicked(move |_| {
            if let Some(a) = w2.upgrade() {
                a.start_machine();
            }
        });
        let w2 = w.clone();
        self.win.connect_close_request(move |_| match w2.upgrade() {
            Some(a) if !a.closing.get() => {
                a.request_close();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        glib::timeout_add_local(Duration::from_millis(500), move || match w.upgrade() {
            Some(a) if !a.closing.get() => {
                a.tick();
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    }

    /// Run `job` off the UI thread, then `then` on it, unless the window is gone.
    fn work<T: Send + 'static>(
        self: &Rc<Self>,
        job: impl FnOnce() -> T + Send + 'static,
        then: impl FnOnce(&Rc<Self>, T) + 'static,
    ) {
        let me = self.clone();
        glib::spawn_future_local(async move {
            let r = gtk::gio::spawn_blocking(job).await;
            if me.closing.get() {
                return;
            }
            match r {
                Ok(v) => then(&me, v),
                Err(_) => me.fail("the work could not be finished"),
            }
        });
    }

    /// A core call for this machine off the UI thread; its error is shown as the core says it.
    fn call<T: Send + 'static>(
        self: &Rc<Self>,
        job: impl FnOnce(&Core, &str) -> CoreResult<T> + Send + 'static,
        then: impl FnOnce(&Rc<Self>, T) + 'static,
    ) {
        let (core, key) = (self.app.core.clone(), self.key());
        self.work(
            move || job(&core, &key).map_err(|e| e.to_string()),
            |me, r| match r {
                Ok(v) => then(me, v),
                Err(e) => me.fail(&e),
            },
        );
    }

    /// As `call`, with the buttons held until it ends.
    fn call_busy<T: Send + 'static>(
        self: &Rc<Self>,
        job: impl FnOnce(&Core, &str) -> CoreResult<T> + Send + 'static,
        then: impl FnOnce(&Rc<Self>, T) + 'static,
    ) {
        self.busy.set(true);
        self.update_nav();
        let (core, key) = (self.app.core.clone(), self.key());
        self.work(
            move || job(&core, &key).map_err(|e| e.to_string()),
            |me, r| {
                me.busy.set(false);
                match r {
                    Ok(v) => then(me, v),
                    Err(e) => me.fail(&e),
                }
                me.update_nav();
            },
        );
    }

    fn fail(&self, message: &str) {
        self.error.set_label(message);
        self.error.set_visible(true);
    }

    fn key(&self) -> String {
        self.m.machine.key().to_string()
    }

    fn step(&self) -> InstallStep {
        self.flow.borrow().step()
    }

    fn machine_up(&self) -> bool {
        !matches!(
            self.m.machine.view().state,
            MachineState::NotRunning | MachineState::Exited
        )
    }

    /// The media server or the automated install is running.
    fn server_running(&self) -> bool {
        let live = |op: &Operation| !op.state().is_finished();
        matches!(&*self.server.borrow(), Server::Running(op) if live(op))
            || self.apply_op.borrow().as_ref().is_some_and(live)
    }

    fn stop_server(&self) {
        if let Some(op) = self.apply_op.borrow().as_ref() {
            op.cancel();
        }
        if let Server::Running(op) = self.server.replace(Server::Off) {
            op.cancel();
        }
    }

    fn refresh_machine(&self) {
        *self.m.entry.borrow_mut() = library::read(&self.m.dir);
        window::rebuild_hero(&self.m);
        window::refresh(&self.app);
    }

    // ---- navigation ---------------------------------------------------------

    /// Show the flow's current step.
    fn go(self: &Rc<Self>) {
        if let Some(op) = self.check_op.take() {
            op.cancel();
        }
        let step = self.step();
        self.page.set(self.page.get() + 1);
        self.error.set_visible(false);
        while let Some(c) = self.content.first_child() {
            self.content.remove(&c);
        }
        self.type_buttons.borrow_mut().clear();
        let steps = self.flow.borrow().steps();
        for (i, r) in self.rows.iter().enumerate() {
            let on = steps.get(i) == Some(&step);
            r.set_css_classes(&[if on { "heading" } else { "dim-label" }]);
        }
        let title = named(text(step.title()), "install.step");
        title.add_css_class("title-2");
        self.content.append(&title);
        match step {
            InstallStep::Release => self.release_page(),
            InstallStep::Media => self.media_page(),
            InstallStep::Software => self.software_page(),
            InstallStep::Method => self.method_page(),
            InstallStep::Disk => self.disk_page(),
            InstallStep::Check => self.check_page(),
            InstallStep::Packages if self.flow.borrow().automatic() => self.apply_page(),
            InstallStep::Finish if self.flow.borrow().automatic() => self
                .content
                .append(&text("IRIX is installed and the machine is stopped.")),
            _ if self.guide.borrow().is_some() => self.guide_page(step),
            _ => self.enter_prepare(),
        }
        self.update_nav();
    }

    fn update_nav(&self) {
        let (flow, busy) = (self.flow.borrow(), self.busy.get());
        self.back.set_sensitive(flow.can_back() && !busy);
        self.next.set_label(flow.continue_label());
        self.next
            .set_sensitive(flow.can_continue(self.machine_up()) && !busy);
        self.start_over.set_visible(flow.resumed());
    }

    fn back_clicked(self: &Rc<Self>) {
        self.flow.borrow_mut().back();
        self.set_up.set(false);
        self.go();
    }

    fn next_clicked(self: &Rc<Self>) {
        if !self.next.is_sensitive() {
            return;
        }
        let no_disk = *self.flow.borrow().disk() == DiskState::None;
        match self.step() {
            InstallStep::Disk if no_disk => self.create_disk(),
            InstallStep::Finish => self.finish(),
            _ => {
                self.flow.borrow_mut().next();
                self.go();
            }
        }
    }

    fn start_over(self: &Rc<Self>) {
        self.stop_server();
        self.call(
            |core, key| core.install_reset(key),
            |me, ()| {
                me.guide.take();
                me.ticks.borrow_mut().clear();
                me.media.take();
                me.plan_line.take();
                me.set_up.set(false);
                me.flow.borrow_mut().restart(disk_labels().default_gb);
                me.go();
            },
        );
    }

    fn method_page(self: &Rc<Self>) {
        self.content.append(&text("How should the install run?"));
        let on = self.flow.borrow().method() == InstallMethod::Automatic;
        let mut group = None;
        let auto = radio("Automatic", "install.method.auto", on, &mut group);
        let guided = radio("Guided", "install.method.guided", !on, &mut group);
        let me = self.clone();
        auto.connect_toggled(move |b| {
            let m = if b.is_active() {
                InstallMethod::Automatic
            } else {
                InstallMethod::Guided
            };
            me.flow.borrow_mut().set_method(m);
        });
        append(
            &self.content,
            &[
                auto.upcast_ref(),
                note("Origami types the package commands for you, logs in when they finish and shuts IRIX down.").upcast_ref(),
                guided.upcast_ref(),
                note("You type each command in the console. Origami shows each step.").upcast_ref(),
            ],
        );
    }

    // ---- steps 1 to 3 -------------------------------------------------------

    fn release_page(self: &Rc<Self>) {
        self.content.append(&text("Which version of IRIX?"));
        let (releases, release, profile) = {
            let f = self.flow.borrow();
            (
                f.releases().to_vec(),
                f.release().to_string(),
                f.profile().to_string(),
            )
        };
        let mut group = None;
        for r in &releases {
            let name = format!("install.release.{}", r.release);
            let b = radio(&r.label, &name, release == r.release, &mut group);
            let (me, id) = (self.clone(), r.release.clone());
            b.connect_toggled(move |b| {
                if b.is_active() {
                    me.flow.borrow_mut().select_release(&id);
                    me.media.take();
                    me.go();
                }
            });
            self.content.append(&b);
            if release != r.release {
                continue;
            }
            let mut pgroup = None;
            for p in &r.profiles {
                let name = format!("install.profile.{}", p.name);
                let b = radio(&p.label, &name, profile == p.name, &mut pgroup);
                b.set_margin_start(28);
                let (me, id) = (self.clone(), p.name.clone());
                b.connect_toggled(move |b| {
                    if b.is_active() {
                        me.flow.borrow_mut().select_profile(&id);
                        me.media.take();
                    }
                });
                let n = note(&p.detail);
                n.set_margin_start(52);
                append(&self.content, &[b.upcast_ref(), n.upcast_ref()]);
            }
        }
    }

    fn media_page(self: &Rc<Self>) {
        let (folder, path) = {
            let f = self.flow.borrow();
            (f.use_folder(), f.folder().cloned())
        };
        let mut group = None;
        let download = radio(
            "Download them from the Origami server",
            "install.media.download",
            !folder,
            &mut group,
        );
        let own = radio(
            texts::MEDIA_FOLDER,
            "install.media.folder",
            folder,
            &mut group,
        );
        let choose = button(UiString::ChooseFolder.text(), "install.media.choose");
        let shown = named(
            text(
                &path
                    .as_ref()
                    .map_or_else(String::new, |p| p.display().to_string()),
            ),
            "install.media.path",
        );
        let verdict = named(text(""), "install.media.verdict");
        let picker = col(6);
        picker.set_margin_start(28);
        picker.set_visible(folder);
        append(
            &picker,
            &[
                choose.upcast_ref(),
                shown.upcast_ref(),
                verdict.upcast_ref(),
            ],
        );
        let read = match &*self.media.borrow() {
            Some(f) => {
                let class = match (f.complete, f.found) {
                    (true, _) => "success",
                    (_, 1..) => "warning",
                    _ => "error",
                };
                verdict.set_label(&f.verdict);
                verdict.set_css_classes(&[class]);
                false
            }
            None => {
                if path.is_some() {
                    verdict.set_label("Checking the folder…");
                }
                path.is_some()
            }
        };
        if let (true, Some(dir)) = (read, path) {
            self.read_folder(dir);
        }
        let (me, p) = (self.clone(), picker.clone());
        own.connect_toggled(move |b| {
            me.flow.borrow_mut().set_use_folder(b.is_active());
            p.set_visible(b.is_active());
            me.update_nav();
        });
        let me = self.clone();
        choose.connect_clicked(move |_| {
            let (me, parent) = (me.clone(), me.win.clone());
            disks::pick_folder(&parent, "Choose the Folder of Discs", move |dir| {
                me.media.take();
                me.read_folder(dir);
                me.go();
            });
        });
        append(
            &self.content,
            &[
                download.upcast_ref(),
                note(texts::MEDIA_DOWNLOAD).upcast_ref(),
                own.upcast_ref(),
                picker.upcast_ref(),
            ],
        );
    }

    /// Read what a folder holds, then show the verdict.
    fn read_folder(self: &Rc<Self>, dir: PathBuf) {
        let (release, profile) = {
            let f = self.flow.borrow();
            (f.release().to_string(), f.profile().to_string())
        };
        // Until the read ends the folder counts as incomplete, so Continue waits.
        let unread = MediaFolder {
            found: 0,
            total: 0,
            missing: Vec::new(),
            verdict: String::new(),
            complete: false,
        };
        self.flow.borrow_mut().set_folder(dir.clone(), &unread);
        let chosen = dir.clone();
        self.work(
            move || install_media_folder(&dir, &release, &profile).map_err(|e| e.to_string()),
            move |me, r| {
                // Another folder was chosen while this one was read.
                if me.flow.borrow().folder() != Some(&chosen) {
                    return;
                }
                match r {
                    Ok(f) => {
                        me.flow.borrow_mut().set_folder(chosen, &f);
                        *me.media.borrow_mut() = Some(f);
                        if me.step() == InstallStep::Media {
                            me.go();
                        }
                    }
                    Err(e) => me.fail(&e),
                }
            },
        );
    }

    fn software_page(self: &Rc<Self>) {
        let profile = self.flow.borrow().profile().to_string();
        self.call(
            move |core, key| core.install_addon_choices(key, &profile),
            |me, choices| {
                if me.step() != InstallStep::Software {
                    return;
                }
                me.flow.borrow_mut().set_choices(choices);
                me.content.append(&text("Install these after IRIX?"));
                let (addons, extras) = {
                    let f = me.flow.borrow();
                    (f.addons().to_vec(), f.extras().to_vec())
                };
                for (c, on) in addons {
                    let b = check(&c.label, &format!("install.addon.{}", c.name), on);
                    b.set_sensitive(!c.locked);
                    let (m2, name) = (me.clone(), c.name.clone());
                    b.connect_toggled(move |b| m2.flow.borrow_mut().tick(&name, b.is_active()));
                    me.content.append(&b);
                    if let Some(n) = &c.note {
                        me.content.append(&note(n));
                    }
                }
                for (i, p) in extras.iter().enumerate() {
                    let r = row();
                    let label = text(&format!(
                        "{} ({})",
                        p.source.clone().unwrap_or_default(),
                        p.install.join(", ")
                    ));
                    label.set_hexpand(true);
                    let remove = gtk::Button::with_label(UiString::Remove.text());
                    let m2 = me.clone();
                    remove.connect_clicked(move |_| {
                        m2.flow.borrow_mut().remove_extra(i);
                        m2.go();
                    });
                    append(&r, &[label.upcast_ref(), remove.upcast_ref()]);
                    me.content.append(&r);
                }
                let add = button(UiString::AddAPackage2.text(), "install.addon.add");
                let m2 = me.clone();
                add.connect_clicked(move |_| m2.add_package());
                me.content.append(&add);
            },
        );
    }

    fn add_package(self: &Rc<Self>) {
        let dialog = named(
            adw::Dialog::builder()
                .title(UiString::AddAPackage.text())
                .content_width(440)
                .build(),
            "install.package.dialog",
        );
        let source = named(
            adw::EntryRow::builder()
                .title("File or HTTPS address")
                .build(),
            "install.package.source",
        );
        let pick = gtk::Button::builder()
            .label(UiString::ChooseFile.text())
            .valign(gtk::Align::Center)
            .build();
        source.add_suffix(&pick);
        let list = named(gtk::TextView::new(), "install.package.list");
        let frame = gtk::ScrolledWindow::builder()
            .child(&list)
            .min_content_height(110)
            .build();
        frame.add_css_class("card");
        let add = named(
            gtk::Button::builder()
                .label(UiString::Add.text())
                .css_classes(["suggested-action"])
                .sensitive(false)
                .build(),
            "install.package.add",
        );
        let selections = {
            let list = list.clone();
            move || {
                let b = list.buffer();
                b.text(&b.start_iter(), &b.end_iter(), false).to_string()
            }
        };
        let ready = {
            let (source, add, selections) = (source.clone(), add.clone(), selections.clone());
            Rc::new(move || {
                add.set_sensitive(
                    !source.text().trim().is_empty() && !selections().trim().is_empty(),
                )
            })
        };
        let r = ready.clone();
        source.connect_changed(move |_| r());
        list.buffer().connect_changed(move |_| ready());
        let (me, s) = (self.clone(), source.clone());
        pick.connect_clicked(move |_| {
            let s = s.clone();
            disks::pick_path(&me.win, "Choose a package", move |p| {
                s.set_text(&p.display().to_string())
            });
        });
        let (me, s, d) = (self.clone(), source.clone(), dialog.clone());
        add.connect_clicked(move |_| {
            me.flow
                .borrow_mut()
                .add_extra(s.text().as_str(), &selections());
            d.close();
            me.go();
        });
        let page = col(12);
        margins(&page, 12, 12, 12, 12);
        let group = adw::PreferencesGroup::new();
        group.add(&source);
        append(
            &page,
            &[
                group.upcast_ref(),
                text("Packages to install, one per line").upcast_ref(),
                frame.upcast_ref(),
            ],
        );
        let (view, header) = (adw::ToolbarView::new(), adw::HeaderBar::new());
        header.pack_end(&add);
        view.add_top_bar(&header);
        view.set_content(Some(&page));
        dialog.set_child(Some(&view));
        dialog.present(Some(&self.win));
    }

    // ---- step 4: the disk ---------------------------------------------------

    fn disk_page(self: &Rc<Self>) {
        self.call(
            |core, key| core.install_disk(key),
            |me, disk| {
                if me.step() != InstallStep::Disk {
                    return;
                }
                match disk {
                    Some(d) => {
                        me.content.append(&text(&d.text));
                        me.flow.borrow_mut().set_disk(DiskState::Present(d));
                        let erase = check(
                            UiString::EraseThisDiskAndInstall.text(),
                            "install.disk.erase",
                            me.flow.borrow().erase(),
                        );
                        let m2 = me.clone();
                        erase.connect_toggled(move |b| {
                            m2.flow.borrow_mut().set_erase(b.is_active());
                            m2.update_nav();
                        });
                        me.content.append(&erase);
                    }
                    None => {
                        me.flow.borrow_mut().set_disk(DiskState::None);
                        me.content.append(&text("This machine has no disk."));
                        let size = named(
                            adw::SpinRow::with_range(1.0, 2048.0, 1.0),
                            "install.disk.size",
                        );
                        size.set_title("Create a disk");
                        size.set_subtitle("Size in GB");
                        size.set_value(f64::from(me.flow.borrow().new_disk_gb()));
                        let m2 = me.clone();
                        size.connect_value_notify(move |r| {
                            m2.flow.borrow_mut().set_new_disk_gb(r.value() as u32)
                        });
                        let group = adw::PreferencesGroup::new();
                        group.add(&size);
                        me.content.append(&group);
                    }
                }
                me.update_nav();
            },
        );
    }

    fn create_disk(self: &Rc<Self>) {
        let mib = u64::from(self.flow.borrow().new_disk_gb()) * 1024;
        let machine = self.m.machine.clone();
        self.call_busy(
            move |_, _| machine.apply_change(&Change::CreateDisk { size_mib: mib }),
            |me, _| {
                me.refresh_machine();
                me.flow.borrow_mut().next();
                me.go();
            },
        );
    }

    // ---- step 5: check ------------------------------------------------------

    fn check_page(self: &Rc<Self>) {
        let passed = self.flow.borrow().check() == CheckState::Passed;
        self.content.append(&text(texts::CHECK_INTRO));
        let status = named(
            text(if passed { texts::PASSED } else { "Ready." }),
            "install.check.status",
        );
        let infoline = named(text(""), "install.check.info");
        infoline.set_visible(false);
        let now = button(UiString::CheckNow.text(), "install.check.now");
        let thorough = check(
            UiString::CheckThoroughlyAlsoReadsThe.text(),
            "install.check.full",
            false,
        );
        let bar = gtk::ProgressBar::builder().visible(false).build();
        let stop = button(UiString::Cancel.text(), "install.check.cancel");
        stop.set_visible(false);
        append(
            &self.content,
            &[
                thorough.upcast_ref(),
                now.upcast_ref(),
                status.upcast_ref(),
                bar.upcast_ref(),
                stop.upcast_ref(),
                infoline.upcast_ref(),
            ],
        );
        let me = self.clone();
        stop.connect_clicked(move |_| {
            if let Some(op) = me.check_op.borrow().as_ref() {
                op.cancel();
            }
        });
        let me = self.clone();
        now.connect_clicked(move |b| {
            let (setup, full) = (me.flow.borrow().setup(), thorough.is_active());
            let prepare = !me.set_up.get();
            me.flow.borrow_mut().set_check(CheckState::Running);
            b.set_sensitive(false);
            status.set_label(texts::CHECKING);
            bar.set_visible(true);
            stop.set_visible(true);
            me.update_nav();
            let (b, status, bar, stop, infoline) = (
                b.clone(),
                status.clone(),
                bar.clone(),
                stop.clone(),
                infoline.clone(),
            );
            me.call(
                move |core, key| {
                    let report = if prepare {
                        core.install_prepare(key, &setup)?
                    } else {
                        Default::default()
                    };
                    Ok((report, core.install_check(key, full)?))
                },
                move |me, (report, op)| {
                    let lines: Vec<&str> = report
                        .lines
                        .iter()
                        .filter(|l| matches!(l.kind, Kind::Warning | Kind::Advice))
                        .map(|l| l.text.as_str())
                        .collect();
                    infoline.set_label(&lines.join("\n"));
                    infoline.set_visible(!lines.is_empty());
                    me.set_up.set(true);
                    me.flow.borrow_mut().set_prepared();
                    me.refresh_machine();
                    *me.check_op.borrow_mut() = Some(op.clone());
                    let weak = Rc::downgrade(me);
                    glib::timeout_add_local(Duration::from_millis(200), move || {
                        let state = op.state();
                        if !state.is_finished() {
                            bar.pulse();
                            return glib::ControlFlow::Continue;
                        }
                        b.set_sensitive(true);
                        bar.set_visible(false);
                        stop.set_visible(false);
                        let (result, words) = match state {
                            OperationState::Succeeded => {
                                (CheckState::Passed, texts::PASSED.to_string())
                            }
                            OperationState::Failed(e) => (CheckState::Failed, e.to_string()),
                            _ => (CheckState::Ready, "Ready.".to_string()),
                        };
                        status.set_label(&words);
                        if result == CheckState::Failed {
                            b.set_label(UiString::CheckAgain.text());
                        }
                        if let Some(me) = weak.upgrade() {
                            me.flow.borrow_mut().set_check(result);
                            me.update_nav();
                        }
                        glib::ControlFlow::Break
                    });
                },
            );
        });
    }

    // ---- steps 6 to 9: the install ------------------------------------------

    /// Entering the first install step: read the guide, start the media server, then the machine.
    fn enter_prepare(self: &Rc<Self>) {
        self.content.append(&text("Starting the install server…"));
        self.call(
            |core, key| Ok((core.install_guide(key)?, core.install_serve(key))),
            |me, (guide, serve)| {
                *me.guide.borrow_mut() = Some(guide);
                me.set_server(serve.map_err(|e| e.to_string()));
                me.start_machine();
                me.go();
            },
        );
    }

    fn set_server(&self, serve: Result<Operation, String>) {
        *self.server.borrow_mut() = match serve {
            Ok(op) => Server::Running(op),
            Err(e) => Server::Failed(e),
        };
    }

    fn start_machine(&self) {
        self.seen_up.set(false);
        if !self.m.state.get().is_up() {
            app::start(&self.app, &self.m);
        }
    }

    fn restart_server(self: &Rc<Self>) {
        let (core, key) = (self.app.core.clone(), self.key());
        self.work(
            move || core.install_serve(&key).map_err(|e| e.to_string()),
            |me, r| {
                me.set_server(r);
                me.tick();
            },
        );
    }

    /// Take the automated install's outcome once its operation ends.
    fn poll_apply(&self) {
        let op = self.apply_op.borrow().clone();
        let Some(op) = op.filter(|op| op.state().is_finished()) else {
            return;
        };
        self.apply_op.take();
        let state = match op.state() {
            OperationState::Succeeded => ApplyState::Done,
            OperationState::Failed(e) => {
                *self.apply_msg.borrow_mut() = e.to_string();
                ApplyState::Failed(op.install_failure().unwrap_or(InstallFailure::Setup))
            }
            _ => ApplyState::Failed(op.install_failure().unwrap_or(InstallFailure::Cancelled)),
        };
        self.flow.borrow_mut().set_apply(state);
    }

    fn apply_page(self: &Rc<Self>) {
        let install = button(UiString::Install.text(), "install.apply");
        install.add_css_class("suggested-action");
        let hint = named(text(""), "install.apply.hint");
        let bar = gtk::ProgressBar::new();
        let line = named(text(""), "install.apply.progress");
        let stop = button(UiString::CancelInstall.text(), "install.apply.cancel");
        let failure = named(text(""), "install.apply.error");
        failure.add_css_class("error");
        let fixed = button(UiString::IFixedItAtInst.text(), "install.apply.fixed");
        let console = button(UiString::OpenConsole.text(), "install.open-console");
        append(
            &self.content,
            &[
                hint.upcast_ref(),
                install.upcast_ref(),
                bar.upcast_ref(),
                line.upcast_ref(),
                stop.upcast_ref(),
                failure.upcast_ref(),
                fixed.upcast_ref(),
                console.upcast_ref(),
            ],
        );
        let (app, m) = (self.app.clone(), self.m.clone());
        console.connect_clicked(move |_| crate::console::present(&app, &m));
        let me = self.clone();
        stop.connect_clicked(move |_| {
            if let Some(op) = me.apply_op.borrow().as_ref() {
                op.cancel();
            }
        });
        let me = self.clone();
        fixed.connect_clicked(move |_| {
            me.flow.borrow_mut().fixed();
            me.go();
        });
        let me = self.clone();
        install.connect_clicked(move |_| {
            me.flow.borrow_mut().set_apply(ApplyState::Running);
            let (core, key) = (me.app.core.clone(), me.key());
            me.work(
                move || core.install_apply_from(&key).map_err(|e| e.to_string()),
                |me, r| match r {
                    Ok(op) => *me.apply_op.borrow_mut() = Some(op),
                    Err(e) => {
                        *me.apply_msg.borrow_mut() = e;
                        let failed = ApplyState::Failed(InstallFailure::Setup);
                        me.flow.borrow_mut().set_apply(failed);
                    }
                },
            );
        });
        let (weak, gen) = (Rc::downgrade(self), self.page.get());
        let render = move || {
            let Some(me) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if me.closing.get() || me.page.get() != gen {
                return glib::ControlFlow::Break;
            }
            me.poll_apply();
            let up = me.machine_up();
            let (state, hint_text, can_install, can_fixed) = {
                let f = me.flow.borrow();
                (f.apply(), f.apply_hint(), f.can_install(up), f.can_fixed())
            };
            hint.set_label(&hint_text);
            install.set_sensitive(can_install);
            fixed.set_visible(can_fixed);
            let progress = me.apply_op.borrow().as_ref().map(|op| op.progress());
            match progress.as_deref().and_then(last_percent) {
                Some(p) => bar.set_fraction(f64::from(p) / 100.0),
                None => bar.pulse(),
            }
            line.set_label(progress.as_deref().unwrap_or(""));
            for w in [
                bar.upcast_ref::<gtk::Widget>(),
                line.upcast_ref(),
                stop.upcast_ref(),
            ] {
                w.set_visible(state == ApplyState::Running);
            }
            failure.set_label(&me.apply_msg.borrow());
            failure.set_visible(matches!(state, ApplyState::Failed(_)));
            me.update_nav();
            glib::ControlFlow::Continue
        };
        render();
        glib::timeout_add_local(Duration::from_millis(300), render);
    }

    fn guide_page(self: &Rc<Self>, step: InstallStep) {
        let Some(guide) = self.guide.borrow().clone() else {
            return;
        };
        let (mut steps, tail) = match step {
            InstallStep::Prepare => (guide.prepare, None),
            InstallStep::Installer => (guide.installer, None),
            InstallStep::Packages => (guide.packages, Some(guide.package_note)),
            _ => (guide.finish, None),
        };
        if step == InstallStep::Packages {
            if self.flow.borrow().by_hand() {
                steps.clear();
            }
            steps.extend(guide.restart);
        }
        if step != InstallStep::Finish {
            let r = row();
            let show = gtk::Button::with_label(UiString::ShowMachine.text());
            let console = named(
                gtk::Button::with_label(UiString::OpenConsole.text()),
                "install.open-console",
            );
            let (app, m) = (self.app.clone(), self.m.clone());
            let (app2, m2) = (app.clone(), m.clone());
            show.connect_clicked(move |_| window::select(&app2, &m2));
            console.connect_clicked(move |_| crate::console::present(&app, &m));
            append(&r, &[show.upcast_ref(), console.upcast_ref()]);
            self.content.append(&r);
        }
        self.content.append(&self.checklist(step as usize, &steps));
        if let Some(t) = tail {
            self.content.append(&note(&t));
        }
    }

    fn checklist(self: &Rc<Self>, section: usize, steps: &[GuideStep]) -> gtk::Box {
        let list = col(10);
        for (i, st) in steps.iter().enumerate() {
            let r = row();
            let tick = check(
                "",
                &format!("install.tick.{section}.{i}"),
                self.ticks.borrow().contains(&(section, i)),
            );
            tick.set_valign(gtk::Align::Start);
            let me = self.clone();
            tick.connect_toggled(move |b| {
                let mut t = me.ticks.borrow_mut();
                if b.is_active() {
                    t.insert((section, i));
                } else {
                    t.remove(&(section, i));
                }
            });
            let body = col(2);
            body.set_hexpand(true);
            body.append(&text(&st.text));
            if let Some(c) = &st.command {
                body.append(
                    &gtk::Label::builder()
                        .label(c)
                        .selectable(true)
                        .xalign(0.0)
                        .wrap(true)
                        .css_classes(["monospace"])
                        .build(),
                );
            }
            append(&r, &[tick.upcast_ref(), body.upcast_ref()]);
            let side = |label: &str, kind: &str| {
                let b = named(
                    gtk::Button::with_label(label),
                    &format!("install.{kind}.{section}.{i}"),
                );
                b.set_valign(gtk::Align::Start);
                r.append(&b);
                b
            };
            if let Some(c) = st.command.clone() {
                side("Copy", "copy").connect_clicked(move |_| {
                    if let Some(d) = gtk::gdk::Display::default() {
                        d.clipboard().set_text(&c);
                    }
                });
            }
            if let (true, Some(send)) = (self.serial, st.send.clone()) {
                let type_it = side("Type it", "type");
                let me = self.clone();
                type_it.connect_clicked(move |_| {
                    if let Err(e) = me.m.machine.console_send(send.as_bytes()) {
                        me.fail(&e.to_string());
                    }
                });
                self.type_buttons.borrow_mut().push(type_it);
            }
            list.append(&r);
        }
        list
    }

    /// The half-second look at the machine and the media server.
    fn tick(self: &Rc<Self>) {
        let up = matches!(
            self.m.machine.view().state,
            MachineState::Starting | MachineState::Running | MachineState::Paused
        );
        self.seen_up.set(self.seen_up.get() | up);
        for b in self.type_buttons.borrow().iter() {
            b.set_visible(up);
        }
        let ended = match &*self.server.borrow() {
            Server::Running(op) => Some(op.state()),
            _ => None,
        };
        let next = match ended {
            Some(OperationState::Failed(e)) => Some(Server::Failed(e.to_string())),
            Some(OperationState::Succeeded) => {
                Some(Server::Failed("The media server stopped.".into()))
            }
            Some(OperationState::Cancelled) => Some(Server::Off),
            _ => None,
        };
        if let Some(s) = next {
            *self.server.borrow_mut() = s;
        }
        let (line, failed) = match &*self.server.borrow() {
            Server::Running(op) => (op.progress(), false),
            Server::Failed(e) => (e.clone(), true),
            Server::Off => (self.plan_line.borrow().clone().unwrap_or_default(), false),
        };
        self.footer.set_label(&line);
        self.footer
            .set_css_classes(if failed { &["error"] } else { &[] });
        self.restart.set_visible(failed);
        let step = self.step();
        self.banner.set_revealed(
            (InstallStep::Prepare..InstallStep::Finish).contains(&step)
                && self.seen_up.get()
                && !self.machine_up(),
        );
        self.update_nav();
    }

    /// Put the network back as it was, then run `then`; the core's advice is shown.
    fn restore_network(self: &Rc<Self>, then: impl FnOnce(&Rc<Self>) + 'static) {
        self.call_busy(
            |core, key| core.install_restore_network(key),
            |me, report| {
                me.refresh_machine();
                let advice: Vec<&str> = report
                    .lines
                    .iter()
                    .filter(|l| matches!(l.kind, Kind::Warning | Kind::Advice))
                    .map(|l| l.text.as_str())
                    .collect();
                if !advice.is_empty() {
                    window::toast(&me.app, &advice.join(" "));
                }
                then(me);
            },
        );
    }

    fn finish(self: &Rc<Self>) {
        self.restore_network(|me| {
            me.dismiss();
            window::toast(&me.app, texts::FINISHED);
        });
    }

    // ---- closing ------------------------------------------------------------

    /// Close the window, ending the server.
    fn dismiss(self: &Rc<Self>) {
        self.closing.set(true);
        if let Some(op) = self.check_op.take() {
            op.cancel();
        }
        self.stop_server();
        OPEN.with(|o| o.borrow_mut().retain(|a| !Rc::ptr_eq(a, self)));
        self.win.destroy();
    }

    fn ask(self: &Rc<Self>, q: CloseQuestion, then: impl Fn(&Rc<Self>, CloseAction) + 'static) {
        let me = self.clone();
        glib::spawn_future_local(async move {
            let buttons: Vec<(&str, &str)> = q
                .buttons
                .iter()
                .map(|(label, a)| (action_id(*a), label.as_str()))
                .collect();
            let id = question(&me.win, &q.heading, &q.text, &buttons, q.default).await;
            if let Some((_, a)) = q.buttons.iter().find(|(_, a)| action_id(*a) == id) {
                then(&me, *a);
            }
        });
    }

    /// Cancel, window close and tab close: the machine question, then the network one.
    fn request_close(self: &Rc<Self>) {
        if self.closing.get() {
            return;
        }
        let q = self.flow.borrow().close_machine_question(self.machine_up());
        match q {
            Some(q) => self.ask(q, |me, a| match a {
                CloseAction::StopMachine => {
                    me.stop_server();
                    app::stop(&me.app, &me.m, StopKind::Force);
                    me.network_question(false);
                }
                CloseAction::LeaveRunning => {
                    me.stop_server();
                    me.network_question(true);
                }
                _ => {}
            }),
            None => {
                self.stop_server();
                self.network_question(self.machine_up());
            }
        }
    }

    /// Offer to put the network back when the install left it private, then close. After
    /// Stop Machine it waits for the machine to be down first.
    fn network_question(self: &Rc<Self>, up: bool) {
        let machine = self.m.machine.clone();
        self.work(
            move || {
                for _ in 0..200 {
                    let state = machine.view().state;
                    if up || matches!(state, MachineState::NotRunning | MachineState::Exited) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                machine
                    .settings()
                    .map(|s| s.network.mode == "private")
                    .unwrap_or(false)
            },
            move |me, private| {
                let q = me
                    .flow
                    .borrow()
                    .close_network_question(up && me.machine_up(), private);
                match q {
                    Some(q) => me.ask(q, |me, a| match a {
                        CloseAction::SetBack => me.restore_network(|me| me.dismiss()),
                        _ => me.dismiss(),
                    }),
                    None => me.dismiss(),
                }
            },
        );
    }
}
