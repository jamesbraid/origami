//! The main window: library sidebar, tabs, header bar, menus and the actions
//! behind every command. A tab dragged out of the bar becomes a window of its
//! own (without the library), which is how a machine pops out.

use crate::app::{self, App};
use crate::commands::{self, Command, Group};
use crate::library::Entry;
use crate::machine::{MachineUi, State};
use crate::prefs::PointerMode;
use crate::{input, settings};
use adw::prelude::*;
use gtk::{gio, glib};
use origami_core::facade::StopKind;
use origami_engine::Control;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub struct Row {
    pub dir: PathBuf,
    pub row: gtk::ListBoxRow,
    pub pill: gtk::Label,
}

pub struct Win {
    pub win: adw::ApplicationWindow,
    pub tv: adw::TabView,
    pub split: adw::OverlaySplitView,
    pub title: adw::WindowTitle,
    pub pill: gtk::Label,
    pub power_btn: gtk::Button,
    pub pointer_btn: gtk::MenuButton,
    pub stack: gtk::Stack,
    pub list: gtk::ListBox,
    pub rows: RefCell<Vec<Row>>,
    pub has_library: bool,
}

fn the_win(app: &App, w: &adw::ApplicationWindow) -> Option<Rc<Win>> {
    app.windows.borrow().iter().find(|x| &x.win == w).cloned()
}

/// The window the user is working in.
pub fn active(app: &App) -> Option<Rc<Win>> {
    let a = app
        .app
        .active_window()
        .and_then(|w| w.downcast::<adw::ApplicationWindow>().ok());
    a.and_then(|w| the_win(app, &w))
        .or_else(|| app.windows.borrow().first().cloned())
}

/// The machine shown in a window's selected tab.
pub fn current(app: &App, w: &Win) -> Option<Rc<MachineUi>> {
    let child = w.tv.selected_page()?.child();
    app.machines
        .borrow()
        .iter()
        .find(|m| m.root.upcast_ref::<gtk::Widget>() == &child)
        .cloned()
}

fn menu_model(commands: &[Command]) -> gio::Menu {
    let menu = gio::Menu::new();
    for (title, group) in [
        ("Machine", Group::Machine),
        ("View", Group::View),
        ("Window", Group::Window),
    ] {
        let sub = gio::Menu::new();
        for c in commands.iter().filter(|c| {
            c.group == group && !c.id.starts_with("machine.pointer") && c.id != "machine.new"
        }) {
            sub.append(Some(&c.title), Some(&commands::action(&c.id)));
            if c.id == "machine.capture" {
                let p = gio::Menu::new();
                for id in ["machine.pointer-follow", "machine.pointer-capture"] {
                    let c = commands
                        .iter()
                        .find(|c| c.id == id)
                        .expect("a pointer command");
                    p.append(Some(&c.title), Some(&commands::action(id)));
                }
                sub.append_section(None, &p);
            }
        }
        menu.append_submenu(Some(title), &sub);
    }
    let app = gio::Menu::new();
    for c in commands
        .iter()
        .filter(|c| c.group == Group::App || c.id == "machine.new")
    {
        app.append(Some(&c.title), Some(&commands::action(&c.id)));
    }
    menu.append_section(None, &app);
    menu
}

