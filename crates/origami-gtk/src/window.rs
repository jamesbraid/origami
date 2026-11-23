//! The main window: library sidebar, tabs, header bar, menus and the actions
//! behind every command. A tab dragged out of the bar becomes a window of its
//! own (without the library), which is how a machine pops out.

use crate::app::{self, App};
use crate::commands::{self, Command, Group};
use crate::library::Entry;
use crate::machine::{MachineUi, State};
use crate::prefs::PointerMode;
use crate::view_scale::ViewScale;
use crate::{input, settings};
use adw::prelude::*;
use gtk::{gio, glib};
use origami_core::facade::StopKind;
use origami_core::facade::UiString;
use origami_engine::Control;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub struct Row {
    pub dir: PathBuf,
    pub row: gtk::ListBoxRow,
    pub pill: gtk::Label,
    pub badge: gtk::Label,
}

pub struct Win {
    pub win: adw::ApplicationWindow,
    pub tv: adw::TabView,
    pub bar: adw::TabBar,
    toasts: adw::ToastOverlay,
    pub split: adw::OverlaySplitView,
    pub title: adw::WindowTitle,
    pub pill: gtk::Label,
    pub power_btn: gtk::Button,
    pub pause_btn: gtk::Button,
    /// Every header button, for the automation's tooltip check.
    pub toolbar: Vec<gtk::Widget>,
    pub cpu_btn: gtk::ToggleButton,
    pub stack: gtk::Stack,
    pub list: gtk::ListBox,
    pub rows: RefCell<Vec<Row>>,
    /// The library row's context menu, and the machine it was opened on.
    pub row_menu: gtk::PopoverMenu,
    row_dir: RefCell<Option<PathBuf>>,
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

/// 1500x1000, or the first monitor's size less a margin when that is smaller.
fn default_size() -> (i32, i32) {
    let (mut w, mut h) = (1500, 1000);
    let mon = gtk::gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_then(|m| m.downcast::<gtk::gdk::Monitor>().ok());
    if let Some(g) = mon.map(|m| m.geometry()) {
        w = w.min(g.width() - 80);
        h = h.min(g.height() - 120);
    }
    (w.max(640), h.max(480))
}

/// What the window shows with no machine open: the cube turning slowly in the
/// studio, over the window with no tile of its own, and nothing to click.
fn empty_view() -> gtk::Widget {
    const TITLE: &str = "No machine open";
    const BODY: &str = "Choose a machine from the library, or create one.";
    let Some(cube) = crate::cube::Cube::new(220, 220, crate::cube::starting_style(0x7b7b84)) else {
        return adw::StatusPage::builder()
            .icon_name("computer-symbolic")
            .title(TITLE)
            .description(BODY)
            .build()
            .upcast();
    };
    cube.widget.set_can_target(false);
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    page.append(&cube.widget);
    page.append(
        &gtk::Label::builder()
            .label(TITLE)
            .css_classes(["title-2"])
            .build(),
    );
    page.append(
        &gtk::Label::builder()
            .label(BODY)
            .css_classes(["dim-label"])
            .build(),
    );
    page.upcast()
}

pub fn build(app: &Rc<App>, with_library: bool) -> Rc<Win> {
    let tv = adw::TabView::new();
    let bar = adw::TabBar::builder()
        .view(&tv)
        .autohide(false)
        .expand_tabs(false)
        .build();
    let empty = empty_view();
    let stack = gtk::Stack::new();
    stack.add_named(&tv, Some("tabs"));
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name("empty");

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));

    let title = adw::WindowTitle::new(UiString::Origami.text(), "");
    let pill = gtk::Label::builder()
        .css_classes(["pill", "pill-grey"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    title_box.append(&title);
    title_box.append(&pill);

    let power_btn = gtk::Button::builder()
        .label(UiString::Start.text())
        .tooltip_text("Start the machine (Ctrl+Shift+R)")
        .action_name("win.machine.start")
        .css_classes(["suggested-action"])
        .build();
    let pause_btn = gtk::Button::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text("Pause or resume the machine (Ctrl+P)")
        .action_name("win.machine.pause")
        .build();
    let cpu_btn = gtk::ToggleButton::builder()
        .label(UiString::Cpu.text())
        .tooltip_text(UiString::ShowOrHideTheEmulated.text())
        .build();
    let pointer_btn = gtk::MenuButton::builder()
        .icon_name("input-mouse-symbolic")
        .tooltip_text("Pointer mode: follow my mouse or capture on click")
        .build();
    let pm = gio::Menu::new();
    pm.append(
        Some(UiString::FollowMyMouse.text()),
        Some("win.machine.pointer-follow"),
    );
    pm.append(
        Some(UiString::CaptureOnClick.text()),
        Some("win.machine.pointer-capture"),
    );
    pointer_btn.set_menu_model(Some(&pm));
    let console_btn = gtk::Button::builder()
        .icon_name("utilities-terminal-symbolic")
        .tooltip_text("Open the machine's serial console (Ctrl+Shift+O)")
        .action_name("win.window.console")
        .build();
    let settings_btn = gtk::Button::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text("Open the machine's settings (Ctrl+Shift+I)")
        .action_name("win.machine.settings")
        .build();
    let menu_btn = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu_model(&app.commands))
        .primary(true)
        .tooltip_text(UiString::OpenTheMainMenu.text())
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
    header.pack_start(&pause_btn);
    header.set_title_widget(Some(&title_box));
    header.pack_end(&menu_btn);
    header.pack_end(&settings_btn);
    header.pack_end(&console_btn);
    header.pack_end(&pointer_btn);
    header.pack_end(&cpu_btn);

    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    content.add_top_bar(&bar);
    content.set_content(Some(&toasts));

    // Library sidebar.
    let list = gtk::ListBox::builder()
        .css_classes(["navigation-sidebar"])
        .selection_mode(gtk::SelectionMode::Single)
        // A click selects; opening takes a double-click or Enter.
        .activate_on_single_click(false)
        .build();
    list.update_property(&[gtk::accessible::Property::Label("Machines")]);
    // The row menu's host: a GtkListBox disposes by removing every child as a
    // row and loops forever on a popover, so the popover hangs off a box.
    let list_host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list_host.append(&list);
    let split = adw::OverlaySplitView::builder()
        .sidebar_width_fraction(0.22)
        .min_sidebar_width(230.0)
        .build();
    if with_library {
        let side_head = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .build();
        let heading = adw::WindowTitle::new("Machines", "");
        heading.set_widget_name("sidebar.heading");
        side_head.set_title_widget(Some(&heading));
        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(UiString::NewMachine.text())
            .action_name("win.machine.new")
            .build();
        side_head.pack_end(&add);
        let side = adw::ToolbarView::new();
        side.add_top_bar(&side_head);
        side.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .child(&list_host)
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
        .default_width(default_size().0)
        .default_height(default_size().1)
        .title(UiString::Origami.text())
        .content(&split)
        .build();
    let menu = gio::Menu::new();
    menu.append(Some(UiString::OpenInNewTab.text()), Some("row.open"));
    menu.append(Some("Show in Files"), Some("row.show"));
    menu.append(Some(UiString::Diagnostics.text()), Some("row.diagnostics"));
    let row_menu = gtk::PopoverMenu::from_model(Some(&menu));
    row_menu.set_parent(&list_host);
    row_menu.set_has_arrow(false);
    row_menu.set_halign(gtk::Align::Start);
    let toolbar: Vec<gtk::Widget> = [
        power_btn.upcast_ref::<gtk::Widget>(),
        pause_btn.upcast_ref(),
        cpu_btn.upcast_ref(),
        pointer_btn.upcast_ref(),
        console_btn.upcast_ref(),
        settings_btn.upcast_ref(),
        menu_btn.upcast_ref(),
    ]
    .into_iter()
    .cloned()
    .collect();
    let w = Rc::new(Win {
        win: win.clone(),
        tv: tv.clone(),
        bar,
        toasts,
        split,
        title,
        pill,
        power_btn,
        pause_btn,
        toolbar,
        cpu_btn,
        stack,
        list: list.clone(),
        rows: RefCell::new(Vec::new()),
        row_menu,
        row_dir: RefCell::new(None),
        has_library: with_library,
    });
    register_actions(app, &w);
    wire_tabs(app, &w);
    wire_cpu_panel(app, &w);
    wire_row_menu(app, &w);
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

/// Reveal a file or folder in the file manager (over the portal when sandboxed).
pub(crate) fn show_in_files(win: &adw::ApplicationWindow, path: PathBuf) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
    let win = win.clone();
    glib::spawn_future_local(async move {
        if let Err(e) = launcher.open_containing_folder_future(Some(&win)).await {
            eprintln!("origami-gtk: show {} in Files: {e}", path.display());
        }
    });
}

/// Right-click on a library row: select it and offer its two commands.
fn wire_row_menu(app: &Rc<App>, w: &Rc<Win>) {
    let actions = gio::SimpleActionGroup::new();
    let open = gio::SimpleAction::new("open", None);
    {
        let (app, weak) = (app.clone(), Rc::downgrade(w));
        open.connect_activate(move |_, _| {
            let dir = weak.upgrade().and_then(|w| w.row_dir.borrow().clone());
            let entry = dir.and_then(|d| app.library.borrow().iter().find(|e| e.dir == d).cloned());
            if let Some(entry) = entry {
                let _ = app::open(&app, &entry);
            }
        });
    }
    let show = gio::SimpleAction::new("show", None);
    {
        let weak = Rc::downgrade(w);
        show.connect_activate(move |_, _| {
            let Some(w) = weak.upgrade() else { return };
            let Some(dir) = w.row_dir.borrow().clone() else {
                return;
            };
            show_in_files(&w.win, dir.join("machine.toml"));
        });
    }
    let diag = gio::SimpleAction::new("diagnostics", None);
    {
        let (app, weak) = (app.clone(), Rc::downgrade(w));
        diag.connect_activate(move |_, _| {
            let dir = weak.upgrade().and_then(|w| w.row_dir.borrow().clone());
            let entry = dir.and_then(|d| app.library.borrow().iter().find(|e| e.dir == d).cloned());
            if let Some(m) = entry.and_then(|e| app::open(&app, &e)) {
                crate::diagnostics::present(&app, &m);
            }
        });
    }
    actions.add_action(&open);
    actions.add_action(&show);
    actions.add_action(&diag);
    {
        // Hand the keyboard back to the row, so the menu key works again.
        let weak = Rc::downgrade(w);
        w.row_menu.connect_closed(move |_| {
            if let Some(row) = weak.upgrade().and_then(|w| w.list.selected_row()) {
                row.grab_focus();
            }
        });
    }
    w.row_menu.insert_action_group("row", Some(&actions));
    let click = gtk::GestureClick::builder().button(3).build();
    let weak = Rc::downgrade(w);
    click.connect_pressed(move |_, _, x, y| {
        let Some(w) = weak.upgrade() else { return };
        let Some(row) = w.list.row_at_y(y as i32) else {
            return;
        };
        popup_row_menu(&w, &row, x as i32, y as i32);
    });
    w.list.add_controller(click);
    // The Menu key and Shift+F10 open it on the selected row.
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(w);
    keys.connect_key_pressed(move |_, key, _, state| {
        let menu = key == gtk::gdk::Key::Menu
            || (key == gtk::gdk::Key::F10 && state.contains(gtk::gdk::ModifierType::SHIFT_MASK));
        let Some(w) = weak.upgrade().filter(|_| menu) else {
            return glib::Propagation::Proceed;
        };
        let Some(row) = w.list.selected_row() else {
            return glib::Propagation::Proceed;
        };
        let at = row.compute_bounds(&w.list).map_or((0, 0), |b| {
            (b.x() as i32 + 24, (b.y() + b.height() / 2.0) as i32)
        });
        popup_row_menu(&w, &row, at.0, at.1);
        glib::Propagation::Stop
    });
    w.list.add_controller(keys);
}

/// Select a library row and show its menu at `x`, `y` in the list.
fn popup_row_menu(w: &Win, row: &gtk::ListBoxRow, x: i32, y: i32) {
    let dir = w
        .rows
        .borrow()
        .iter()
        .find(|r| &r.row == row)
        .map(|r| r.dir.clone());
    w.list.select_row(Some(row));
    *w.row_dir.borrow_mut() = dir;
    w.row_menu
        .set_pointing_to(Some(&gtk::gdk::Rectangle::new(x, y, 1, 1)));
    w.row_menu.popup();
}

/// The CPU button shows or hides the selected machine's panel; the load is
/// read twice a second only while it is shown.
fn wire_cpu_panel(app: &Rc<App>, w: &Rc<Win>) {
    let (app, weak) = (app.clone(), Rc::downgrade(w));
    w.cpu_btn.connect_toggled(move |_| {
        if let Some(w) = weak.upgrade() {
            sync_cpu_panel(&app, &w);
        }
    });
}

/// Show the panel on the selected tab's machine when the button is down, and
/// hide every other one; start polling for it.
pub fn sync_cpu_panel(app: &Rc<App>, w: &Rc<Win>) {
    let on = w.cpu_btn.is_active();
    let current = current(app, w);
    for m in machines_in(app, w) {
        let show = on && current.as_ref().is_some_and(|c| Rc::ptr_eq(c, &m));
        if m.cpu.root.is_visible() != show {
            m.cpu.root.set_visible(show);
            if show {
                m.cpu.show(m.machine.osview());
                let (m2, btn) = (m.clone(), w.cpu_btn.clone());
                crate::cpu_panel::poll(move || btn.is_active() && m2.cpu.root.is_visible(), m);
            }
        }
    }
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
            if crate::install::install_running(Some(m.machine.key())) {
                let (tv, page, app) = (tv.clone(), page.clone(), app.clone());
                glib::spawn_future_local(async move {
                    let quit = crate::install::stop_install(&w.win, true).await;
                    if quit {
                        crate::install::stop_installs(&app, Some(m.machine.key()));
                        forget(&app, &m);
                    }
                    tv.close_page_finish(&page, quit);
                });
                return glib::Propagation::Stop;
            }
            if !m.state.get().is_up() || app.cfg.automation.is_some() {
                forget(&app, &m);
                tv.close_page_finish(page, true);
                return glib::Propagation::Stop;
            }
            let dialog = adw::AlertDialog::new(
                Some(&format!("Close {}?", m.name)),
                Some("The machine is running. Closing the tab can leave it running in the background."),
            );
            dialog.add_responses(&[("cancel", "Cancel"), ("keep", "Keep running"), ("stop", "Power off")]);
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
                sync_cpu_panel(&app, &w);
            }
            refresh(&app);
        });
    }
}

