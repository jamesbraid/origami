//! The main window: library sidebar, tabs, header bar, menus and the actions
//! behind every command. A tab dragged out of the bar becomes a window of its
//! own (without the library), which is how a machine pops out.

use crate::alerts;
use crate::app::{self, App};
use crate::commands::{self, Command, Group};
use crate::library::Entry;
use crate::machine::{MachineUi, State};
use crate::prefs::PointerMode;
use crate::view_scale::ViewScale;
use crate::{input, settings, style};
use adw::prelude::*;
use gtk::{gio, glib};
use origami_core::facade::{
    classify_drop, drop_caption, empty_state, library_line_for, machine_card, show_in_label,
    state_label, status_bar, toast as core_toast, ActionKind, AlertKind, ButtonRole, CoreError,
    DropKind, EmptyKind, ErrorKind, Platform, ShownState, StopKind, ToastKind, Tone, UiString,
};
use origami_core::Change;
use origami_engine::Control;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

pub struct Row {
    pub dir: PathBuf,
    pub row: gtk::ListBoxRow,
    pub name: gtk::Label,
    pub dot: gtk::Box,
    pub word: gtk::Label,
}

pub struct Win {
    pub win: adw::ApplicationWindow,
    pub tv: adw::TabView,
    pub bar: adw::TabBar,
    toasts: adw::ToastOverlay,
    pub split: adw::OverlaySplitView,
    pub title: adw::WindowTitle,
    pub power_btn: gtk::Button,
    pub pause_btn: gtk::Button,
    /// The pointer toggle, and whether the window is setting it itself.
    pub pointer_btn: gtk::ToggleButton,
    pub syncing: Rc<Cell<bool>>,
    /// Every header button, for the automation's tooltip check.
    pub toolbar: Vec<gtk::Widget>,
    /// The CPU load panel is shown on the selected machine.
    pub cpu_on: Cell<bool>,
    /// The bar under a running machine's picture, and its two items.
    pub content: adw::ToolbarView,
    pub size_label: gtk::Label,
    pub pointer_label: gtk::Label,
    /// The sidebar: the library, or the install guide while an install runs.
    pub side: gtk::Stack,
    /// The library row whose card (needs upgrade, can't open) fills the content.
    pub card_for: RefCell<Option<PathBuf>>,
    /// What a drag over the window would do, from the core: Change CD, New machine from this disk.
    pub drop_caption: gtk::Label,
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

/// The main menu: what the header bar has no room for, then the machine, view and
/// window commands, then the app's.
fn menu_model(commands: &[Command]) -> gio::Menu {
    let title = |id: &str| {
        commands
            .iter()
            .find(|c| c.id == id)
            .map_or_else(String::new, |c| c.title.clone())
    };
    let item = |m: &gio::Menu, id: &str| m.append(Some(&title(id)), Some(&commands::action(id)));
    let menu = gio::Menu::new();
    let top = gio::Menu::new();
    for id in [
        "machine.new",
        "view.cpu-load",
        "window.diagnostics",
        "machine.screenshot",
    ] {
        item(&top, id);
    }
    menu.append_section(None, &top);
    let by_group = gio::Menu::new();
    for (name, group) in [
        ("Machine", Group::Machine),
        ("View", Group::View),
        ("Window", Group::Window),
    ] {
        let sub = gio::Menu::new();
        for c in commands.iter().filter(|c| {
            c.group == group
                && !c.id.starts_with("machine.pointer")
                && !matches!(
                    c.id.as_str(),
                    "machine.new"
                        | "view.cpu-load"
                        | "window.diagnostics"
                        | "machine.screenshot"
                        | "view.shortcuts"
                        | "machine.cancel-start"
                        | "card.show-folder"
                )
        }) {
            item(&sub, &c.id);
            if c.id == "machine.capture-mouse" {
                let p = gio::Menu::new();
                for id in ["machine.pointer-follow", "machine.pointer-capture"] {
                    item(&p, id);
                }
                sub.append_section(None, &p);
            }
        }
        by_group.append_submenu(Some(name), &sub);
    }
    menu.append_section(None, &by_group);
    let app = gio::Menu::new();
    for c in commands.iter().filter(|c| c.group == Group::App) {
        item(&app, &c.id);
    }
    item(&app, "view.shortcuts");
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

/// A status page of the core's words, with a button when the words name one.
fn status(icon: &str, kind: EmptyKind) -> adw::StatusPage {
    let e = empty_state(kind);
    let page = adw::StatusPage::builder()
        .icon_name(icon)
        .title(e.title)
        .description(e.sentence)
        .build();
    page.set_widget_name(match kind {
        EmptyKind::NoMachines => "empty.no-machines",
        _ => "empty.none-selected",
    });
    if let (Some(label), EmptyKind::NoMachines) = (e.button, kind) {
        page.set_child(Some(
            &gtk::Button::builder()
                .label(label)
                .halign(gtk::Align::Center)
                .action_name("win.machine.new")
                .css_classes(["suggested-action", "pill"])
                .build(),
        ));
    }
    page
}

pub fn build(app: &Rc<App>, with_library: bool) -> Rc<Win> {
    let tv = adw::TabView::new();
    // One open machine needs no tab bar; it appears with the second.
    let bar = adw::TabBar::builder()
        .view(&tv)
        .autohide(true)
        .expand_tabs(false)
        .build();
    let empty = status("computer-symbolic", EmptyKind::NoneSelected);
    let stack = gtk::Stack::new();
    stack.add_named(&tv, Some("tabs"));
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name("empty");

    let drop_caption = gtk::Label::builder()
        .css_classes(["hud", "title-3"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .visible(false)
        .can_target(false)
        .build();
    let over = gtk::Overlay::builder().child(&stack).build();
    over.add_overlay(&drop_caption);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&over));

    let title = adw::WindowTitle::new(UiString::Origami.text(), "");

    let tip = |id: &str| {
        app.commands
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.tooltip.clone())
    };
    let power_btn = gtk::Button::builder()
        .label(UiString::Start.text())
        .tooltip_text(tip("machine.start").unwrap_or_default())
        .action_name("win.machine.start")
        .css_classes(["suggested-action"])
        .build();
    let pause_btn = gtk::Button::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text(tip("machine.pause").unwrap_or_default())
        .action_name("win.machine.pause")
        .build();
    // One icon toggle: down sends the mouse to the machine on a click, up follows the pointer.
    let pointer_btn = gtk::ToggleButton::builder()
        .icon_name("input-mouse-symbolic")
        .tooltip_text(tip("machine.capture-mouse").unwrap_or_default())
        .build();
    let syncing = Rc::new(Cell::new(false));
    {
        let syncing = syncing.clone();
        pointer_btn.connect_toggled(move |b| {
            if syncing.get() {
                return;
            }
            let _ = b.activate_action(
                if b.is_active() {
                    "win.machine.pointer-capture"
                } else {
                    "win.machine.pointer-follow"
                },
                None,
            );
        });
    }
    let console_btn = gtk::Button::builder()
        .icon_name("utilities-terminal-symbolic")
        .tooltip_text(tip("window.console").unwrap_or_default())
        .action_name("win.window.console")
        .build();
    let settings_btn = gtk::Button::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text(tip("machine.settings").unwrap_or_default())
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
        .tooltip_text(UiString::ShowOrHideTheLibrary.text())
        .build();
    if with_library {
        header.pack_start(&sidebar_toggle);
    }
    header.pack_start(&power_btn);
    header.pack_start(&pause_btn);
    header.set_title_widget(Some(&title));
    header.pack_end(&menu_btn);
    header.pack_end(&settings_btn);
    header.pack_end(&console_btn);
    header.pack_end(&pointer_btn);

    // GNOME has no status-bar widget: the picture's size and the pointer's
    // words sit in the flat bottom bar of the content, while a machine runs.
    let size_label = gtk::Label::builder()
        .css_classes(["caption", "dim-label"])
        .xalign(0.0)
        .hexpand(true)
        .build();
    let pointer_label = gtk::Label::builder()
        .css_classes(["caption", "dim-label"])
        .xalign(1.0)
        .build();
    let bottom = gtk::Box::builder()
        .margin_start(12)
        .margin_end(12)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    bottom.append(&size_label);
    bottom.append(&pointer_label);
    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    content.add_top_bar(&bar);
    content.add_bottom_bar(&bottom);
    content.set_reveal_bottom_bars(false);
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
    let side = gtk::Stack::new();
    if with_library {
        let side_head = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .build();
        let heading = adw::WindowTitle::new("Machines", "");
        heading.set_widget_name("sidebar.heading");
        side_head.set_title_widget(Some(&heading));
        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(tip("machine.new").unwrap_or_default())
            .action_name("win.machine.new")
            .build();
        side_head.pack_end(&add);
        let library = adw::ToolbarView::new();
        library.add_top_bar(&side_head);
        library.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .child(&list_host)
                .vexpand(true)
                .build(),
        ));
        side.add_named(&library, Some("library"));
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
    menu.append(Some(&show_in_label(Platform::Linux)), Some("row.show"));
    menu.append(Some(UiString::Diagnostics.text()), Some("row.diagnostics"));
    if let Some(c) = app.commands.iter().find(|c| c.id == "machine.remove") {
        menu.append(Some(&c.title), Some("row.remove"));
    }
    let row_menu = gtk::PopoverMenu::from_model(Some(&menu));
    row_menu.set_parent(&list_host);
    row_menu.set_has_arrow(false);
    row_menu.set_halign(gtk::Align::Start);
    let toolbar: Vec<gtk::Widget> = [
        power_btn.upcast_ref::<gtk::Widget>(),
        pause_btn.upcast_ref(),
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
        power_btn,
        pause_btn,
        pointer_btn: pointer_btn.clone(),
        syncing,
        toolbar,
        cpu_on: Cell::new(false),
        content,
        size_label,
        pointer_label,
        side,
        card_for: RefCell::new(None),
        drop_caption,
        stack,
        list: list.clone(),
        rows: RefCell::new(Vec::new()),
        row_menu,
        row_dir: RefCell::new(None),
        has_library: with_library,
    });
    register_actions(app, &w);
    wire_tabs(app, &w);
    wire_row_menu(app, &w);
    wire_drops(app, &w);
    {
        // A machine that cannot open shows its card as soon as its row is selected.
        let (app, weak) = (app.clone(), Rc::downgrade(&w));
        let outer = app.clone();
        list.connect_row_selected(move |_, row| {
            let Some(w) = weak.upgrade() else { return };
            let entry = row.and_then(|row| {
                let dir = w
                    .rows
                    .borrow()
                    .iter()
                    .find(|r| &r.row == row)
                    .map(|r| r.dir.clone())?;
                app.library.borrow().iter().find(|e| e.dir == dir).cloned()
            });
            let open = entry
                .as_ref()
                .is_some_and(|e| app.machines.borrow().iter().any(|m| m.dir == e.dir));
            match entry {
                Some(e) if !open && (e.needs_upgrade || e.error.is_some()) => {
                    crate::card::show(&app, &w, &e, None)
                }
                _ => clear_card(&w),
            }
        });
        let (app, weak) = (outer, Rc::downgrade(&w));
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

/// The files a drag carries, once its value has loaded.
fn dropped(value: &glib::Value) -> Vec<PathBuf> {
    value
        .get::<gtk::gdk::FileList>()
        .map(|l| l.files().iter().filter_map(|f| f.path()).collect())
        .unwrap_or_default()
}

/// Drop a CD image on a machine's page to change its disc, or a disk image on the
/// library to make a machine from it. The caption says which while the drag is over.
fn wire_drops(app: &Rc<App>, w: &Rc<Win>) {
    let target = |kind: DropKind| {
        let t = gtk::DropTarget::new(
            gtk::gdk::FileList::static_type(),
            gtk::gdk::DragAction::COPY,
        );
        t.set_preload(true);
        let weak = Rc::downgrade(w);
        let caption = move |t: &gtk::DropTarget| {
            let Some(w) = weak.upgrade() else { return };
            let first = t
                .value()
                .map(|v| dropped(&v))
                .and_then(|f| f.first().cloned());
            let words = first
                .as_deref()
                .filter(|p| classify_drop(p) == kind)
                .and_then(|_| drop_caption(kind));
            w.drop_caption.set_label(words.unwrap_or_default());
            w.drop_caption
                .set_visible(words.is_some() && t.current_drop().is_some());
        };
        t.connect_value_notify(caption.clone());
        t.connect_enter(move |t, _, _| {
            caption(t);
            gtk::gdk::DragAction::COPY
        });
        let weak = Rc::downgrade(w);
        t.connect_leave(move |_| {
            if let Some(w) = weak.upgrade() {
                w.drop_caption.set_visible(false);
            }
        });
        t
    };
    let page = target(DropKind::CdImage);
    {
        let (app, weak) = (app.clone(), Rc::downgrade(w));
        page.connect_drop(move |_, value, _, _| {
            let Some(w) = weak.upgrade() else {
                return false;
            };
            w.drop_caption.set_visible(false);
            drop_on_page(&app, &w, dropped(value))
        });
    }
    w.stack.add_controller(page);
    if w.has_library {
        let library = target(DropKind::DiskImage);
        let (app, weak) = (app.clone(), Rc::downgrade(w));
        library.connect_drop(move |_, value, _, _| {
            let Some(w) = weak.upgrade() else {
                return false;
            };
            w.drop_caption.set_visible(false);
            drop_on_library(&app, &w, dropped(value))
        });
        w.list.add_controller(library);
    }
}

/// A CD image dropped on the machine's page changes its disc.
pub fn drop_on_page(app: &Rc<App>, w: &Rc<Win>, files: Vec<PathBuf>) -> bool {
    let file = files
        .into_iter()
        .find(|p| classify_drop(p) == DropKind::CdImage);
    match (file, current(app, w)) {
        (Some(file), Some(m)) => {
            change_cd(app, &m, file);
            true
        }
        _ => false,
    }
}

/// A disk image dropped on the library starts New machine with it chosen.
pub fn drop_on_library(app: &Rc<App>, w: &Rc<Win>, files: Vec<PathBuf>) -> bool {
    match files
        .into_iter()
        .find(|p| classify_drop(p) == DropKind::DiskImage)
    {
        Some(file) => {
            settings::new_machine_from(app, &w.win, file);
            true
        }
        None => false,
    }
}

/// Put `image` in the machine's CD drive, attaching a drive first when it has none.
fn change_cd(app: &Rc<App>, m: &Rc<MachineUi>, image: PathBuf) {
    let (app, m) = (app.clone(), m.clone());
    glib::spawn_future_local(async move {
        let (machine, file) = (m.machine.clone(), image.clone());
        let done = gtk::gio::spawn_blocking(move || {
            let drive = origami_machine::read_machine(std::path::Path::new(machine.key()))
                .ok()
                .and_then(|f| {
                    f.drive
                        .into_iter()
                        .find(|d| d.kind == "cdrom")
                        .map(|d| d.name)
                });
            match drive {
                Some(name) => machine
                    .change_media(&name, Some(&file))
                    .map(|()| Some(name)),
                None => {
                    let target = machine.settings()?.cd_target;
                    let request = origami_core::DriveRequest {
                        file,
                        kind: origami_core::DriveKind::Cdrom,
                        target: target.ok_or_else(|| {
                            CoreError::new(ErrorKind::Unsupported, "This machine has no CD drive.")
                        })?,
                        overlay: false,
                    };
                    machine
                        .apply_change(&Change::AttachDrive(request))
                        .map(|_| None)
                }
            }
        })
        .await;
        match done {
            Ok(Ok(drive)) => {
                if let (true, Some(drive)) = (m.state.get().is_up(), drive) {
                    let shown = image.file_name().map(|n| n.to_string_lossy().into_owned());
                    m.media.borrow_mut().insert(drive, shown);
                }
                *m.entry.borrow_mut() = crate::library::fresh(&app.core, &m.dir);
            }
            Ok(Err(e)) => *m.error.borrow_mut() = Some(e),
            Err(_) => {}
        }
        rebuild_hero(&m);
        refresh(&app);
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
    let remove = gio::SimpleAction::new("remove", None);
    {
        let (app, weak) = (app.clone(), Rc::downgrade(w));
        remove.connect_activate(move |_, _| {
            let Some(w) = weak.upgrade() else { return };
            let dir = w.row_dir.borrow().clone();
            let entry = dir.and_then(|d| app.library.borrow().iter().find(|e| e.dir == d).cloned());
            if let Some(e) = entry {
                remove_machine(&app, &w, e);
            }
        });
    }
    actions.add_action(&open);
    actions.add_action(&show);
    actions.add_action(&diag);
    actions.add_action(&remove);
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

/// Show the panel on the selected tab's machine when CPU load is on, and
/// hide every other one; start polling for it.
pub fn sync_cpu_panel(app: &Rc<App>, w: &Rc<Win>) {
    let on = w.cpu_on.get();
    let current = current(app, w);
    for m in machines_in(app, w) {
        let show = on && current.as_ref().is_some_and(|c| Rc::ptr_eq(c, &m));
        if m.cpu.root.is_visible() != show {
            m.cpu.root.set_visible(show);
            if show {
                m.cpu.show(m.machine.osview());
                let (m2, weak) = (m.clone(), Rc::downgrade(w));
                crate::cpu_panel::poll(
                    move || {
                        weak.upgrade().is_some_and(|w| w.cpu_on.get()) && m2.cpu.root.is_visible()
                    },
                    m,
                );
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
            let Some(w) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let child = page.child();
            let m = app
                .machines
                .borrow()
                .iter()
                .find(|m| m.root.upcast_ref::<gtk::Widget>() == &child)
                .cloned();
            let Some(m) = m else {
                return glib::Propagation::Proceed;
            };
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
            if !m.state.get().is_up() || alerts::skipped(&app) {
                forget(&app, &m);
                tv.close_page_finish(page, true);
                return glib::Propagation::Stop;
            }
            let (tv, page, app) = (tv.clone(), page.clone(), app.clone());
            glib::spawn_future_local(async move {
                let kind = AlertKind::CloseTab {
                    name: m.name.clone(),
                };
                match alerts::ask(&w.win, &kind).await {
                    ButtonRole::Safe => {
                        forget(&app, &m);
                        tv.close_page_finish(&page, true);
                    }
                    ButtonRole::Destructive => {
                        app::stop(&app, &m, StopKind::Clean);
                        forget(&app, &m);
                        tv.close_page_finish(&page, true);
                    }
                    ButtonRole::Cancel => tv.close_page_finish(&page, false),
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
                        w.list.unselect_all();
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
                clear_card(&w);
                sync_selection(&app, &w);
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
    if running.is_empty() || alerts::skipped(app) {
        ms.iter().for_each(|m| forget(app, m));
        drop_window(app, w);
        return glib::Propagation::Proceed;
    }
    let kind = AlertKind::CloseWindow {
        names: running.iter().map(|m| m.name.clone()).collect(),
    };
    let (app, w) = (app.clone(), w.clone());
    glib::spawn_future_local(async move {
        let choice = alerts::ask(&w.win, &kind).await;
        if choice == ButtonRole::Cancel {
            return;
        }
        for m in &ms {
            if choice == ButtonRole::Destructive && m.state.get().is_up() {
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

/// Fill the content with `entry`'s card (needs upgrade, can't open), over any tabs.
pub fn show_card(app: &Rc<App>, entry: &Entry, error: Option<CoreError>) {
    if let Some(w) = active(app) {
        crate::card::show(app, &w, entry, error);
    }
}

/// The highlighted row follows the open tab, and clears when none is open.
fn sync_selection(app: &Rc<App>, w: &Win) {
    if !w.has_library || w.card_for.borrow().is_some() {
        return;
    }
    match current(app, w) {
        Some(m) => {
            let row = w
                .rows
                .borrow()
                .iter()
                .find(|r| r.dir == m.dir)
                .map(|r| r.row.clone());
            if let Some(row) = row.filter(|r| w.list.selected_row().as_ref() != Some(r)) {
                w.list.select_row(Some(&row));
            }
        }
        None if w.tv.n_pages() == 0 => w.list.unselect_all(),
        None => {}
    }
}

/// Back from a card to the tabs, or to the empty page.
pub fn clear_card(w: &Win) {
    if w.card_for.take().is_some() {
        w.stack
            .set_visible_child_name(if w.tv.n_pages() == 0 { "empty" } else { "tabs" });
    }
}

/// The library, or the install guide while the selected machine's install runs.
fn sync_side(w: &Win, m: Option<&Rc<MachineUi>>) {
    let want = match m {
        Some(m) if crate::install::guide_running(m.machine.key()) => "guide",
        _ => "library",
    };
    if w.has_library && w.side.child_by_name(want).is_some() {
        w.side.set_visible_child_name(want);
        // The guide's steps need more room than the library's names.
        w.split
            .set_min_sidebar_width(if want == "guide" { 360.0 } else { 230.0 });
        w.split
            .set_sidebar_width_fraction(if want == "guide" { 0.3 } else { 0.22 });
        if want == "guide" {
            w.split.set_show_sidebar(true);
        }
    }
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

/// Ask the core's alert for `kind`, then run `then` when the destructive button is chosen.
fn confirm(app: &Rc<App>, w: &Rc<Win>, kind: AlertKind, then: impl Fn() + 'static) {
    if alerts::skipped(app) {
        then();
        return;
    }
    let win = w.win.clone();
    glib::spawn_future_local(async move {
        if alerts::ask(&win, &kind).await == ButtonRole::Destructive {
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
            if matches!(m.state.get(), State::Running | State::NoPicture) {
                let kind = AlertKind::PowerOff {
                    name: m.name.clone(),
                };
                confirm(app, w, kind, move || app::stop(&a, &mm, StopKind::Clean));
            } else {
                app::stop(app, &m, StopKind::Clean);
            }
        }
        ("machine.cancel-start", Some(m)) => app::stop(app, &m, StopKind::Force),
        ("machine.force-quit", Some(m)) => {
            let (a, mm) = (app.clone(), m.clone());
            let kind = AlertKind::ForceQuit {
                name: m.name.clone(),
            };
            confirm(app, w, kind, move || app::stop(&a, &mm, StopKind::Force));
        }
        ("machine.reset", Some(m)) => {
            let (a, mm) = (app.clone(), m.clone());
            let kind = AlertKind::Reset {
                name: m.name.clone(),
            };
            confirm(app, w, kind, move || app::control(&a, &mm, Control::Reset));
        }
        ("machine.remove", _) => {
            if let Some(entry) = current(app, w)
                .map(|m| m.entry.borrow().clone())
                .or_else(|| selected_entry(app, w))
            {
                remove_machine(app, w, entry);
            }
        }
        ("machine.pause", Some(m)) => app::control(app, &m, Control::Pause),
        ("machine.resume", Some(m)) => app::control(app, &m, Control::Resume),
        ("machine.release-keys", Some(m)) => input::release_all(app, &m),
        ("machine.screenshot", Some(m)) => screenshot(w, &m),
        ("machine.capture-mouse", Some(m)) => {
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
        ("view.shortcuts", _) => crate::shortcuts::present(&w.win),
        ("card.show-folder", Some(m)) => show_in_files(&w.win, m.dir.join("machine.toml")),
        ("view.cpu-load", _) => {
            w.cpu_on.set(!w.cpu_on.get());
            sync_cpu_panel(app, w);
        }
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

thread_local! {
    /// The last toast shown, for the self-test.
    static LAST_TOAST: RefCell<String> = const { RefCell::new(String::new()) };
}

pub fn last_toast() -> String {
    LAST_TOAST.with_borrow(Clone::clone)
}

/// The core's toast for `kind`, with its button's action when it has one.
pub fn toast_for(app: &App, kind: &ToastKind, action: impl Fn() + 'static) {
    let t = core_toast(kind, Platform::Linux);
    LAST_TOAST.with_borrow_mut(|l| *l = t.text.clone());
    let toast = adw::Toast::builder().title(&t.text).build();
    if let Some(label) = &t.action {
        toast.set_button_label(Some(label));
        toast.connect_button_clicked(move |_| action());
    }
    if let Some(w) = active(app) {
        w.toasts.add_toast(toast);
    }
}

/// Close `m`'s tab without asking, for a stopped machine that is going away.
fn close_tab(app: &Rc<App>, m: &Rc<MachineUi>) {
    let Some(page) = m.page.borrow().clone() else {
        return;
    };
    for w in app.windows.borrow().iter() {
        if w.tv.page_position(&page) >= 0 {
            w.tv.close_page(&page);
        }
    }
}

/// Move a stopped machine to the Trash after asking, then say so in a toast. The alert
/// names the disk images that stay where they are, which the core reads before anything moves.
fn remove_machine(app: &Rc<App>, w: &Rc<Win>, entry: Entry) {
    let (app2, w2) = (app.clone(), w.clone());
    glib::spawn_future_local(async move {
        let key = entry.dir.to_string_lossy().into_owned();
        let (core, k) = (app2.core.clone(), key.clone());
        let plan = gtk::gio::spawn_blocking(move || core.remove_plan(&k)).await;
        let disk_left = match plan {
            Ok(Ok(plan)) => plan.disk_left,
            _ => Vec::new(),
        };
        let kind = AlertKind::Remove {
            name: entry.name.clone(),
            disk_left,
        };
        let (app, win) = (app2.clone(), w2.win.clone());
        confirm(&app2, &w2, kind, move || {
            if let Some(m) = app::by_key(&app, &key) {
                close_tab(&app, &m);
            }
            let (app, win, key) = (app.clone(), win.clone(), key.clone());
            glib::spawn_future_local(async move {
                let core = app.core.clone();
                match gtk::gio::spawn_blocking(move || core.remove_machine(&key)).await {
                    Ok(Ok(removed)) => {
                        app::rescan(&app);
                        toast_for(&app, &ToastKind::Removed { name: removed.name }, || {});
                    }
                    Ok(Err(e)) => {
                        let d = adw::AlertDialog::new(Some(&e.message), e.detail.as_deref());
                        d.add_response("ok", "OK");
                        d.present(Some(&win));
                    }
                    Err(_) => {}
                }
            });
        });
    });
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
            let weak = Rc::downgrade(w);
            let win = w.win.clone();
            let t = core_toast(&ToastKind::ScreenshotSaved, Platform::Linux);
            let toast = adw::Toast::builder().title(&t.text).build();
            toast.set_button_label(t.action.as_deref());
            toast.connect_button_clicked(move |_| {
                if weak.upgrade().is_some() {
                    show_in_files(&win, path.clone());
                }
            });
            w.toasts.add_toast(toast);
        }
        Some(Err(e)) => *m.error.borrow_mut() = Some(CoreError::new(ErrorKind::Io, e.to_string())),
        None => {
            *m.error.borrow_mut() = Some(CoreError::new(
                ErrorKind::NotFound,
                "there is no picture to save",
            ))
        }
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
    // Not built yet: the Eject CD command.
    if id == "machine.eject-cd" {
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
            "machine.remove" if m.is_none() => selected,
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
            "machine.cancel-start" => st == Some(State::Starting),
            _ => when.holds(core_state),
        };
    }
    match id {
        "machine.capture-mouse" => running,
        "machine.pointer-follow" | "machine.pointer-capture" => m.is_some(),
        id if ViewScale::from_command(id).is_some() => m.is_some(),
        "window.close-tab" | "window.move-to-new-window" | "view.cpu-load" => m.is_some(),
        _ => true,
    }
}

/// Which accelerators are live. While input is captured the guest owns every
/// key, so none are.
pub fn sync_accels(app: &Rc<App>) {
    let captured = app.machines.borrow().iter().any(|m| m.captured.get());
    for c in &app.commands {
        let accels: Vec<&str> = if captured && c.id != "machine.capture-mouse" {
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
        if m.hero_state.get() != Some(m.shown()) {
            rebuild_hero(&m);
        }
    }
    for w in app.windows.borrow().iter() {
        let m = current(app, w);
        let carded = w.card_for.borrow().clone();
        tint_bar(
            &w.bar,
            m.as_ref().and_then(|m| m.entry.borrow().art),
            m.as_ref()
                .is_some_and(|m| m.root.visible_child_name().as_deref() == Some("hero")),
        );
        match (&m, &carded) {
            (Some(m), None) => {
                let label = m.label();
                w.title.set_title(&m.name);
                w.title.set_subtitle(label.word);
                let up = m.state.get().is_up();
                let (label, tip, action) = match m.state.get() {
                    State::Starting => (
                        UiString::CancelStart.text(),
                        "Stop the machine before it has started",
                        "win.machine.cancel-start",
                    ),
                    State::Stopping => (
                        "Force quit",
                        "End the machine at once",
                        "win.machine.force-quit",
                    ),
                    _ if up => (
                        UiString::PowerOff.text(),
                        command_tip(app, "machine.stop"),
                        "win.machine.stop",
                    ),
                    _ => (
                        UiString::Start.text(),
                        command_tip(app, "machine.start"),
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
                w.syncing.set(true);
                w.pointer_btn
                    .set_active(m.pointer.get() == crate::prefs::PointerMode::Capture);
                w.syncing.set(false);
                w.pause_btn.set_tooltip_text(Some(command_tip(
                    app,
                    if paused {
                        "machine.resume"
                    } else {
                        "machine.pause"
                    },
                )));
                w.win.set_title(Some(&format!("{} - Origami", m.name)));
            }
            (_, Some(dir)) => {
                let entry = app.library.borrow().iter().find(|e| &e.dir == dir).cloned();
                if let Some(e) = entry {
                    w.title.set_title(&e.name);
                    w.title.set_subtitle(state_label(e.shown()).word);
                    w.win.set_title(Some(&format!("{} - Origami", e.name)));
                }
            }
            (None, None) => {
                w.title.set_title(UiString::Origami.text());
                w.title.set_subtitle("");
                w.power_btn.set_label(UiString::Start.text());
                w.power_btn
                    .set_tooltip_text(Some(command_tip(app, "machine.start")));
                w.power_btn.set_action_name(Some("win.machine.start"));
                w.win.set_title(Some(UiString::Origami.text()));
            }
        }
        // The bottom bar belongs to the machine's picture, never to a card.
        let bar = match (&m, &carded) {
            (Some(m), None) => {
                let follows = m.pointer.get() == crate::prefs::PointerMode::Follow;
                let view = m.machine.view();
                // A console-only machine has no picture or pointer to describe.
                status_bar(&view, follows, m.captured.get(), Platform::Linux)
                    .filter(|_| view.has_display)
            }
            _ => None,
        };
        w.content.set_reveal_bottom_bars(bar.is_some());
        if let Some(b) = &bar {
            w.size_label.set_label(b.picture.as_deref().unwrap_or(""));
            w.pointer_label
                .set_label(b.pointer.as_deref().unwrap_or(""));
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
        sync_side(w, m.as_ref());
        for r in w.rows.borrow().iter() {
            let entry = app
                .library
                .borrow()
                .iter()
                .find(|e| e.dir == r.dir)
                .cloned();
            let Some(entry) = entry else { continue };
            let open = app
                .machines
                .borrow()
                .iter()
                .find(|m| m.dir == r.dir)
                .cloned();
            let state = open.as_ref().map_or_else(|| entry.shown(), |m| m.shown());
            let line = library_line_for(state, entry.model());
            r.word.set_label(&line.text);
            style::set_dot(&r.dot, line.tone);
            r.dot.set_visible(line.tone != Tone::None);
            let description = origami_core::facade::row_description_for(
                state,
                &entry.name,
                entry.model(),
                entry.system_disk.as_deref(),
            );
            r.row.set_tooltip_text(Some(&description));
            r.row
                .update_property(&[gtk::accessible::Property::Label(&description)]);
        }
    }
}

fn command_tip<'a>(app: &'a App, id: &str) -> &'a str {
    app.commands
        .iter()
        .find(|c| c.id == id)
        .map_or("", |c| c.tooltip.as_str())
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
        if let Some(old) = w.stack.child_by_name("library-empty") {
            w.stack.remove(&old);
        }
        if entries.is_empty() {
            let page = status("computer-symbolic", EmptyKind::NoMachines);
            w.stack.add_named(&page, Some("library-empty"));
            if w.tv.n_pages() == 0 {
                w.stack.set_visible_child_name("library-empty");
            }
        } else if w.stack.visible_child_name().as_deref() == Some("library-empty") {
            w.stack
                .set_visible_child_name(if w.tv.n_pages() == 0 { "empty" } else { "tabs" });
        }
        for e in entries {
            let g = machine_mark(e.art, 34, 18);
            let title = gtk::Label::builder()
                .label(&e.name)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build();
            let dot = style::dot(Tone::None);
            let word = gtk::Label::builder()
                .css_classes(["caption", "dim-label"])
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            let states = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            states.append(&dot);
            states.append(&word);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
            text.append(&title);
            text.append(&states);
            let row_box = gtk::Box::builder()
                .spacing(8)
                .margin_top(6)
                .margin_bottom(6)
                .margin_start(6)
                .margin_end(6)
                .build();
            row_box.append(&g);
            row_box.append(&text);
            let row = gtk::ListBoxRow::builder().child(&row_box).build();
            w.list.append(&row);
            w.rows.borrow_mut().push(Row {
                dir: e.dir.clone(),
                row,
                name: title,
                dot,
                word,
            });
        }
    }
    for w in app.windows.borrow().iter() {
        sync_selection(app, w);
    }
    refresh(app);
}

/// The action name a card button runs.
fn card_action(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::OpenConsole => "win.window.console",
        ActionKind::PowerOff => "win.machine.stop",
        ActionKind::Reconnect | ActionKind::TryAgain => "win.machine.start",
        ActionKind::Resume => "win.machine.resume",
        ActionKind::ShowInFolder => "win.card.show-folder",
        ActionKind::OpenSettings | ActionKind::ChooseDisk | ActionKind::ChooseFirmware => {
            "win.machine.settings"
        }
        ActionKind::Upgrade => "win.machine.start",
    }
}

fn card_buttons(actions: &[origami_core::facade::CardAction]) -> gtk::Box {
    let row = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    for (i, a) in actions.iter().enumerate() {
        let b = gtk::Button::builder()
            .label(&a.label)
            .action_name(card_action(a.kind))
            .build();
        if a.kind == ActionKind::Resume {
            b.set_widget_name("card.resume");
        }
        if i == 0 {
            b.add_css_class("suggested-action");
            b.add_css_class("pill");
        }
        row.append(&b);
    }
    row
}

/// What lies over the picture: the No picture card, or Resume over a dimmed picture.
fn rebuild_veil(m: &Rc<MachineUi>, shown: ShownState, card: Option<&origami_core::facade::Card>) {
    while let Some(c) = m.veil.first_child() {
        m.veil.remove(&c);
    }
    m.veil.remove_css_class("scrim");
    let Some(card) = card.filter(|_| matches!(shown, ShownState::NoPicture | ShownState::Paused))
    else {
        m.veil.set_visible(false);
        return;
    };
    m.veil.set_visible(true);
    if shown == ShownState::Paused {
        m.veil.add_css_class("scrim");
        let b = card_buttons(&card.actions);
        b.set_halign(gtk::Align::Center);
        b.set_valign(gtk::Align::Center);
        b.set_hexpand(true);
        b.set_vexpand(true);
        m.veil.append(&b);
        return;
    }
    let page = adw::StatusPage::builder()
        .title(&card.title)
        .description(&card.body)
        .child(&card_buttons(&card.actions))
        .build();
    page.add_css_class("on-dark");
    let plate = gtk::Box::builder()
        .css_classes(["plate"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    plate.append(&page);
    m.veil.append(&plate);
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
    for (text, css) in [(value, "heading"), (title, "caption")] {
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

/// Rebuild a machine's page: the card its state has, and what it is made of.
pub fn rebuild_hero(m: &Rc<MachineUi>) {
    let shown = m.shown();
    m.hero_state.set(Some(shown));
    while let Some(c) = m.hero.first_child() {
        m.hero.remove(&c);
    }
    m.hero.set_margin_top(24);
    m.hero.set_margin_bottom(24);
    m.hero.set_margin_start(24);
    m.hero.set_margin_end(24);
    let e = m.entry.borrow().clone();
    let error = m.error.borrow().clone();
    let card = machine_card(shown, error.as_ref(), &e.name, Platform::Linux);
    rebuild_veil(m, shown, card.as_ref());

    let page = adw::StatusPage::builder().vexpand(true).build();
    page.set_widget_name("machine.page");
    page.add_css_class("machine-art");
    match e.art.and_then(crate::art::hero_paintable) {
        Some(p) => page.set_paintable(Some(&p)),
        None => page.set_icon_name(Some("computer-symbolic")),
    }
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .halign(gtk::Align::Center)
        .build();
    match card.as_ref().filter(|_| shown != ShownState::Paused) {
        Some(c) => {
            page.set_title(&c.title);
            page.set_description(Some(&c.body));
            if !c.actions.is_empty() {
                body.append(&card_buttons(&c.actions));
            }
            if shown == ShownState::Saving {
                let bar = gtk::ProgressBar::builder().pulse_step(0.1).build();
                let b = bar.clone();
                glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                    if b.root().is_none() {
                        return glib::ControlFlow::Break;
                    }
                    b.pulse();
                    glib::ControlFlow::Continue
                });
                body.append(&bar);
            }
            if let Some(d) = &c.details {
                body.append(&crate::card::details_row(d));
            }
        }
        None => {
            let start = gtk::Button::builder()
                .label(UiString::Start.text())
                .action_name("win.machine.start")
                .halign(gtk::Align::Center)
                .css_classes(["suggested-action", "pill"])
                .build();
            body.append(&start);
        }
    }
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
            "System" => "drive-harddisk-symbolic",
            _ => "view-app-grid-symbolic",
        };
        flow.append(&summary_card(icon, &c.value, &c.title));
    }
    body.append(&flow);
    page.set_child(Some(&body));
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
    m.hero.append(&page);
}

/// The app icon from the binary, so About shows it where the icon theme has none (a run outside an install).
fn app_icon() -> gtk::gdk::Texture {
    const PNG: &[u8] =
        include_bytes!("../../../resources/linux/icons/hicolor/256x256/apps/fans.irix.Origami.png");
    gtk::gdk::Texture::from_bytes(&glib::Bytes::from_static(PNG)).expect("the bundled icon decodes")
}

/// The About dialog: libadwaita's, with the cube in place of its icon where there is a GPU for it.
fn about(app: &Rc<App>, parent: &adw::ApplicationWindow) {
    let full = option_env!("ORIGAMI_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    // A release shows the version alone; a development build keeps the git description after it.
    let version = if cfg!(debug_assertions) {
        full
    } else {
        full.split('-').next().unwrap_or(full)
    };
    let dark = adw::StyleManager::default().is_dark();
    let scene = crate::cube::about_scene();
    let url = origami_credits::credits().ok().map(|c| c.project_url);
    let cube = crate::cube::Cube::new(420, 230, crate::cube::about_style(dark, scene)).map(Rc::new);
    let dialog = adw::AboutDialog::builder()
        .application_name(UiString::Origami.text())
        .application_icon("fans.irix.Origami")
        .version(origami_core::facade::version_line(version))
        .build();
    if let Some(url) = url {
        dialog.set_website(&url);
    }
    // The two credit lists are the app's own windows; their links open them.
    for (title, uri) in [
        (
            UiString::Acknowledgements.text(),
            "origami:acknowledgements",
        ),
        (UiString::Licences.text(), "origami:licences"),
    ] {
        dialog.add_link(title, uri);
    }
    {
        let parent = parent.clone();
        dialog.connect_activate_link(move |_, uri| match uri {
            "origami:acknowledgements" => {
                crate::credits::acknowledgements(&parent);
                true
            }
            "origami:licences" => {
                crate::credits::licences(&parent);
                true
            }
            _ => false,
        });
    }
    // The scene was picked from the clock at open; only the tones follow the appearance.
    let style = adw::StyleManager::default();
    let handler = RefCell::new(cube.as_ref().map(|cube| {
        let cube = cube.clone();
        style.connect_dark_notify(move |s| {
            cube.set_style(crate::cube::about_style(s.is_dark(), scene))
        })
    }));
    dialog.connect_closed(move |_| {
        if let Some(id) = handler.take() {
            adw::StyleManager::default().disconnect(id);
        }
    });
    // Shift-click on the cube and the right arrow step through the scenes.
    let keys = gtk::EventControllerKey::new();
    if let Some(cube) = &cube {
        cube.enable_pointer();
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
    dialog.add_controller(keys);
    dialog.add_controller(egg_keys(app, &dialog));
    dialog.present(Some(parent));
    // The stock dialog's icon makes way for the cube, or shows the bundled picture where the theme has none.
    let mut images = Vec::new();
    crate::automation::descendants(
        dialog.upcast_ref(),
        &|c| c.has_css_class("icon-dropshadow"),
        &mut images,
    );
    let Some(icon) = images.first() else {
        return eprintln!("origami-gtk: no cube: About has no icon to replace");
    };
    match (&cube, icon.downcast_ref::<gtk::Image>()) {
        (Some(cube), _) => {
            if let Some(holder) = icon.parent().and_then(|p| p.downcast::<gtk::Box>().ok()) {
                icon.set_visible(false);
                holder.prepend(&cube.widget);
            }
        }
        (None, Some(image)) => image.set_paintable(Some(&app_icon())),
        _ => {}
    }
}

/// The letters h, i, n, v in a row put the host inventory in About's troubleshooting page.
fn egg_keys(app: &Rc<App>, about: &adw::AboutDialog) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let seen = RefCell::new(String::new());
    let (app, about) = (app.clone(), about.downgrade());
    keys.connect_key_pressed(move |_, key, _, state| {
        let plain = !state
            .intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK);
        if let (true, Some(c)) = (plain, key.to_unicode()) {
            let mut seen = seen.borrow_mut();
            seen.push(c.to_ascii_lowercase());
            let from = seen.len().saturating_sub(4);
            *seen = seen[from..].to_string();
            if *seen == "hinv" {
                show_inventory(&app, &about);
            }
        }
        glib::Propagation::Proceed
    });
    keys
}

/// Read the inventory off the UI thread (it reads the disk) and give it to About.
fn show_inventory(app: &Rc<App>, about: &glib::WeakRef<adw::AboutDialog>) {
    let (core, gpu) = (app.core.clone(), app.dev.as_ref().map(|d| d.name.clone()));
    let about = about.clone();
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
        if let Some(about) = about.upgrade() {
            about.set_debug_info(&text);
        }
    });
}