pub fn build(app: &Rc<App>, with_library: bool) -> Rc<Win> {
    let tv = adw::TabView::new();
    let bar = adw::TabBar::builder().view(&tv).autohide(true).build();
    let empty = adw::StatusPage::builder()
        .icon_name("computer-symbolic")
        .title("No machine open")
        .description("Choose a machine from the library, or create one.")
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&tv, Some("tabs"));
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name("empty");

    let title = adw::WindowTitle::new("Origami", "");
    let pill = gtk::Label::builder()
        .css_classes(["pill", "pill-grey"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    title_box.append(&title);
    title_box.append(&pill);

    let power_btn = gtk::Button::builder()
        .label("Start")
        .action_name("win.machine.start")
        .css_classes(["suggested-action"])
        .build();
    let pointer_btn = gtk::MenuButton::builder()
        .icon_name("input-mouse-symbolic")
        .tooltip_text("Pointer mode")
        .build();
    let pm = gio::Menu::new();
    pm.append(Some("Follow My Mouse"), Some("win.machine.pointer-follow"));
    pm.append(
        Some("Capture on Click"),
        Some("win.machine.pointer-capture"),
    );
    pointer_btn.set_menu_model(Some(&pm));
    let console_btn = gtk::Button::builder()
        .icon_name("utilities-terminal-symbolic")
        .tooltip_text("Console")
        .action_name("win.window.console")
        .build();
    let settings_btn = gtk::Button::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text("Settings")
        .action_name("win.machine.settings")
        .build();
    let menu_btn = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu_model(&app.commands))
        .primary(true)
        .tooltip_text("Main menu")
        .build();
    let header = adw::HeaderBar::new();
    let sidebar_toggle = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Show library")
        .build();
    if with_library {
        header.pack_start(&sidebar_toggle);
    }
    header.pack_start(&power_btn);
    header.set_title_widget(Some(&title_box));
    header.pack_end(&menu_btn);
    header.pack_end(&settings_btn);
    header.pack_end(&console_btn);
    header.pack_end(&pointer_btn);

    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    content.add_top_bar(&bar);
    content.set_content(Some(&stack));

    // Library sidebar.
    let list = gtk::ListBox::builder()
        .css_classes(["navigation-sidebar"])
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    list.update_property(&[gtk::accessible::Property::Label("Machines")]);
    let split = adw::OverlaySplitView::builder()
        .sidebar_width_fraction(0.22)
        .min_sidebar_width(230.0)
        .build();
    if with_library {
        let side_head = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .build();
        side_head.set_title_widget(Some(&adw::WindowTitle::new("Machines", "")));
        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("New Machine")
            .action_name("win.machine.new")
            .build();
        side_head.pack_end(&add);
        let side = adw::ToolbarView::new();
        side.add_top_bar(&side_head);
        side.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .child(&list)
                .vexpand(true)
                .build(),
        ));
        split.set_sidebar(Some(&side));
        split.set_content(Some(&content));
        sidebar_toggle.set_active(true);
        sidebar_toggle
            .bind_property("active", &split, "show-sidebar")
            .bidirectional()
            .sync_create()
            .build();
    } else {
        split.set_content(Some(&content));
        split.set_collapsed(true);
    }

    let win = adw::ApplicationWindow::builder()
        .application(&app.app)
        .default_width(1400)
        .default_height(900)
        .title("Origami")
        .content(&split)
        .build();
    let w = Rc::new(Win {
        win: win.clone(),
        tv: tv.clone(),
        split,
        title,
        pill,
        power_btn,
        pointer_btn,
        stack,
        list: list.clone(),
        rows: RefCell::new(Vec::new()),
        has_library: with_library,
    });
    register_actions(app, &w);
    wire_tabs(app, &w);
    {
        let (app, weak) = (app.clone(), Rc::downgrade(&w));
        list.connect_row_activated(move |_, row| {
            let Some(w) = weak.upgrade() else { return };
            let dir = w
                .rows
                .borrow()
                .iter()
                .find(|r| &r.row == row)
                .map(|r| r.dir.clone());
            if let Some(entry) =
                dir.and_then(|d| app.library.borrow().iter().find(|e| e.dir == d).cloned())
            {
                let _ = app::open(&app, &entry);
            }
        });
    }
    {
        let (app2, weak) = (app.clone(), Rc::downgrade(&w));
        win.connect_close_request(move |_| {
            let Some(w) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            // Closing the library window ends the app, and asks about machines
            // still running; closing a pop-out closes its tabs the way a tab's
            // close button does.
            if w.has_library {
                if app::running_machines(&app2).is_empty() {
                    return glib::Propagation::Proceed;
                }
                app::request_quit(&app2);
                return glib::Propagation::Stop;
            }
            close_popout(&app2, &w)
        });
        let app = app.clone();
        win.connect_is_active_notify(move |_| refresh(&app));
    }
    app.windows.borrow_mut().push(w.clone());
    {
        let (app, weak) = (app.clone(), Rc::downgrade(&w));
        win.connect_destroy(move |_| {
            if let Some(w) = weak.upgrade() {
                app.windows.borrow_mut().retain(|x| !Rc::ptr_eq(x, &w));
            }
        });
    }
    sync_accels(app);
    refresh(app);
    w
}