/// Drop a machine's tab state; the machine itself keeps running.
fn forget(app: &Rc<App>, m: &Rc<MachineUi>) {
    if m.state.get().is_up() {
        let tex = m
            .picture
            .paintable()
            .and_then(|p| p.downcast::<crate::crisp::Crisp>().ok())
            .and_then(|c| c.texture());
        if let Some(tex) = tex {
            m.machine.keep_picture(m.run.get(), 0, Box::new(tex));
        }
    }
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
        ("keep", "Keep running"),
        ("stop", "Power off"),
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
                    "Power off anyway",
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
                "Force quit",
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
        ("machine.screenshot", Some(m)) => screenshot(w, &m),
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
        ("machine.install-irix", Some(m)) => crate::install::open(app, &w.win, &m),
        ("window.console", Some(m)) => crate::console::present(app, &m),
        ("window.diagnostics", Some(m)) => crate::diagnostics::present(app, &m),
        ("view.show-library", _) => w.split.set_show_sidebar(!w.split.shows_sidebar()),
        ("view.full-screen", _) => w.win.set_fullscreened(!w.win.is_fullscreen()),
        (id, Some(m)) if ViewScale::from_command(id).is_some() => {
            if let Some(v) = ViewScale::from_command(id) {
                m.resize_window(v);
            }
        }
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
        ("app.about", _) => about(app, &w.win),
        ("app.quit", _) => app::request_quit(app),
        _ => {}
    }
}