fn wire_tabs(app: &Rc<App>, w: &Rc<Win>) {
    let tv = &w.tv;
    {
        // Dragging a tab out of the bar asks for a window to put it in.
        let app = app.clone();
        tv.connect_create_window(move |_| {
            let nw = build(&app, false);
            nw.win.present();
            Some(nw.tv.clone())
        });
    }
    {
        let (app, weak) = (app.clone(), Rc::downgrade(w));
        tv.connect_close_page(move |tv, page| {
            let Some(w) = weak.upgrade() else { return glib::Propagation::Proceed };
            let child = page.child();
            let m = app.machines.borrow().iter().find(|m| m.root.upcast_ref::<gtk::Widget>() == &child).cloned();
            let Some(m) = m else { return glib::Propagation::Proceed };
            if !m.state.get().is_up() || app.cfg.automation.is_some() {
                forget(&app, &m);
                tv.close_page_finish(page, true);
                return glib::Propagation::Stop;
            }
            let dialog = adw::AlertDialog::new(
                Some(&format!("Close {}?", m.name)),
                Some("The machine is running. Closing the tab can leave it running in the background."),
            );
            dialog.add_responses(&[("cancel", "Cancel"), ("keep", "Keep Running"), ("stop", "Power Off")]);
            dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("keep"));
            dialog.set_close_response("cancel");
            let (tv, page, app) = (tv.clone(), page.clone(), app.clone());
            glib::spawn_future_local(async move {
                match dialog.choose_future(Some(&w.win)).await.as_str() {
                    "keep" => {
                        forget(&app, &m);
                        tv.close_page_finish(&page, true);
                    }
                    "stop" => {
                        app::stop(&app, &m, StopKind::Clean);
                        forget(&app, &m);
                        tv.close_page_finish(&page, true);
                    }
                    _ => tv.close_page_finish(&page, false),
                }
            });
            glib::Propagation::Stop
        });
    }
    {
        let (app, weak) = (app.clone(), Rc::downgrade(w));
        let app_attached = app.clone();
        tv.connect_page_detached(move |tv, _, _| {
            if let Some(w) = weak.upgrade() {
                if tv.n_pages() == 0 {
                    if w.has_library {
                        w.stack.set_visible_child_name("empty");
                    } else {
                        w.win.close();
                    }
                }
            }
            refresh(&app);
        });
        let (app, weak) = (app_attached, Rc::downgrade(w));
        tv.connect_page_attached(move |_, page, _| {
            if let Some(w) = weak.upgrade() {
                w.stack.set_visible_child_name("tabs");
                // Landing in a new window re-realizes the picture; the next
                // frame goes to the new surface.
                let child = page.child();
                if let Some(m) = app
                    .machines
                    .borrow()
                    .iter()
                    .find(|m| m.root.upcast_ref::<gtk::Widget>() == &child)
                {
                    *m.page.borrow_mut() = Some(page.clone());
                }
            }
            refresh(&app);
        });
    }
    {
        let (app, weak) = (app.clone(), Rc::downgrade(w));
        tv.connect_selected_page_notify(move |_| {
            if let Some(w) = weak.upgrade() {
                if let Some(m) = current(&app, &w) {
                    if matches!(m.state.get(), State::Running | State::NoPicture) {
                        m.screen.grab_focus();
                    }
                }
            }
            refresh(&app);
        });
    }
}

/// Drop a machine's tab state; the machine itself keeps running.
fn forget(app: &Rc<App>, m: &Rc<MachineUi>) {
    input::release_all(app, m);
    if let Some(id) = m.hook.take() {
        m.machine.remove_frame_hook(id);
    }
    app.machines.borrow_mut().retain(|x| !Rc::ptr_eq(x, m));
}

/// A closed window stays alive while the app holds its `Win`, and the destroy
/// signal only comes when the last reference goes, so take it off the list
/// when it closes.
fn drop_window(app: &App, w: &Rc<Win>) {
    app.windows.borrow_mut().retain(|x| !Rc::ptr_eq(x, w));
}

/// The machines whose tabs are in `w`.
fn machines_in(app: &App, w: &Win) -> Vec<Rc<MachineUi>> {
    let pages = w.tv.pages();
    (0..pages.n_items())
        .filter_map(|i| pages.item(i).and_downcast::<adw::TabPage>())
        .filter_map(|p| {
            app.machines
                .borrow()
                .iter()
                .find(|m| m.root.upcast_ref::<gtk::Widget>() == &p.child())
                .cloned()
        })
        .collect()
}

/// A pop-out window is closing with tabs in it: ask about running machines
/// as a tab's close button does, then forget them so none is left without a
/// window to show it.
fn close_popout(app: &Rc<App>, w: &Rc<Win>) -> glib::Propagation {
    let ms = machines_in(app, w);
    let running: Vec<_> = ms
        .iter()
        .filter(|m| m.state.get().is_up())
        .cloned()
        .collect();
    if running.is_empty() || app.cfg.automation.is_some() {
        ms.iter().for_each(|m| forget(app, m));
        drop_window(app, w);
        return glib::Propagation::Proceed;
    }
    let names: Vec<_> = running.iter().map(|m| m.name.clone()).collect();
    let dialog = adw::AlertDialog::new(
        Some("Close this window?"),
        Some(&format!(
            "Still running: {}. Closing can leave them running in the background.",
            names.join(", ")
        )),
    );
    dialog.add_responses(&[
        ("cancel", "Cancel"),
        ("keep", "Keep Running"),
        ("stop", "Power Off"),
    ]);
    dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("keep"));
    dialog.set_close_response("cancel");
    let (app, w) = (app.clone(), w.clone());
    glib::spawn_future_local(async move {
        let choice = dialog.choose_future(Some(&w.win)).await;
        if choice == "cancel" {
            return;
        }
        for m in &ms {
            if choice == "stop" && m.state.get().is_up() {
                app::stop(&app, m, StopKind::Clean);
            }
            forget(&app, m);
        }
        drop_window(&app, &w);
        w.win.destroy();
    });
    glib::Propagation::Stop
}

/// A machine's still at `pt` points, or the generic glyph tile when it has none.
fn machine_mark(art: Option<&'static str>, pt: i32, icon: i32) -> gtk::Widget {
    if let Some(id) = art {
        let image = crate::art::small_image(id, pt);
        image.set_halign(gtk::Align::Center);
        image.set_valign(gtk::Align::Center);
        return image.upcast();
    }
    let g = gtk::Box::builder()
        .css_classes(["glyph"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .width_request(pt)
        .height_request(pt)
        .build();
    g.append(
        &gtk::Image::builder()
            .icon_name("computer-symbolic")
            .pixel_size(icon)
            .hexpand(true)
            .vexpand(true)
            .build(),
    );
    g.upcast()
}

pub fn add_tab(app: &Rc<App>, m: &Rc<MachineUi>) {
    let Some(w) = active(app) else { return };
    let page = w.tv.append(&m.root);
    page.set_title(&m.name);
    page.set_tooltip(&m.dir.display().to_string());
    if let Some(id) = m.entry.borrow().art {
        crate::art::tab_icon(&page, id);
    }
    *m.page.borrow_mut() = Some(page.clone());
    w.stack.set_visible_child_name("tabs");
    w.tv.set_selected_page(&page);
    rebuild_hero(m);
}

pub fn select(app: &Rc<App>, m: &Rc<MachineUi>) {
    let page = m.page.borrow().clone();
    if let Some(page) = page.as_ref() {
        for w in app.windows.borrow().iter() {
            if w.tv.page_position(page) >= 0 && w.tv.pages().n_items() > 0 {
                let child = page.child();
                if child.ancestor(adw::TabView::static_type()).as_ref()
                    == Some(w.tv.upcast_ref::<gtk::Widget>())
                {
                    w.tv.set_selected_page(page);
                    w.win.present();
                }
            }
        }
    }
}

// ---- actions ----------------------------------------------------------------

fn confirm(
    app: &Rc<App>,
    w: &Rc<Win>,
    heading: &str,
    body: &str,
    yes: &str,
    then: impl Fn() + 'static,
) {
    if app.cfg.automation.is_some() {
        then();
        return;
    }
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "Cancel"), ("yes", yes)]);
    dialog.set_response_appearance("yes", adw::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");
    let win = w.win.clone();
    glib::spawn_future_local(async move {
        if dialog.choose_future(Some(&win)).await == "yes" {
            then();
        }
    });
}

fn register_actions(app: &Rc<App>, w: &Rc<Win>) {
    for c in &app.commands {
        let act = gio::SimpleAction::new(&c.id, None);
        let (app, weak, id) = (app.clone(), Rc::downgrade(w), c.id.clone());
        act.connect_activate(move |_, _| {
            if let Some(w) = weak.upgrade() {
                run(&app, &w, &id);
            }
        });
        w.win.add_action(&act);
    }
}