/// A short message over the window the user is working in.
pub fn toast(app: &App, text: &str) {
    if let Some(w) = active(app) {
        w.toasts.add_toast(adw::Toast::new(text));
    }
}

/// Save the picture on screen as a PNG in the user's Pictures folder, named
/// "<machine> <yyyy-mm-dd at HH.mm.ss>.png", and offer to show it.
fn screenshot(w: &Rc<Win>, m: &MachineUi) {
    let dir = dirs::picture_dir().unwrap_or_else(|| PathBuf::from("."));
    let stamp = glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("%Y-%m-%d at %H.%M.%S").ok())
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
        Some(Ok(())) => {
            let toast = adw::Toast::builder()
                .title("Screenshot saved")
                .button_label("Show in Files")
                .build();
            let weak = Rc::downgrade(w);
            toast.connect_button_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    show_in_files(&w.win, path.clone());
                }
            });
            w.toasts.add_toast(toast);
        }
        Some(Err(e)) => *m.error.borrow_mut() = Some(e.to_string()),
        None => *m.error.borrow_mut() = Some("there is no picture to save".into()),
    }
}

/// The default pointer mode changed: machines with no setting of their own follow it now.
pub fn default_pointer_changed(app: &Rc<App>) {
    let open: Vec<_> = app.machines.borrow().clone();
    for m in open {
        let mode = app.prefs.borrow().pointer_for(&m.dir);
        if m.pointer.get() != mode {
            input::release_all(app, &m);
            m.pointer.set(mode);
            rebuild_hero(&m);
        }
    }
    refresh(app);
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
    if m.as_ref().is_some_and(|m| m.busy.get())
        && matches!(id, "machine.pause" | "machine.resume" | "machine.reset")
    {
        return false;
    }
    // Not built yet: the Eject CD command and the shortcuts sheet.
    if matches!(id, "machine.eject-cd" | "app.shortcuts") {
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
            "machine.install-irix" => {
                let runs = match &m {
                    Some(m) => m.entry.borrow().runs_irix,
                    None => selected_entry(app, w).is_none_or(|e| e.runs_irix),
                };
                runs && (m.is_none() || when.holds(core_state))
            }
            "machine.force-quit" => up,
            _ => when.holds(core_state),
        };
    }
    match id {
        "machine.capture" => running,
        "machine.pointer-follow" | "machine.pointer-capture" => m.is_some(),
        id if ViewScale::from_command(id).is_some() => m.is_some(),
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

/// Outline the selected tab in the chassis colour: `style.rs` has one rule per still.
fn tint_bar(bar: &adw::TabBar, art: Option<&str>, flat: bool) {
    // The stopped machine's pane runs its fade up under the bar, so no rule.
    if flat {
        bar.add_css_class("flat");
    } else {
        bar.remove_css_class("flat");
    }
    let want = art.map(|id| format!("chassis-{id}"));
    for c in bar.css_classes() {
        if c.starts_with("chassis-") && Some(c.as_str()) != want.as_deref() {
            bar.remove_css_class(&c);
        }
    }
    if let Some(c) = want {
        bar.add_css_class(&c);
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
        tint_bar(
            &w.bar,
            m.as_ref().and_then(|m| m.entry.borrow().art),
            m.as_ref()
                .is_some_and(|m| m.root.visible_child_name().as_deref() == Some("hero")),
        );
        match &m {
            Some(m) => {
                let (word, class) = m.state.get().pill();
                w.title.set_title(&m.name);
                w.title.set_subtitle(&m.entry.borrow().product_label);
                for c in ["pill-grey", "pill-green", "pill-amber", "pill-red"] {
                    w.pill.remove_css_class(c);
                }
                w.pill.add_css_class(class);
                w.pill.set_label(word);
                w.pill.set_visible(true);
                let up = m.state.get().is_up();
                let (label, tip, action) = match m.state.get() {
                    State::Starting | State::Stopping => (
                        "Force quit",
                        "Force quit the emulator",
                        "win.machine.force-quit",
                    ),
                    _ if up => (
                        "Power off",
                        "Power off the machine (Ctrl+Shift+P)",
                        "win.machine.stop",
                    ),
                    _ => (
                        "Start",
                        "Start the machine (Ctrl+Shift+R)",
                        "win.machine.start",
                    ),
                };
                w.power_btn.set_label(label);
                w.power_btn.set_tooltip_text(Some(tip));
                w.power_btn.set_action_name(Some(action));
                if up {
                    w.power_btn.remove_css_class("suggested-action");
                } else {
                    w.power_btn.add_css_class("suggested-action");
                }
                let paused = m.state.get() == State::Paused;
                w.pause_btn.set_icon_name(if paused {
                    "media-playback-start-symbolic"
                } else {
                    "media-playback-pause-symbolic"
                });
                w.pause_btn.set_action_name(Some(if paused {
                    "win.machine.resume"
                } else {
                    "win.machine.pause"
                }));
                w.win.set_title(Some(&format!("{} - Origami", m.name)));
            }
            None => {
                w.title.set_title(UiString::Origami.text());
                w.title.set_subtitle("");
                w.pill.set_visible(false);
                w.power_btn.set_label(UiString::Start.text());
                w.power_btn
                    .set_tooltip_text(Some("Start the machine (Ctrl+Shift+R)"));
                w.power_btn.set_action_name(Some("win.machine.start"));
                w.win.set_title(Some(UiString::Origami.text()));
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
            let old = app
                .library
                .borrow()
                .iter()
                .any(|e| e.dir == r.dir && e.needs_upgrade);
            let (word, class) = if old {
                crate::upgrade::PILL
            } else {
                state.pill()
            };
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
        // Only the rows: the row menu's popover is a child of the list too.
        for r in w.rows.borrow().iter() {
            w.list.remove(&r.row);
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
            let badge = gtk::Label::builder()
                .label(e.system_disk.as_deref().unwrap_or_default())
                .css_classes(["pill", "pill-grey"])
                .visible(e.system_disk.is_some())
                .build();
            badge.set_widget_name("library.badge");
            let states = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            states.append(&pill);
            states.append(&badge);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
            text.append(&title);
            text.append(&states);
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
                badge,
            });
        }
    }
    refresh(app);
}

/// A summary card: a fixed 140 by 104 with the icon in a fixed box, so a row
/// of them lines up whatever the text.
fn summary_card(icon: &str, value: &str, title: &str) -> adw::Clamp {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .valign(gtk::Align::Center)
        .css_classes(["tile"])
        .build();
    b.append(
        &gtk::Image::builder()
            .icon_name(icon)
            .pixel_size(20)
            .height_request(28)
            .css_classes(["dim-label"])
            .build(),
    );
    for (text, css) in [(value, "tile-title"), (title, "caption")] {
        b.append(
            &gtk::Label::builder()
                .label(text)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes([css])
                .build(),
        );
    }
    adw::Clamp::builder()
        .maximum_size(140)
        .tightening_threshold(140)
        .width_request(140)
        .height_request(104)
        .child(&b)
        .build()
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
            .label(origami_core::facade::pointer_status(
                m.pointer.get() == crate::prefs::PointerMode::Follow,
                false,
            ))
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
            .label(UiString::Settings.text())
            .action_name("win.machine.settings")
            .build(),
    );
    let text = gtk::Box::new(gtk::Orientation::Vertical, 10);
    text.set_hexpand(true);
    text.append(&name);
    if !e.product_label.is_empty() {
        text.append(
            &gtk::Label::builder()
                .label(&e.product_label)
                .xalign(0.0)
                .css_classes(["product-line"])
                .build(),
        );
    }
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
    // The tint is the pane's, not the card's: the scrolled window behind it.
    if let Some(sw) = m.hero.ancestor(gtk::ScrolledWindow::static_type()) {
        for c in sw.css_classes() {
            if c.starts_with("fade-") {
                sw.remove_css_class(&c);
            }
        }
        if let Some(id) = e.art {
            sw.add_css_class(&format!("fade-{id}"));
        }
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
        .halign(gtk::Align::Center)
        .build();
    for c in &e.cards {
        let icon = match c.title.as_str() {
            "Memory" => "media-flash-symbolic",
            "Nodes" => "network-workgroup-symbolic",
            _ => "view-app-grid-symbolic",
        };
        flow.append(&summary_card(icon, &c.value, &c.title));
    }
    m.hero.append(&flow);
}

/// The app icon from the binary, so About shows it where the icon theme has none (a run outside an install).
fn app_icon() -> gtk::gdk::Texture {
    const PNG: &[u8] =
        include_bytes!("../../../resources/linux/icons/hicolor/256x256/apps/fans.irix.Origami.png");
    gtk::gdk::Texture::from_bytes(&glib::Bytes::from_static(PNG)).expect("the bundled icon decodes")
}

/// The About window: the cube on a band (an icon where there is no GPU for
/// it), the version, who made it and two buttons for the credits.
fn about(app: &Rc<App>, parent: &adw::ApplicationWindow) {
    let version = option_env!("ORIGAMI_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    let dark = adw::StyleManager::default().is_dark();
    let scene = crate::cube::about_scene();
    let about = origami_credits::credits().ok();
    let copyright = about.as_ref().map_or("", |c| c.copyright.as_str());
    let url = about.as_ref().map_or("", |c| c.project_url.as_str());
    let cube = crate::cube::Cube::new(420, 230, crate::cube::about_style(dark, scene)).map(Rc::new);
    let label = |text: &str, css: &[&str]| {
        gtk::Label::builder()
            .label(text)
            .css_classes(css.to_vec())
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build()
    };
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(14)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    match &cube {
        Some(cube) => cube.enable_pointer(),
        None => page.append(
            &gtk::Image::builder()
                .paintable(&app_icon())
                .pixel_size(96)
                .margin_top(24)
                .build(),
        ),
    }
    page.append(&label("Origami", &["title-1"]));
    page.append(&label(
        &origami_core::facade::version_line(version),
        &["dim-label"],
    ));
    // Typing h, i, n, v slides the host inventory over the credits; Escape or
    // a click puts them back.
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideUp)
        .hhomogeneous(true)
        .vhomogeneous(true)
        .build();
    let who = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .valign(gtk::Align::Start)
        .build();
    who.append(&label(copyright, &[]));
    who.append(&label("BSD 3-Clause licence", &[]));
    let site = gtk::LinkButton::with_label(url, url.trim_start_matches("https://"));
    who.append(&site);
    stack.add_named(&who, Some("credits"));
    let inventory = gtk::Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .yalign(0.0)
        .margin_top(8)
        .margin_start(8)
        .css_classes(["monospace", "caption"])
        .build();
    let shelf = gtk::ScrolledWindow::builder()
        .min_content_height(150)
        .child(&inventory)
        .build();
    let back = gtk::GestureClick::new();
    back.set_propagation_phase(gtk::PropagationPhase::Capture);
    back.connect_pressed({
        let stack = stack.downgrade();
        move |_, _, _, _| {
            if let Some(s) = stack.upgrade() {
                s.set_visible_child_name("credits");
            }
        }
    });
    shelf.add_controller(back);
    stack.add_named(&shelf, Some("hinv"));
    page.append(&stack);
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    let acknowledgements = gtk::Button::with_label(UiString::Acknowledgements.text());
    acknowledgements.connect_clicked(crate::credits::acknowledgements);
    let licences = gtk::Button::with_label(UiString::Licences.text());
    licences.connect_clicked(crate::credits::licences);
    buttons.append(&acknowledgements);
    buttons.append(&licences);
    page.append(&buttons);
    // The scene was picked from the clock at open; only the tones follow the appearance.
    let style = adw::StyleManager::default();
    let handler = RefCell::new(cube.as_ref().map(|cube| {
        let cube = cube.clone();
        style.connect_dark_notify(move |s| {
            cube.set_style(crate::cube::about_style(s.is_dark(), scene))
        })
    }));
    // Shift-click on the cube and the right arrow step through the scenes.
    let keys = gtk::EventControllerKey::new();
    if let Some(cube) = &cube {
        let click = gtk::GestureClick::new();
        click.connect_pressed({
            let step = cube.stepper();
            move |g, _, _, _| {
                if g.current_event_state()
                    .contains(gtk::gdk::ModifierType::SHIFT_MASK)
                {
                    step();
                }
            }
        });
        cube.widget.add_controller(click);
        keys.connect_key_pressed({
            let step = cube.stepper();
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Right {
                    step();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    // The band runs edge to edge; the text sits under it.
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    if let Some(cube) = &cube {
        root.append(&cube.widget);
    }
    root.append(&page);
    toolbar.set_content(Some(&root));
    let dialog = adw::Dialog::builder()
        .title(UiString::AboutOrigami.text())
        .content_width(420)
        .child(&toolbar)
        .build();
    dialog.connect_closed(move |_| {
        if let Some(id) = handler.take() {
            adw::StyleManager::default().disconnect(id);
        }
    });
    dialog.add_controller(keys);
    dialog.add_controller(egg_keys(app, &stack, &inventory));
    dialog.present(Some(parent));
}

/// The letters h, i, n, v in a row show the inventory; Escape hides it
/// before it closes the dialog.
fn egg_keys(app: &Rc<App>, stack: &gtk::Stack, inventory: &gtk::Label) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let seen = RefCell::new(String::new());
    let (app, stack, inventory) = (app.clone(), stack.downgrade(), inventory.downgrade());
    keys.connect_key_pressed(move |_, key, _, state| {
        let (Some(stack), Some(inventory)) = (stack.upgrade(), inventory.upgrade()) else {
            return glib::Propagation::Proceed;
        };
        if key == gtk::gdk::Key::Escape && stack.visible_child_name().as_deref() == Some("hinv") {
            stack.set_visible_child_name("credits");
            return glib::Propagation::Stop;
        }
        let plain = !state
            .intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK);
        if let (true, Some(c)) = (plain, key.to_unicode()) {
            let mut seen = seen.borrow_mut();
            seen.push(c.to_ascii_lowercase());
            let from = seen.len().saturating_sub(4);
            *seen = seen[from..].to_string();
            if *seen == "hinv" {
                show_inventory(&app, &stack, &inventory);
            }
        }
        glib::Propagation::Proceed
    });
    keys
}

/// Read the inventory off the UI thread (it reads the disk) and slide it in.
fn show_inventory(app: &Rc<App>, stack: &gtk::Stack, inventory: &gtk::Label) {
    let (core, gpu) = (app.core.clone(), app.dev.as_ref().map(|d| d.name.clone()));
    let (stack, inventory) = (stack.downgrade(), inventory.downgrade());
    glib::spawn_future_local(async move {
        let text = gio::spawn_blocking(move || {
            let machines = core
                .list_machines()
                .ok()
                .map(|l| l.into_iter().map(|m| (m.name, m.product)).collect());
            let host = origami_core::hinv::gather(&core.root(), machines, gpu);
            origami_core::hinv::format(&host)
        })
        .await
        .unwrap_or_else(|_| "The inventory could not be read.".into());
        if let (Some(stack), Some(inventory)) = (stack.upgrade(), inventory.upgrade()) {
            inventory.set_label(&text);
            stack.set_visible_child_name("hinv");
        }
    });
}