/// Run a command by its identifier. The menus, shortcuts, buttons and the
/// automation socket all end up here.
pub fn run(app: &Rc<App>, w: &Rc<Win>, id: &str) {
    let m = current(app, w);
    match (id, m) {
        ("machine.start", Some(m)) => app::start(app, &m),
        ("machine.start", None) => {
            if let Some(m) = selected_entry(app, w).and_then(|e| app::open(app, &e)) {
                app::start(app, &m);
            }
        }
        ("machine.stop", Some(m)) => {
            let (a, mm) = (app.clone(), m.clone());
            if m.state.get() == State::Running {
                confirm(
                    app,
                    w,
                    &format!("Power off {}?", m.name),
                    "To the guest this is like pulling the power cord. IRIX should be shut down first.",
                    "Power Off Anyway",
                    move || app::stop(&a, &mm, StopKind::Clean),
                );
            } else {
                app::stop(app, &m, StopKind::Clean);
            }
        }
        ("machine.force-quit", Some(m)) => {
            let (a, mm) = (app.clone(), m.clone());
            confirm(
                app,
                w,
                &format!("Force quit {}'s emulator?", m.name),
                "Changes not yet written to disk can be lost.",
                "Force Quit",
                move || app::stop(&a, &mm, StopKind::Force),
            );
        }
        ("machine.reset", Some(m)) => {
            let (a, mm) = (app.clone(), m.clone());
            confirm(
                app,
                w,
                &format!("Reset {}?", m.name),
                "The guest restarts as if its reset button was pressed.",
                "Reset",
                move || app::control(&a, &mm, Control::Reset),
            );
        }
        ("machine.pause", Some(m)) => app::control(app, &m, Control::Pause),
        ("machine.resume", Some(m)) => app::control(app, &m, Control::Resume),
        ("machine.release-keys", Some(m)) => input::release_all(app, &m),
        ("machine.screenshot", Some(m)) => screenshot(&m),
        ("machine.capture", Some(m)) => {
            m.pointer.set(PointerMode::Capture);
            let on = !m.captured.get();
            if on {
                m.screen.grab_focus();
            }
            input::set_captured(app, &m, on);
        }
        ("machine.pointer-follow", Some(m)) => set_pointer(app, &m, PointerMode::Follow),
        ("machine.pointer-capture", Some(m)) => set_pointer(app, &m, PointerMode::Capture),
        ("machine.settings", Some(m)) => settings::present_machine(app, &w.win, &m),
        ("machine.settings", None) => {
            if let Some(m) = selected_entry(app, w).and_then(|e| app::open(app, &e)) {
                settings::present_machine(app, &w.win, &m);
            }
        }
        ("window.console", Some(m)) => crate::console::present(app, &m),
        ("view.show-library", _) => w.split.set_show_sidebar(!w.split.shows_sidebar()),
        ("view.full-screen", _) => w.win.set_fullscreened(!w.win.is_fullscreen()),
        ("window.close-tab", Some(_)) => {
            if let Some(p) = w.tv.selected_page() {
                w.tv.close_page(&p);
            }
        }
        ("window.move-to-new-window", Some(m)) => {
            // Not in the `if let`: the move re-enters and sets `m.page`.
            let page = m.page.borrow().clone();
            if let Some(page) = page {
                let nw = build(app, false);
                w.tv.transfer_page(&page, &nw.tv, 0);
                nw.win.present();
            }
        }
        ("machine.new", _) => settings::new_machine(app, &w.win),
        ("app.preferences", _) => settings::preferences(app, &w.win),
        ("app.about", _) => about(&w.win),
        ("app.quit", _) => app::request_quit(app),
        _ => {}
    }
}

/// Save the picture on screen as a PNG in the user's Pictures folder.
fn screenshot(m: &MachineUi) {
    let dir = dirs::picture_dir().unwrap_or_else(|| PathBuf::from("."));
    let stamp = glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("%Y%m%d-%H%M%S").ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let path = dir.join(format!("{} {stamp}.png", m.name));
    let saved = m
        .picture
        .paintable()
        .and_then(|p| p.downcast::<crate::crisp::Crisp>().ok())
        .and_then(|c| c.texture())
        .map(|t| t.save_to_png(&path));
    match saved {
        Some(Ok(())) => {}
        Some(Err(e)) => *m.error.borrow_mut() = Some(e.to_string()),
        None => *m.error.borrow_mut() = Some("there is no picture to save".into()),
    }
}

fn set_pointer(app: &Rc<App>, m: &Rc<MachineUi>, mode: PointerMode) {
    input::release_all(app, m);
    m.pointer.set(mode);
    app.prefs.borrow_mut().set_pointer_for(&m.dir, Some(mode));
    rebuild_hero(m);
    refresh(app);
}

fn selected_entry(app: &App, w: &Win) -> Option<Entry> {
    let row = w.list.selected_row()?;
    let dir = w
        .rows
        .borrow()
        .iter()
        .find(|r| r.row == row)
        .map(|r| r.dir.clone())?;
    app.library.borrow().iter().find(|e| e.dir == dir).cloned()
}

/// Whether a command can run right now, for the actions, menus and tests.
pub fn enabled(app: &App, w: &Win, id: &str) -> bool {
    let m = current(app, w);
    let st = m.as_ref().map(|m| m.state.get());
    let up = st.is_some_and(State::is_up);
    let running = matches!(st, Some(State::Running | State::NoPicture));
    let selected = m.is_some() || w.list.selected_row().is_some();
    // Not built yet: the Diagnostics window, CD changes and the shortcuts sheet.
    if matches!(
        id,
        "window.diagnostics" | "machine.eject-cd" | "app.shortcuts"
    ) {
        return false;
    }
    // The core's commands say when they apply, with the library's selection
    // standing in for a tab that is not open yet, and a start in progress
    // still allowed to be forced down.
    if let Some(when) = app
        .commands
        .iter()
        .find(|c| c.id == id)
        .and_then(|c| c.when)
    {
        let core_state = m.as_ref().map(|m| m.machine.view().state);
        return match id {
            "machine.start" if m.is_none() => selected,
            "machine.start" => when.holds(core_state) || st == Some(State::LeftRunning),
            "machine.settings" => selected,
            "machine.force-quit" => up,
            _ => when.holds(core_state),
        };
    }
    match id {
        "machine.capture" => running,
        "machine.pointer-follow" | "machine.pointer-capture" => m.is_some(),
        "window.close-tab" | "window.move-to-new-window" => m.is_some(),
        _ => true,
    }
}

/// Which accelerators are live. While input is captured the guest owns every
/// key, so none are.
pub fn sync_accels(app: &Rc<App>) {
    let captured = app.machines.borrow().iter().any(|m| m.captured.get());
    for c in &app.commands {
        let accels: Vec<&str> = if captured && c.id != "machine.capture" {
            vec![]
        } else {
            c.accels.iter().map(String::as_str).collect()
        };
        app.app
            .set_accels_for_action(&commands::action(&c.id), &accels);
    }
}

/// Bring every visible thing in line with the machines' states.
pub fn refresh(app: &Rc<App>) {
    let machines: Vec<_> = app.machines.borrow().clone();
    for m in machines {
        if m.hero_state.get() != Some(m.state.get()) {
            rebuild_hero(&m);
        }
    }
    for w in app.windows.borrow().iter() {
        let m = current(app, w);
        match &m {
            Some(m) => {
                let (word, class) = m.state.get().pill();
                w.title.set_title(&m.name);
                w.title.set_subtitle(&m.entry.borrow().model);
                for c in ["pill-grey", "pill-green", "pill-amber", "pill-red"] {
                    w.pill.remove_css_class(c);
                }
                w.pill.add_css_class(class);
                w.pill.set_label(word);
                w.pill.set_visible(true);
                w.pointer_btn
                    .set_tooltip_text(Some(&format!("Pointer: {}", m.pointer.get().label())));
                let up = m.state.get().is_up();
                let (label, action) = match m.state.get() {
                    State::Starting | State::Stopping => ("Force Quit", "win.machine.force-quit"),
                    _ if up => ("Power Off", "win.machine.stop"),
                    _ => ("Start", "win.machine.start"),
                };
                w.power_btn.set_label(label);
                w.power_btn.set_action_name(Some(action));
                if up {
                    w.power_btn.remove_css_class("suggested-action");
                } else {
                    w.power_btn.add_css_class("suggested-action");
                }
                w.win.set_title(Some(&format!("{} - Origami", m.name)));
            }
            None => {
                w.title.set_title("Origami");
                w.title.set_subtitle("");
                w.pill.set_visible(false);
                w.power_btn.set_label("Start");
                w.power_btn.set_action_name(Some("win.machine.start"));
                w.win.set_title(Some("Origami"));
            }
        }
        for c in &app.commands {
            if let Some(a) = w
                .win
                .lookup_action(&c.id)
                .and_then(|a| a.downcast::<gio::SimpleAction>().ok())
            {
                a.set_enabled(enabled(app, w, &c.id));
            }
        }
        for r in w.rows.borrow().iter() {
            let state = app
                .machines
                .borrow()
                .iter()
                .find(|m| m.dir == r.dir)
                .map(|m| m.state.get())
                .unwrap_or(State::Stopped);
            let (word, class) = state.pill();
            for c in ["pill-grey", "pill-green", "pill-amber", "pill-red"] {
                r.pill.remove_css_class(c);
            }
            r.pill.add_css_class(class);
            r.pill.set_label(word);
        }
    }
}

// ---- library ----------------------------------------------------------------

pub fn rebuild_library(app: &Rc<App>) {
    for w in app.windows.borrow().iter().filter(|w| w.has_library) {
        while let Some(c) = w.list.first_child() {
            w.list.remove(&c);
        }
        w.rows.borrow_mut().clear();
        let entries = app.library.borrow().clone();
        if entries.is_empty() {
            let page = adw::StatusPage::builder()
                .icon_name("computer-symbolic")
                .title("No machines yet")
                .description("Create one with New Machine.")
                .build();
            w.stack.add_named(&page, Some("library-empty"));
            if w.tv.n_pages() == 0 {
                w.stack.set_visible_child_name("library-empty");
            }
        }
        for e in entries {
            let g = machine_mark(e.art, 34, 18);
            let title = gtk::Label::builder()
                .label(&e.name)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            let pill = gtk::Label::builder()
                .label("Stopped")
                .css_classes(["pill", "pill-grey"])
                .halign(gtk::Align::Start)
                .build();
            let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
            text.append(&title);
            text.append(&pill);
            let row_box = gtk::Box::builder()
                .spacing(10)
                .margin_top(6)
                .margin_bottom(6)
                .margin_start(6)
                .margin_end(6)
                .build();
            row_box.append(&g);
            row_box.append(&text);
            let row = gtk::ListBoxRow::builder().child(&row_box).build();
            row.update_property(&[gtk::accessible::Property::Label(&e.name)]);
            if let Some(err) = &e.error {
                row.set_tooltip_text(Some(err));
            }
            w.list.append(&row);
            w.rows.borrow_mut().push(Row {
                dir: e.dir.clone(),
                row,
                pill,
            });
        }
    }
    refresh(app);
}

/// Rebuild a machine's hero card: its state, what it is made of, and what to
/// do next.
pub fn rebuild_hero(m: &Rc<MachineUi>) {
    m.hero_state.set(Some(m.state.get()));
    while let Some(c) = m.hero.first_child() {
        m.hero.remove(&c);
    }
    m.hero.set_margin_top(24);
    m.hero.set_margin_bottom(24);
    m.hero.set_margin_start(24);
    m.hero.set_margin_end(24);
    let e = m.entry.borrow().clone();
    let state = m.state.get();
    let (word, class) = state.pill();

    let g: gtk::Widget = match e.art.and_then(|id| crate::art::framed(id, "hero")) {
        Some(hero) => hero.upcast(),
        None => machine_mark(None, 72, 36),
    };
    g.set_valign(gtk::Align::Center);
    let name = gtk::Label::builder()
        .label(&e.name)
        .css_classes(["title-1"])
        .xalign(0.0)
        .build();
    let pills = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    pills.append(
        &gtk::Label::builder()
            .label(word)
            .css_classes(["pill", class])
            .build(),
    );
    if !e.model.is_empty() {
        pills.append(
            &gtk::Label::builder()
                .label(&e.model)
                .css_classes(["pill", "pill-grey"])
                .build(),
        );
    }
    pills.append(
        &gtk::Label::builder()
            .label(m.pointer.get().label())
            .css_classes(["pill", "pill-grey"])
            .build(),
    );
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let primary = match state {
        State::LeftRunning => ("Reconnect", "win.machine.start"),
        State::Stopping => ("Saving…", "win.machine.force-quit"),
        _ => ("Start", "win.machine.start"),
    };
    buttons.append(
        &gtk::Button::builder()
            .label(primary.0)
            .action_name(primary.1)
            .css_classes(["suggested-action", "pill"])
            .sensitive(state != State::Stopping)
            .build(),
    );
    buttons.append(
        &gtk::Button::builder()
            .label("Settings")
            .action_name("win.machine.settings")
            .build(),
    );
    let text = gtk::Box::new(gtk::Orientation::Vertical, 10);
    text.set_hexpand(true);
    text.append(&name);
    text.append(&pills);
    if state == State::Stopping {
        let bar = gtk::ProgressBar::builder().pulse_step(0.1).build();
        let b = bar.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
            if b.root().is_none() {
                return glib::ControlFlow::Break;
            }
            b.pulse();
            glib::ControlFlow::Continue
        });
        text.append(&bar);
    }
    if let Some(err) = m.error.borrow().as_ref().or(e.error.as_ref()) {
        text.append(
            &gtk::Label::builder()
                .label(err)
                .wrap(true)
                .xalign(0.0)
                .css_classes(["dim-label"])
                .selectable(true)
                .build(),
        );
    }
    text.append(&buttons);
    let card = gtk::Box::builder()
        .spacing(20)
        .css_classes(["hero"])
        .build();
    if let Some(id) = e.art {
        card.add_css_class(&format!("wash-{id}"));
    }
    card.append(&g);
    card.append(&text);
    m.hero.append(&card);

    m.hero.append(
        &gtk::Label::builder()
            .label("Hardware")
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .column_spacing(12)
        .row_spacing(12)
        .max_children_per_line(4)
        .homogeneous(true)
        .build();
    let s = &e.summary;
    let mut tiles: Vec<(String, String)> = vec![
        (
            if s.processor.is_empty() {
                "Processor".into()
            } else {
                s.processor.to_uppercase()
            },
            format!("{} CPU{}", s.cpus, if s.cpus == 1 { "" } else { "s" }),
        ),
        (s.memory.clone(), "memory per node".into()),
    ];
    if let Some(g) = &s.graphics {
        tiles.push((g.clone(), "graphics".into()));
    }
    tiles.push((
        format!("{} drive{}", s.drives, if s.drives == 1 { "" } else { "s" }),
        "storage".into(),
    ));
    tiles.push((
        s.network.clone(),
        format!(
            "{} port forward{}",
            s.forwards,
            if s.forwards == 1 { "" } else { "s" }
        ),
    ));
    if s.shares > 0 {
        tiles.push((
            format!(
                "{} folder{}",
                s.shares,
                if s.shares == 1 { "" } else { "s" }
            ),
            "shared".into(),
        ));
    }
    for (t, sub) in tiles {
        let b = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .css_classes(["tile"])
            .build();
        b.append(
            &gtk::Label::builder()
                .label(&t)
                .xalign(0.0)
                .css_classes(["tile-title"])
                .build(),
        );
        b.append(
            &gtk::Label::builder()
                .label(&sub)
                .xalign(0.0)
                .css_classes(["dim-label"])
                .build(),
        );
        flow.append(&b);
    }
    m.hero.append(&flow);
}

/// The About window: the cube on granite, the version and the photographs'
/// credits. Without a GPU for the cube it is the stock dialog.
fn about(parent: &adw::ApplicationWindow) {
    let version = env!("CARGO_PKG_VERSION");
    let dark = adw::StyleManager::default().is_dark();
    let Some(cube) = crate::cube::Cube::new(160, crate::cube::about_style(dark)) else {
        adw::AboutDialog::builder()
            .application_name("Origami")
            .application_icon("computer-symbolic")
            .developer_name("Origami")
            .version(version)
            .comments("SGI workstations and servers on your desktop")
            .license_type(gtk::License::Bsd3)
            .build()
            .present(Some(parent));
        return;
    };
    let label = |text: &str, css: &[&str]| {
        gtk::Label::builder()
            .label(text)
            .css_classes(css.to_vec())
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build()
    };
    let credits: String = origami_render::scenes::SCENES
        .iter()
        .map(|s| format!("{}: {}", s.title, s.credit))
        .collect::<Vec<_>>()
        .join("\n");
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    page.append(&cube.widget);
    page.append(&label("Origami", &["title-1"]));
    page.append(&label(version, &["dim-label"]));
    page.append(&label("SGI workstations and servers on your desktop", &[]));
    page.append(&label(&credits, &["caption", "dim-label"]));
    page.append(&label(
        "All trademarks are the property of their respective owners.",
        &["caption", "dim-label"],
    ));
    // What the stock dialog's licence row showed: BSD 3-Clause, with its text.
    let licence = gtk::Expander::builder()
        .label("Licensed under the BSD 3-Clause License. No warranty.")
        .child(
            &gtk::ScrolledWindow::builder()
                .min_content_height(160)
                .child(&label(
                    include_str!("../../../LICENSE"),
                    &["caption", "monospace"],
                ))
                .build(),
        )
        .build();
    page.append(&licence);
    // The scene follows the appearance while the window is open.
    let style = adw::StyleManager::default();
    let cube = Rc::new(cube);
    let handler = RefCell::new(Some(style.connect_dark_notify({
        let cube = cube.clone();
        move |s| cube.set_style(crate::cube::about_style(s.is_dark()))
    })));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title("About Origami")
        .content_width(420)
        .child(&toolbar)
        .build();
    dialog.connect_closed(move |_| {
        if let Some(id) = handler.take() {
            adw::StyleManager::default().disconnect(id);
        }
    });
    dialog.present(Some(parent));
}
