//! Settings for one machine, Preferences for the app, and the New Machine
//! sheet. Machine settings read the machine file; changing them on a stopped
//! machine goes through the core's change operations, which this app does not
//! have yet, so the panes show values and say so instead of faking an edit.
//! Pointer mode is ours and is editable.

use crate::alerts;
use crate::app::{self, App};
use crate::disks;
use crate::library;
use crate::machine::MachineUi;
use crate::prefs::PointerMode;
use crate::prefs_dialog::Prefs;
use crate::{input, window};
use adw::prelude::*;
use gtk::glib;
use origami_core::facade::NetworkSetting;
use origami_core::facade::{empty_state, AlertKind, EmptyKind, UiString};
use origami_core::facade::{CreateRequest, DiskChoice, IdentityField, OperationState, SystemDisk};
use origami_core::{Change, Line, NetworkMode};
use std::cell::RefCell;
use std::rc::Rc;

/// How many times a machine's settings dialog has been built, for the scenarios to wait on a rebuild.
static BUILDS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn builds() -> u32 {
    BUILDS.load(std::sync::atomic::Ordering::Relaxed)
}

fn value_row(title: &str, value: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .subtitle_selectable(true)
        .build()
}

fn group(title: &str, rows: Vec<adw::ActionRow>) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::builder().title(title).build();
    for r in rows {
        g.add(&r);
    }
    g
}

/// What the settings dialog needs from the core, read off the UI thread.
struct Loaded {
    file: origami_machine::MachineFile,
    memory: Vec<u32>,
    cpus: Vec<u32>,
    console_port: String,
    console_ports: Vec<origami_core::facade::ConsolePortSetting>,
    disk_target: Option<u32>,
    cd_target: Option<u32>,
    network: NetworkSetting,
    shares: Vec<origami_core::facade::ShareSetting>,
    /// Each drive's title and caption, as the core words them.
    rows: Vec<origami_core::facade::DriveRow>,
    /// Where the PROM prints its startup messages.
    boot: Option<origami_core::facade::BootConsole>,
}

/// Through the machine's own engine, so an iris machine is read by iris.
fn load(machine: &origami_core::facade::Machine, dir: &std::path::Path) -> Result<Loaded, String> {
    let settings = machine.settings().map_err(|e| e.to_string())?;
    let file = origami_machine::read_machine(dir).map_err(|e| e.to_string())?;
    Ok(Loaded {
        memory: settings.memory_allowed_mib,
        cpus: settings.cpus_per_node_allowed,
        console_port: settings.console_port,
        console_ports: settings.console_ports,
        disk_target: settings.disk_target,
        cd_target: settings.cd_target,
        network: settings.network,
        shares: settings.shares,
        rows: machine.drive_rows().map_err(|e| e.to_string())?,
        boot: machine.boot_console().ok(),
        file,
    })
}

pub fn present_machine(app: &Rc<App>, parent: &adw::ApplicationWindow, m: &Rc<MachineUi>) {
    present_with(app, parent, m, vec![]);
}

/// Open the settings with `notes`, advice an edit just returned, shown at the top.
fn present_with(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    notes: Vec<Line>,
) {
    let (app, parent, m) = (app.clone(), parent.clone(), m.clone());
    glib::spawn_future_local(async move {
        let (machine, dir) = (m.machine.clone(), m.dir.clone());
        let loaded = gtk::gio::spawn_blocking(move || {
            // The core reads the machine through the engine, which may start it.
            load(&machine, &dir)
        })
        .await
        .unwrap_or_else(|_| Err("the settings could not be read".into()));
        build_machine_dialog(&app, &parent, &m, loaded, notes);
    });
}

/// Apply one edit to the stopped machine on a worker thread, then show the
/// result: the dialog is rebuilt from the file so it never shows a guess.
fn apply(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    dialog: &Prefs,
    change: Change,
) {
    apply_with(app, parent, m, dialog, move |machine| {
        machine.apply_change(&change)
    });
}

/// A network edit: the core builds the change from the machine's file.
fn apply_network(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    dialog: &Prefs,
    (mode, device, share, subnet): (NetworkMode, String, Option<String>, Option<String>),
) {
    apply_with(app, parent, m, dialog, move |machine| {
        machine.set_network(mode, &device, share.as_deref(), subnet.as_deref())
    });
}

fn apply_with(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    dialog: &Prefs,
    edit: impl FnOnce(
            &origami_core::facade::Machine,
        ) -> origami_core::facade::CoreResult<origami_core::Report>
        + Send
        + 'static,
) {
    let machine = m.machine.clone();
    let (app, parent, m, dialog) = (app.clone(), parent.clone(), m.clone(), dialog.clone());
    glib::spawn_future_local(async move {
        let r = gtk::gio::spawn_blocking(move || edit(&machine).map_err(|e| e.to_string())).await;
        match r {
            Ok(Ok(report)) => {
                let notes = report
                    .lines
                    .into_iter()
                    .filter(|l| l.shown().is_some())
                    .collect();
                reopen(&app, &parent, &m, &dialog, notes)
            }
            Ok(Err(e)) => dialog.add_toast(adw::Toast::new(&e)),
            Err(_) => dialog.add_toast(adw::Toast::new("the change could not be applied")),
        }
    });
}

/// Show the machine's settings again from its file after an edit.
fn reopen(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    dialog: &Prefs,
    notes: Vec<Line>,
) {
    *m.entry.borrow_mut() = library::fresh(&app.core, &m.dir);
    crate::window::rebuild_hero(m);
    crate::window::refresh(app);
    dialog.close();
    present_with(app, parent, m, notes);
}

/// While the machine runs, the line at the top of a page that says why its hardware is locked,
/// with the way out beside it.
fn add_locked(app: &Rc<App>, dialog: &Prefs, page: &adw::PreferencesPage, running: bool) {
    if !running {
        return;
    }
    let locked = adw::ActionRow::builder()
        .title(UiString::PowerOffToChangeHardwareSettings.text())
        .title_lines(0)
        .build();
    locked.set_widget_name("settings.locked");
    let power_off = gtk::Button::builder()
        .label(UiString::PowerOff.text())
        .valign(gtk::Align::Center)
        .build();
    power_off.set_widget_name("settings.power-off");
    let (app, dialog) = (app.clone(), dialog.clone());
    power_off.connect_clicked(move |_| {
        dialog.close();
        if let Some(w) = window::active(&app) {
            window::run(&app, &w, "machine.stop");
        }
    });
    locked.add_suffix(&power_off);
    let g = adw::PreferencesGroup::new();
    g.add(&locked);
    page.add(&g);
}

fn build_machine_dialog(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    loaded: Result<Loaded, String>,
    notes: Vec<Line>,
) {
    let dialog = Prefs::new(&format!("{} Settings", m.name));
    dialog.set_widget_name("settings.dialog");
    BUILDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let running = m.state.get().is_up();

    let general = adw::PreferencesPage::builder()
        .title(UiString::General.text())
        .icon_name("preferences-system-symbolic")
        .build();
    add_locked(app, &dialog, &general, running);
    general.add(&group(
        "Machine",
        vec![
            value_row(UiString::Name.text(), &m.name),
            value_row("Location", &m.dir.display().to_string()),
            value_row(UiString::Model.text(), &m.entry.borrow().product_label),
        ],
    ));
    dialog.add(&general);

    match &loaded {
        Ok(l) => {
            let f = &l.file;
            let hw = adw::PreferencesPage::builder()
                .title(UiString::ProcessorAndMemory.text())
                .icon_name("system-run-symbolic")
                .build();
            add_locked(app, &dialog, &hw, running);
            let g = adw::PreferencesGroup::builder()
                .title(glib::markup_escape_text(
                    UiString::ProcessorAndMemory.text(),
                ))
                .build();
            if let Some(p) = &f.machine.processor {
                g.add(&value_row(UiString::Processor.text(), p));
            }
            let cpus = adw::ComboRow::builder()
                .title(UiString::ProcessorsPerNode.text())
                .sensitive(!running)
                .build();
            let labels: Vec<String> = l.cpus.iter().map(u32::to_string).collect();
            cpus.set_model(Some(&gtk::StringList::new(
                &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            )));
            cpus.set_selected(
                l.cpus
                    .iter()
                    .position(|&c| c == f.machine.cpus_per_node)
                    .unwrap_or(0) as u32,
            );
            let start = cpus.selected();
            {
                let (app, parent, m, d, values) = (
                    app.clone(),
                    parent.clone(),
                    m.clone(),
                    dialog.clone(),
                    l.cpus.clone(),
                );
                cpus.connect_selected_notify(move |c| {
                    if c.selected() != start {
                        if let Some(&n) = values.get(c.selected() as usize) {
                            apply(
                                &app,
                                &parent,
                                &m,
                                &d,
                                Change::SetProcessor {
                                    module: None,
                                    cpus_per_node: Some(n),
                                },
                            );
                        }
                    }
                });
            }
            g.add(&cpus);
            let mem = adw::ComboRow::builder()
                .title(UiString::MemoryPerNode.text())
                .sensitive(!running)
                .build();
            let labels: Vec<String> = l
                .memory
                .iter()
                .map(|v| origami_core::facade::memory_label(*v))
                .collect();
            mem.set_model(Some(&gtk::StringList::new(
                &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            )));
            let current: u32 = f
                .machine
                .memory_per_node
                .trim_end_matches("MiB")
                .parse()
                .unwrap_or(0);
            mem.set_selected(l.memory.iter().position(|&v| v == current).unwrap_or(0) as u32);
            let start = mem.selected();
            {
                let (app, parent, m, d, values) = (
                    app.clone(),
                    parent.clone(),
                    m.clone(),
                    dialog.clone(),
                    l.memory.clone(),
                );
                mem.connect_selected_notify(move |c| {
                    if c.selected() != start {
                        if let Some(&v) = values.get(c.selected() as usize) {
                            apply(&app, &parent, &m, &d, Change::SetMemory(v));
                        }
                    }
                });
            }
            g.add(&mem);
            if f.machine.nodes > 1 {
                g.add(&value_row(
                    UiString::Nodes.text(),
                    &f.machine.nodes.to_string(),
                ));
            }
            hw.add(&g);
            dialog.add(&hw);

            let gfx = adw::PreferencesPage::builder()
                .title(UiString::Graphics.text())
                .icon_name("video-display-symbolic")
                .build();
            let rows: Vec<_> = if f.pipe.is_empty() {
                vec![value_row(UiString::Graphics.text(), "None")]
            } else {
                f.pipe
                    .iter()
                    .map(|p| value_row(&p.product, &p.family))
                    .collect()
            };
            gfx.add(&group(UiString::Graphics.text(), rows));
            dialog.add(&gfx);

            let drives = adw::PreferencesPage::builder()
                .title(UiString::Drives.text())
                .icon_name("drive-harddisk-symbolic")
                .build();
            add_locked(app, &dialog, &drives, running);
            let g = adw::PreferencesGroup::builder()
                .title(UiString::Drives.text())
                .build();
            for d in &l.rows {
                let cd = f
                    .drive
                    .iter()
                    .any(|x| x.name == d.name && x.kind == "cdrom");
                // A disc swapped while the machine runs is not in its file; the tab remembers it.
                let live = cd.then(|| m.media.borrow().get(&d.name).cloned()).flatten();
                let caption = match &live {
                    Some(Some(file)) => file.clone(),
                    Some(None) => String::new(),
                    None => d.caption.clone(),
                };
                let r = value_row(&d.title, &caption);
                r.set_widget_name(&format!("drive.row.{}", d.name));
                if cd {
                    cd_buttons(app, parent, m, &dialog, &r, &d.name, live != Some(None));
                }
                let b = gtk::Button::builder()
                    .icon_name("list-remove-symbolic")
                    .tooltip_text(UiString::Detach.text())
                    .valign(gtk::Align::Center)
                    .sensitive(!running)
                    .css_classes(["flat"])
                    .build();
                b.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Detach {}",
                    d.title
                ))]);
                let (app, parent, m, dlg, name) = (
                    app.clone(),
                    parent.clone(),
                    m.clone(),
                    dialog.clone(),
                    d.name.clone(),
                );
                let title = d.title.clone();
                b.connect_clicked(move |_| {
                    let (a, p, m, d, name) = (
                        app.clone(),
                        parent.clone(),
                        m.clone(),
                        dlg.clone(),
                        name.clone(),
                    );
                    let kind = AlertKind::DetachDrive {
                        name: title.clone(),
                    };
                    alerts::confirm(&app, &parent, kind, move || {
                        apply(&a, &p, &m, &d, Change::DetachDrive(name))
                    });
                });
                r.add_suffix(&b);
                g.add(&r);
            }
            let mut adds = vec![(
                "drive.add-image",
                UiString::AddADiskImage.text(),
                l.disk_target.filter(|_| !running),
            )];
            if !f.drive.iter().any(|d| d.kind == "cdrom") {
                adds.push((
                    "drive.insert-cd",
                    UiString::InsertACdImage.text(),
                    l.cd_target.filter(|_| !running),
                ));
            }
            for (id, title, target) in adds {
                let row = adw::ActionRow::builder()
                    .title(title)
                    .activatable(target.is_some())
                    .sensitive(target.is_some())
                    .build();
                row.set_widget_name(id);
                row.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
                let (app, parent, m, dlg) =
                    (app.clone(), parent.clone(), m.clone(), dialog.clone());
                row.connect_activated(move |_| match (id, target) {
                    ("drive.add-image", Some(t)) => add_image(&app, &parent, &m, &dlg, t),
                    (_, Some(t)) => {
                        let (app, parent, m, dlg) =
                            (app.clone(), parent.clone(), m.clone(), dlg.clone());
                        disks::pick_path(&parent.clone(), "Choose a CD-ROM image", move |path| {
                            let file = path;
                            let kind = origami_core::DriveKind::Cdrom;
                            let request = origami_core::DriveRequest {
                                file,
                                kind,
                                target: t,
                                overlay: false,
                            };
                            apply(&app, &parent, &m, &dlg, Change::AttachDrive(request));
                        });
                    }
                    _ => {}
                });
                g.add(&row);
            }
            drives.add(&g);
            dialog.add(&drives);

            let net = adw::PreferencesPage::builder()
                .title("Network")
                .icon_name("network-wired-symbolic")
                .build();
            add_locked(app, &dialog, &net, running);
            if !notes.is_empty() {
                net.add(&notes_group(&notes, dialog.dialog.upcast_ref()));
            }
            network_groups(app, parent, m, &dialog, &net, &l.network, running);
            let fg = adw::PreferencesGroup::builder()
                .title(UiString::PortForwards.text())
                .build();
            for p in &f.network.forward {
                let r = value_row(
                    &p.name,
                    &format!(
                        "{} host {} to guest {}",
                        p.protocol, p.host_port, p.guest_port
                    ),
                );
                let b = gtk::Button::builder()
                    .icon_name("list-remove-symbolic")
                    .tooltip_text(UiString::Remove.text())
                    .valign(gtk::Align::Center)
                    .sensitive(!running)
                    .css_classes(["flat"])
                    .build();
                b.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Remove forward {}",
                    p.name
                ))]);
                let (app, parent, m, dlg, name) = (
                    app.clone(),
                    parent.clone(),
                    m.clone(),
                    dialog.clone(),
                    p.name.clone(),
                );
                b.connect_clicked(move |_| {
                    apply(&app, &parent, &m, &dlg, Change::RemoveForward(name.clone()))
                });
                r.add_suffix(&b);
                fg.add(&r);
            }
            let add = adw::ActionRow::builder()
                .title(UiString::AddAPortForward.text())
                .activatable(!running)
                .sensitive(!running)
                .build();
            add.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
            {
                let (app, parent, m, dlg) =
                    (app.clone(), parent.clone(), m.clone(), dialog.clone());
                add.connect_activated(move |_| add_forward(&app, &parent, &m, &dlg));
            }
            fg.add(&add);
            // Forwards do nothing with the network off.
            fg.set_visible(l.network.mode != "none");
            net.add(&fg);
            dialog.add(&net);
            if !notes.is_empty() {
                dialog.set_visible_page(&net);
            }
            let folders = adw::PreferencesPage::builder()
                .title("Shared folders")
                .icon_name("folder-symbolic")
                .build();
            shares_group(app, parent, m, &dialog, &folders, &net, &l.shares, running);
            dialog.add(&folders);
            let startup = adw::PreferencesPage::builder()
                .title(UiString::StartupFirmware.text())
                .icon_name("system-shutdown-symbolic")
                .build();
            let g = adw::PreferencesGroup::builder()
                .title(glib::markup_escape_text(UiString::StartupFirmware.text()))
                .description(UiString::TheConsoleWindowAndA.text())
                .build();
            let line = adw::ComboRow::builder()
                .title(UiString::ConsoleSerialLine.text())
                .use_subtitle(true)
                .sensitive(!running)
                .build();
            line.set_widget_name("settings.console");
            let labels: Vec<&str> = l.console_ports.iter().map(|p| p.label.as_str()).collect();
            line.set_model(Some(&gtk::StringList::new(&labels)));
            line.set_selected(
                l.console_ports
                    .iter()
                    .position(|p| p.name == l.console_port)
                    .unwrap_or(0) as u32,
            );
            let start = line.selected();
            {
                let (app, parent, m, d, ports) = (
                    app.clone(),
                    parent.clone(),
                    m.clone(),
                    dialog.clone(),
                    l.console_ports.clone(),
                );
                line.connect_selected_notify(move |c| {
                    if c.selected() != start {
                        if let Some(p) = ports.get(c.selected() as usize) {
                            let port = (!p.is_default).then(|| p.name.clone());
                            apply(&app, &parent, &m, &d, Change::SetConsole(port));
                        }
                    }
                });
            }
            if let Some(boot) = l.boot {
                let row = value_row(
                    UiString::StartupMessagesOn.text(),
                    origami_core::facade::startup_value(boot),
                );
                row.set_widget_name("settings.startup-messages");
                g.add(&row);
            }
            g.add(&line);
            startup.add(&g);
            dialog.add(&startup);
        }
        Err(e) => {
            let page = adw::PreferencesPage::builder()
                .title("Problem")
                .icon_name("dialog-warning-symbolic")
                .build();
            page.add(&group(
                "This machine could not be read",
                vec![value_row("Why", e)],
            ));
            dialog.add(&page);
        }
    }

    let input_page = adw::PreferencesPage::builder()
        .title(UiString::Input.text())
        .icon_name("input-mouse-symbolic")
        .build();
    let g = adw::PreferencesGroup::builder()
        .title(UiString::Pointer.text())
        .description(
            "How the mouse reaches the guest. This machine overrides the default in Preferences.",
        )
        .build();
    let combo = adw::ComboRow::builder()
        .title(UiString::Mode.text())
        .model(&gtk::StringList::new(&[
            "Follow my mouse",
            "Capture on click",
        ]))
        .build();
    combo.set_selected(if m.pointer.get() == PointerMode::Follow {
        0
    } else {
        1
    });
    {
        let (app, m) = (app.clone(), m.clone());
        combo.connect_selected_notify(move |c| {
            let mode = if c.selected() == 0 {
                PointerMode::Follow
            } else {
                PointerMode::Capture
            };
            input::release_all(&app, &m);
            m.pointer.set(mode);
            app.prefs.borrow_mut().set_pointer_for(&m.dir, Some(mode));
            window::rebuild_hero(&m);
            window::refresh(&app);
        });
    }
    g.add(&combo);
    input_page.add(&g);
    dialog.add(&input_page);
    dialog.present(Some(parent));
}

/// One row per downloaded disk, each with Remove; `rows` holds the rows shown now.
fn list_downloaded(
    app: &Rc<App>,
    group: &adw::PreferencesGroup,
    rows: Rc<RefCell<Vec<gtk::Widget>>>,
) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }
    let found = app.core.cached_disks().unwrap_or_default();
    if found.is_empty() {
        let e = empty_state(EmptyKind::NoDownloadedDisks);
        let page = adw::StatusPage::builder()
            .title(e.title)
            .description(e.sentence)
            .css_classes(["compact"])
            .build();
        group.add(&page);
        rows.borrow_mut().push(page.upcast());
    }
    for disk in found {
        let row = adw::ActionRow::builder()
            .title(&disk.label)
            .subtitle(origami_core::facade::size_label(disk.bytes))
            .build();
        let remove = gtk::Button::builder()
            .label(UiString::Remove.text())
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&remove);
        let (again, group_again, rows_again, key, label) = (
            app.clone(),
            group.clone(),
            rows.clone(),
            disk.key,
            disk.label,
        );
        remove.connect_clicked(move |_| {
            let (a, g, rows, key) = (
                again.clone(),
                group_again.clone(),
                rows_again.clone(),
                key.clone(),
            );
            let kind = AlertKind::RemoveDownload {
                name: label.clone(),
            };
            alerts::confirm(&again, &group_again, kind, move || {
                let _ = a.core.remove_cached_disk(&key);
                list_downloaded(&a, &g, rows);
            });
        });
        group.add(&row);
        rows.borrow_mut().push(row.upcast());
    }
}

/// Advice an edit returned, each line with a Copy button when it names a command.
fn notes_group(notes: &[Line], root: &gtk::Widget) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::new();
    g.set_widget_name("settings.notes");
    for n in notes {
        let row = adw::ActionRow::builder()
            .title(n.shown().unwrap_or_default())
            .title_lines(0)
            .build();
        if let Some(command) = n.copy_text() {
            let copy = gtk::Button::builder()
                .label(UiString::Copy.text())
                .valign(gtk::Align::Center)
                .build();
            let root = root.clone();
            copy.connect_clicked(move |_| root.clipboard().set_text(command));
            row.add_suffix(&copy);
        }
        g.add(&row);
    }
    g
}

/// A button that puts `text` on the clipboard.
fn copy_button(root: &Prefs, text: String) -> gtk::Button {
    let copy = gtk::Button::builder()
        .label(UiString::Copy.text())
        .valign(gtk::Align::Center)
        .build();
    let root = root.clone();
    copy.connect_clicked(move |_| root.clipboard().set_text(&text));
    copy
}

fn entry(title: &str, id: &str, text: &str, running: bool) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title(title)
        .sensitive(!running)
        .build();
    row.set_widget_name(id);
    row.set_text(text);
    row
}

/// Run `f` with the row when its text is committed: Return, or the field losing focus
/// with a change in it. The `apply` signal is kept for the self-test's `activate`.
fn on_commit(row: &adw::EntryRow, f: impl Fn(&adw::EntryRow) + 'static) {
    let f = Rc::new(f);
    let last = Rc::new(RefCell::new(row.text().to_string()));
    let commit = {
        let (f, last) = (f.clone(), last.clone());
        move |r: &adw::EntryRow| {
            if *last.borrow() != r.text().as_str() {
                *last.borrow_mut() = r.text().to_string();
                f(r);
            }
        }
    };
    row.connect_entry_activated(commit.clone());
    let focus = gtk::EventControllerFocus::new();
    let weak = row.downgrade();
    focus.connect_leave(move |_| {
        if let Some(r) = weak.upgrade() {
            commit(&r);
        }
    });
    row.add_controller(focus);
    row.connect_apply(move |r| f(r));
}

/// The shared folders, each with the commands that mount it in IRIX, and the button that adds one.
#[allow(clippy::too_many_arguments)]
fn shares_group(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    dialog: &Prefs,
    page: &adw::PreferencesPage,
    network: &adw::PreferencesPage,
    shares: &[origami_core::facade::ShareSetting],
    running: bool,
) {
    let g = adw::PreferencesGroup::builder()
        .title("Shared folders")
        .description(UiString::RunTheseAsRootIn.text())
        .build();
    for sh in shares {
        let r = value_row(&sh.name, &sh.host_dir);
        let b = gtk::Button::builder()
            .icon_name("list-remove-symbolic")
            .tooltip_text(UiString::StopSharing.text())
            .valign(gtk::Align::Center)
            .sensitive(!running)
            .css_classes(["flat"])
            .build();
        b.update_property(&[gtk::accessible::Property::Label(&format!(
            "Stop sharing {}",
            sh.name
        ))]);
        let (app, parent, m, dlg, name) = (
            app.clone(),
            parent.clone(),
            m.clone(),
            dialog.clone(),
            sh.name.clone(),
        );
        b.connect_clicked(move |_| {
            apply(&app, &parent, &m, &dlg, Change::RemoveShare(name.clone()))
        });
        r.add_suffix(&b);
        g.add(&r);
        let mount = adw::ActionRow::builder().title_lines(0).build();
        mount.set_widget_name(&format!("share.mount.{}", sh.name));
        match &sh.mount_problem {
            Some(problem) => {
                mount.set_title(problem);
                if sh.needs_address {
                    let go = gtk::Button::builder()
                        .label(UiString::SetTheAddressInNetwork.text())
                        .valign(gtk::Align::Center)
                        .build();
                    let (dialog, network) = (dialog.clone(), network.clone());
                    go.connect_clicked(move |_| dialog.set_visible_page(&network));
                    mount.add_suffix(&go);
                }
            }
            None => {
                let lines = sh.mount_lines.join("\n");
                mount.set_title(&lines);
                mount.add_css_class("monospace");
                mount.add_suffix(&copy_button(dialog, lines));
            }
        }
        g.add(&mount);
    }
    let go = {
        let (app, parent, m, dlg) = (app.clone(), parent.clone(), m.clone(), dialog.clone());
        move || share_folder(&app, &parent, &m, &dlg)
    };
    if shares.is_empty() {
        // Nothing shared yet: the core's empty state, with its button.
        let e = empty_state(EmptyKind::NoSharedFolders);
        let button = gtk::Button::builder()
            .label(e.button.unwrap_or_default())
            .halign(gtk::Align::Center)
            .sensitive(!running)
            .css_classes(["suggested-action", "pill"])
            .build();
        button.set_widget_name("share.add-folder");
        button.connect_clicked(move |_| go());
        g.add(
            &adw::StatusPage::builder()
                .title(e.title)
                .description(e.sentence)
                .child(&button)
                .css_classes(["compact"])
                .build(),
        );
    } else {
        let add = adw::ActionRow::builder()
            .title(UiString::ShareAFolder.text())
            .activatable(!running)
            .sensitive(!running)
            .build();
        add.set_widget_name("share.add-folder");
        add.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
        add.connect_activated(move |_| go());
        g.add(&add);
    }
    page.add(&g);
}

/// Mode, the Bridged fields, the Ethernet address and the private subnet.
fn network_groups(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    dialog: &Prefs,
    page: &adw::PreferencesPage,
    n: &NetworkSetting,
    running: bool,
) {
    let choices = origami_core::facade::host_network_modes();
    let setup = origami_core::facade::bridged_setup();
    let is_bridged = |mode: NetworkMode| matches!(mode, NetworkMode::Tap | NetworkMode::Vmnet);
    let current = choices.iter().position(|c| c.mode.as_str() == n.mode);
    let host_mode = choices.iter().map(|c| c.mode).find(|m| is_bridged(*m));
    let g = adw::PreferencesGroup::builder().title("Network").build();
    // One radio row per mode, each with its caption; choosing applies at once.
    let mut radios: Vec<(usize, gtk::CheckButton)> = Vec::new();
    let mut first: Option<gtk::CheckButton> = None;
    for (i, c) in choices.iter().enumerate() {
        let radio = gtk::CheckButton::builder()
            .valign(gtk::Align::Center)
            .build();
        radio.set_widget_name(&format!("settings.network.{}", c.mode.as_str()));
        match &first {
            Some(f) => radio.set_group(Some(f)),
            None => first = Some(radio.clone()),
        }
        // A mode this computer cannot run leaves nothing selected, so any choice is a change.
        radio.set_active(current == Some(i));
        let row = adw::ActionRow::builder()
            .title(&c.label)
            .subtitle(&c.detail)
            .activatable_widget(&radio)
            .sensitive(!running)
            .build();
        row.set_widget_name(&format!("settings.network-row.{}", c.mode.as_str()));
        row.add_prefix(&radio);
        g.add(&row);
        radios.push((i, radio));
    }
    if current.is_none() {
        g.set_description(Some(&format!("Currently {}", n.mode)));
    }
    page.add(&g);

    // Bridged shows its fields before anything is saved: the core needs the device with the mode.
    let host = adw::PreferencesGroup::builder()
        .title(UiString::Bridged.text())
        .visible(current.is_some_and(|i| is_bridged(choices[i].mode)))
        .build();
    let device = entry(
        &format!("{} ({})", setup.field, setup.example),
        "network.device",
        &n.device,
        running,
    );
    let share = entry(
        "Address for shared folders (ADDRESS/PREFIX)",
        "network.share-address",
        n.share_address.as_deref().unwrap_or_default(),
        running,
    );
    let help = adw::ExpanderRow::builder()
        .title(UiString::HowToSetThisUp.text())
        .build();
    help.add_row(
        &adw::ActionRow::builder()
            .title(&setup.steps)
            .title_lines(0)
            .build(),
    );
    if let Some(commands) = setup.commands.clone() {
        help.add_suffix(&copy_button(dialog, commands));
    }
    host.add(&device);
    host.add(&share);
    host.add(&help);
    for w in &n.warnings {
        host.add(&adw::ActionRow::builder().title(w).title_lines(0).build());
    }
    page.add(&host);

    if let Some(host_mode) = host_mode {
        let send = {
            let (app, parent, m, d, device, share) = (
                app.clone(),
                parent.clone(),
                m.clone(),
                dialog.clone(),
                device.clone(),
                share.clone(),
            );
            move |_: &adw::EntryRow| {
                let (device, share) = (device.text().to_string(), share.text().to_string());
                apply_network(
                    &app,
                    &parent,
                    &m,
                    &d,
                    (host_mode, device, Some(share), None),
                );
            }
        };
        on_commit(&device, send.clone());
        on_commit(&share, send);
    }
    for (i, radio) in &radios {
        let (app, parent, m, d, host) = (
            app.clone(),
            parent.clone(),
            m.clone(),
            dialog.clone(),
            host.clone(),
        );
        let (choice, same) = (choices[*i].clone(), current == Some(*i));
        radio.connect_toggled(move |r| {
            if !r.is_active() {
                return;
            }
            // Choosing the mode the machine has leaves the fields as the file shows them.
            host.set_visible(is_bridged(choice.mode));
            if !same && !is_bridged(choice.mode) {
                apply_network(
                    &app,
                    &parent,
                    &m,
                    &d,
                    (choice.mode, String::new(), None, None),
                );
            }
        });
    }

    let id = adw::PreferencesGroup::new();
    let address = value_row(
        UiString::EthernetAddress.text(),
        &format!("{}\n{}", n.mac.as_deref().unwrap_or("None"), n.mac_note),
    );
    address.set_widget_name("network.mac");
    if let Some(mac) = n.mac.clone() {
        address.add_suffix(&copy_button(dialog, mac));
    }
    id.add(&address);
    page.add(&id);

    if let Some(mode) = current
        .map(|i| choices[i].mode)
        .filter(|m| matches!(m, NetworkMode::User | NetworkMode::Private))
    {
        let adv = adw::ExpanderRow::builder()
            .title(UiString::Advanced.text())
            .build();
        let subnet = entry(
            "Private subnet (a /24)",
            "network.subnet",
            &n.subnet,
            running,
        );
        let (app, parent, m, d) = (app.clone(), parent.clone(), m.clone(), dialog.clone());
        on_commit(&subnet, move |row| {
            let subnet = Some(row.text().to_string());
            apply_network(&app, &parent, &m, &d, (mode, String::new(), None, subnet));
        });
        adv.add_row(&subnet);
        let ag = adw::PreferencesGroup::new();
        ag.add(&adv);
        page.add(&ag);
    }
}

/// Change and Eject for a CD-ROM row. They work while the machine runs, too.
fn cd_buttons(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    dialog: &Prefs,
    row: &adw::ActionRow,
    drive: &str,
    has_disc: bool,
) {
    for (icon, tip, change) in [
        ("media-optical-symbolic", UiString::ChangeCd.text(), true),
        ("media-eject-symbolic", UiString::Eject.text(), false),
    ] {
        if !change && !has_disc {
            continue;
        }
        let b = gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tip)
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        b.set_widget_name(&format!(
            "drive.cd.{}",
            if change { "change" } else { "eject" }
        ));
        b.update_property(&[gtk::accessible::Property::Label(&format!("{tip} {drive}"))]);
        let (app, parent, m, dlg, drive) = (
            app.clone(),
            parent.clone(),
            m.clone(),
            dialog.clone(),
            drive.to_string(),
        );
        b.connect_clicked(move |_| {
            let (app, parent, m, dlg, drive) = (
                app.clone(),
                parent.clone(),
                m.clone(),
                dlg.clone(),
                drive.clone(),
            );
            if change {
                disks::pick_path(&parent.clone(), "Choose a CD-ROM image", move |path| {
                    media(&app, &parent, &m, &dlg, &drive, Some(path))
                });
            } else {
                media(&app, &parent, &m, &dlg, &drive, None);
            }
        });
        row.add_suffix(&b);
    }
}

fn media(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    dialog: &Prefs,
    drive: &str,
    image: Option<std::path::PathBuf>,
) {
    let (machine, name) = (m.machine.clone(), drive.to_string());
    let (app, parent, m, dialog) = (app.clone(), parent.clone(), m.clone(), dialog.clone());
    let (drive, shown) = (
        drive.to_string(),
        image
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
    );
    glib::spawn_future_local(async move {
        let r = gtk::gio::spawn_blocking(move || {
            machine
                .change_media(&name, image.as_deref())
                .map_err(|e| e.to_string())
        })
        .await;
        match r {
            Ok(Ok(())) => {
                // A running machine's swap is not in its file: remember it for the Drives list.
                if m.state.get().is_up() {
                    m.media.borrow_mut().insert(drive, shown.clone());
                }
                if shown.is_none() {
                    window::toast_for(&app, &origami_core::facade::ToastKind::DiscEjected, || {});
                }
                reopen(&app, &parent, &m, &dialog, vec![])
            }
            Ok(Err(e)) => {
                eprintln!("origami-gtk: CD change refused: {e}");
                dialog.add_toast(adw::Toast::new(&e))
            }
            Err(_) => dialog.add_toast(adw::Toast::new("the CD could not be changed")),
        }
    });
}

/// A small dialog with `content`, a header button and the progress of a long edit.
fn edit_dialog(
    title: &str,
    action: &str,
    content: &[&adw::PreferencesGroup],
) -> (
    adw::Dialog,
    gtk::Button,
    gtk::ProgressBar,
    gtk::Label,
    gtk::Button,
    gtk::Label,
) {
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(460)
        .build();
    let page = adw::PreferencesPage::new();
    for g in content {
        page.add(*g);
    }
    let progress = adw::PreferencesGroup::new();
    let bar = gtk::ProgressBar::builder().visible(false).build();
    let line = gtk::Label::builder().visible(false).build();
    let stop = gtk::Button::builder()
        .label(UiString::Cancel.text())
        .halign(gtk::Align::End)
        .visible(false)
        .build();
    let err = gtk::Label::builder()
        .wrap(true)
        .css_classes(["error"])
        .visible(false)
        .xalign(0.0)
        .build();
    for w in [
        bar.upcast_ref::<gtk::Widget>(),
        line.upcast_ref(),
        stop.upcast_ref(),
        err.upcast_ref(),
    ] {
        progress.add(w);
    }
    page.add(&progress);
    let go = gtk::Button::builder()
        .label(action)
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let header = adw::HeaderBar::new();
    header.pack_end(&go);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));
    (dialog, go, bar, line, stop, err)
}

/// Add a disk image to the machine, copied into its folder or used in place.
fn add_image(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    settings: &Prefs,
    target: u32,
) {
    let go_ok = Rc::new(std::cell::RefCell::new(None::<gtk::Button>));
    let form = {
        let go_ok = go_ok.clone();
        disks::ImageForm::new(app, Some(m.machine.key().to_string()), move |ok| {
            if let Some(go) = go_ok.borrow().as_ref() {
                go.set_sensitive(ok);
            }
        })
    };
    let (dialog, go, bar, line, stop, err) =
        edit_dialog("Add a disk image", "Add disk", &[&form.group]);
    *go_ok.borrow_mut() = Some(go.clone());
    form.choose(parent);
    let present_on = settings.clone();
    let (app, parent, m, settings) = (app.clone(), parent.clone(), m.clone(), settings.clone());
    go.connect_clicked({
        let (dialog, go) = (dialog.clone(), go.clone());
        move |_| {
            let Some((file, copy, overlay)) = form.request() else {
                return;
            };
            go.set_sensitive(false);
            let request = origami_core::DriveRequest {
                file,
                kind: origami_core::DriveKind::Disk,
                target,
                overlay,
            };
            match m.machine.import_drive(request, copy) {
                Ok(op) => {
                    let (app, parent, m, settings, dialog, err, go) = (
                        app.clone(),
                        parent.clone(),
                        m.clone(),
                        settings.clone(),
                        dialog.clone(),
                        err.clone(),
                        go.clone(),
                    );
                    disks::follow(op, &bar, &line, &stop, move |state| match state {
                        OperationState::Failed(e) => {
                            err.set_label(&e.to_string());
                            err.set_visible(true);
                            go.set_sensitive(true);
                        }
                        _ => {
                            dialog.close();
                            reopen(&app, &parent, &m, &settings, vec![]);
                        }
                    });
                }
                Err(e) => {
                    err.set_label(&e.to_string());
                    err.set_visible(true);
                    go.set_sensitive(true);
                }
            }
        }
    });
    dialog.present(Some(&present_on.dialog));
}

fn add_forward(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    settings: &Prefs,
) {
    let dialog = adw::Dialog::builder()
        .title("Add a port forward")
        .content_width(420)
        .build();
    let page = adw::PreferencesPage::new();
    let g = adw::PreferencesGroup::new();
    let name = adw::EntryRow::builder()
        .title(UiString::Name.text())
        .build();
    let proto = adw::ComboRow::builder()
        .title("Protocol")
        .model(&gtk::StringList::new(&["tcp", "udp"]))
        .build();
    let host = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    host.set_title(UiString::HostPort.text());
    host.set_value(2222.0);
    let guest = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    guest.set_title(UiString::GuestPort.text());
    guest.set_value(22.0);
    for w in [
        name.upcast_ref::<gtk::Widget>(),
        proto.upcast_ref(),
        host.upcast_ref(),
        guest.upcast_ref(),
    ] {
        g.add(w);
    }
    page.add(&g);
    let add = gtk::Button::builder()
        .label(UiString::Add.text())
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    {
        let add = add.clone();
        name.connect_changed(move |e| add.set_sensitive(!e.text().trim().is_empty()));
    }
    let header = adw::HeaderBar::new();
    header.pack_end(&add);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));
    {
        let (app, parent, m, settings, dialog) = (
            app.clone(),
            parent.clone(),
            m.clone(),
            settings.clone(),
            dialog.clone(),
        );
        add.connect_clicked(move |_| {
            let forward = origami_machine::PortForward {
                name: name.text().trim().to_string(),
                protocol: if proto.selected() == 0 {
                    "tcp".into()
                } else {
                    "udp".into()
                },
                host_port: host.value() as u16,
                guest_port: guest.value() as u16,
            };
            dialog.close();
            apply(&app, &parent, &m, &settings, Change::AddForward(forward));
        });
    }
    dialog.present(Some(&settings.dialog));
}

fn share_folder(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    settings: &Prefs,
) {
    let (app, parent2, m, settings) = (app.clone(), parent.clone(), m.clone(), settings.clone());
    disks::pick_folder(parent, "Share a folder", move |path| {
        name_share(&app, &parent2, &m, &settings, path)
    });
}

/// Ask what IRIX will call a folder, checking the name as it is typed.
fn name_share(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    settings: &Prefs,
    dir: std::path::PathBuf,
) {
    let name = adw::EntryRow::builder()
        .title(UiString::NameInIrix.text())
        .build();
    name.set_widget_name("share.name");
    name.set_text(&origami_core::facade::suggest_share_name(
        &dir.display().to_string(),
    ));
    let folder = value_row(UiString::Folder.text(), &dir.display().to_string());
    let group = adw::PreferencesGroup::new();
    group.add(&name);
    group.add(&folder);
    let (dialog, go, _bar, _line, _stop, err) = edit_dialog("Share a folder", "Add", &[&group]);
    go.set_widget_name("share.add");
    let check = {
        let (name, go, err) = (name.clone(), go.clone(), err.clone());
        move || {
            let problem = origami_core::facade::check_share_name(&name.text()).err();
            err.set_label(&problem.as_ref().map(|e| e.to_string()).unwrap_or_default());
            err.set_visible(problem.is_some());
            go.set_sensitive(problem.is_none());
        }
    };
    check();
    name.connect_changed(move |_| check());
    let (app, window, m, settings) = (app.clone(), parent.clone(), m.clone(), settings.clone());
    go.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
            let change = Change::AddShare {
                name: name.text().to_string(),
                host_dir: dir.clone(),
            };
            apply(&app, &window, &m, &settings, change);
        }
    });
    dialog.present(Some(parent));
}

pub fn preferences(app: &Rc<App>, parent: &adw::ApplicationWindow) {
    let dialog = Prefs::new(UiString::Preferences.text());
    let general = adw::PreferencesPage::builder()
        .title(UiString::General.text())
        .icon_name("preferences-system-symbolic")
        .build();
    let folder = value_row(
        "Machines are kept in",
        &app.core.root().display().to_string(),
    );
    let choose = gtk::Button::builder()
        .label(UiString::Choose.text())
        .valign(gtk::Align::Center)
        .build();
    folder.add_suffix(&choose);
    {
        let (app, parent, folder) = (app.clone(), parent.clone(), folder.clone());
        choose.connect_clicked(move |_| {
            let (app, folder) = (app.clone(), folder.clone());
            gtk::FileDialog::builder()
                .title(UiString::ChooseTheFolderThatHoldsYourMachines.text())
                .build()
                .select_folder(Some(&parent), gtk::gio::Cancellable::NONE, move |r| {
                    if let Some(path) = r.ok().and_then(|f| f.path()) {
                        folder.set_subtitle(&path.display().to_string());
                        crate::app::set_library(&app, path);
                    }
                });
        });
    }
    general.add(&group("Library", vec![folder]));
    dialog.add(&general);

    let input_page = adw::PreferencesPage::builder()
        .title(UiString::Input.text())
        .icon_name("input-mouse-symbolic")
        .build();
    let g = adw::PreferencesGroup::builder()
        .title(UiString::Pointer.text())
        .description("The default for machines that have no setting of their own.")
        .build();
    let combo = adw::ComboRow::builder()
        .title("Default mode")
        .model(&gtk::StringList::new(&[
            "Follow my mouse",
            "Capture on click",
        ]))
        .build();
    combo.set_selected(if app.prefs.borrow().pointer == PointerMode::Follow {
        0
    } else {
        1
    });
    {
        let app = app.clone();
        combo.connect_selected_notify(move |c| {
            let mut p = app.prefs.borrow_mut();
            p.pointer = if c.selected() == 0 {
                PointerMode::Follow
            } else {
                PointerMode::Capture
            };
            p.save();
            drop(p);
            crate::window::default_pointer_changed(&app);
        });
    }
    g.add(&combo);
    input_page.add(&g);
    input_page.add(&group(
        "Release",
        vec![value_row("Release captured input", "Ctrl+Alt")],
    ));
    dialog.add(&input_page);

    let storage = adw::PreferencesPage::builder()
        .title(UiString::Storage.text())
        .icon_name("drive-harddisk-symbolic")
        .build();
    let downloaded = adw::PreferencesGroup::builder()
        .title(UiString::DownloadedDisks.text())
        .build();
    downloaded.set_widget_name("prefs.downloaded");
    storage.add(&downloaded);
    dialog.add(&storage);
    list_downloaded(app, &downloaded, Rc::new(RefCell::new(Vec::new())));
    dialog.present(Some(parent));
}

pub fn new_machine(app: &Rc<App>, parent: &adw::ApplicationWindow) {
    open_new_machine(app, parent, None);
}

/// New machine with this disk image already chosen, for one dropped on the library.
pub fn new_machine_from(app: &Rc<App>, parent: &adw::ApplicationWindow, image: std::path::PathBuf) {
    open_new_machine(app, parent, Some(image));
}

fn open_new_machine(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    image: Option<std::path::PathBuf>,
) {
    let (app, parent) = (app.clone(), parent.clone());
    glib::spawn_future_local(async move {
        // The first catalogue call may start the engine's program.
        let core = app.core.clone();
        let offerings =
            gtk::gio::spawn_blocking(move || core.catalogue().map_err(|e| e.to_string()))
                .await
                .unwrap_or_else(|_| Err("the catalogue could not be read".into()));
        match offerings {
            Ok(o) => new_machine_dialog(&app, &parent, o, image),
            Err(e) => {
                let d = adw::AlertDialog::new(Some("Cannot list the machines"), Some(&e));
                d.add_response("ok", "OK");
                d.present(Some(&parent));
            }
        }
    });
}

/// The offerings one engine lists; without an engine, all of them.
fn for_engine(
    offerings: &[origami_core::facade::OfferingSummary],
    engine: Option<&str>,
) -> Vec<origami_core::facade::OfferingSummary> {
    offerings
        .iter()
        .filter(|o| engine.is_none_or(|e| o.engine == e))
        .cloned()
        .collect()
}

fn new_machine_dialog(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    offerings: Vec<origami_core::facade::OfferingSummary>,
    image: Option<std::path::PathBuf>,
) {
    let dialog = adw::Dialog::builder()
        .title(UiString::NewMachine.text())
        .content_width(460)
        .content_height(880)
        .build();
    dialog.set_widget_name("new.dialog");
    let page = adw::PreferencesPage::new();
    let g = adw::PreferencesGroup::builder()
        .title(UiString::Machine.text())
        .build();
    // The engine comes first: it decides which models are offered, and the
    // machine keeps it for life.
    let known = app.core.engines();
    let engines: Vec<String> = known.iter().map(|e| e.id.clone()).collect();
    let engine = adw::ComboRow::builder()
        .title("Engine")
        .model(&gtk::StringList::new(
            &known.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ))
        .sensitive(engines.len() > 1)
        .build();
    let shown_offerings = Rc::new(RefCell::new(Vec::new()));
    let combo = adw::ComboRow::builder()
        .title(UiString::Model.text())
        .build();
    let fill = {
        let (offerings, engines, shown) =
            (offerings.clone(), engines.clone(), shown_offerings.clone());
        move |engine: &adw::ComboRow, combo: &adw::ComboRow| {
            let id = engines.get(engine.selected() as usize).map(String::as_str);
            let list = for_engine(&offerings, id);
            let labels: Vec<&str> = list.iter().map(|o| o.label.as_str()).collect();
            combo.set_model(Some(&gtk::StringList::new(&labels)));
            let at = origami_core::facade::default_offering(
                list.iter().map(|o| (o.id.as_str(), o.product.as_str())),
            );
            combo.set_selected(at.unwrap_or(0) as u32);
            *shown.borrow_mut() = list;
        }
    };
    fill(&engine, &combo);
    let offerings = shown_offerings;
    let name = adw::EntryRow::builder()
        .title(UiString::Name.text())
        .activates_default(true)
        .build();
    name.set_widget_name("new.name");
    let preview = gtk::Box::builder().halign(gtk::Align::Center).build();
    let show = {
        let (preview, offerings) = (preview.clone(), offerings.clone());
        move |combo: &adw::ComboRow| {
            let o = offerings.borrow().get(combo.selected() as usize).cloned();
            let o = o.as_ref();
            let id = o.and_then(|o| crate::art::of(&o.product, Some(&o.topology)));
            let card = id.and_then(|id| crate::art::framed(id, "card"));
            if let Some(old) = preview.first_child() {
                preview.remove(&old);
            }
            preview.set_visible(card.is_some());
            if let Some(card) = card {
                preview.append(&card);
            }
        }
    };
    show(&combo);
    combo.connect_selected_notify(show);
    {
        let combo = combo.clone();
        engine.connect_selected_notify(move |e| fill(e, &combo));
    }
    let shown = adw::PreferencesGroup::new();
    shown.add(&preview);
    page.add(&shown);
    g.add(&engine);
    g.add(&combo);
    g.add(&name);
    page.add(&g);

    // The disk: one radio row per way to get one, in the core's order and words.
    let disk_group = adw::PreferencesGroup::builder()
        .title(UiString::Disk.text())
        .build();
    disk_group.set_widget_name("new.disk-group");
    let labels = origami_core::facade::disk_labels();
    let choice = Rc::new(std::cell::Cell::new(DiskChoice::Install));
    let mut radios: Vec<(DiskChoice, adw::ActionRow, gtk::CheckButton)> = Vec::new();
    let mut first: Option<gtk::CheckButton> = None;
    for o in origami_core::facade::disk_options(true) {
        let id = match o.choice {
            DiskChoice::Ready => "ready",
            DiskChoice::Install => "install",
            DiskChoice::Image => "image",
            DiskChoice::NoDisk => "none",
        };
        let radio = gtk::CheckButton::builder()
            .valign(gtk::Align::Center)
            .build();
        radio.set_widget_name(&format!("new.disk.{id}"));
        match &first {
            Some(f) => radio.set_group(Some(f)),
            None => first = Some(radio.clone()),
        }
        let row = adw::ActionRow::builder()
            .title(&o.title)
            .subtitle(&o.caption)
            .activatable_widget(&radio)
            .build();
        row.set_widget_name(&format!("new.disk-row.{id}"));
        row.add_prefix(&radio);
        if o.recommended {
            row.add_suffix(
                &gtk::Label::builder()
                    .label(UiString::Recommended.text())
                    .css_classes(["caption", "dim-label"])
                    .build(),
            );
        }
        disk_group.add(&row);
        radios.push((o.choice, row, radio));
    }
    let row_of = |c: DiskChoice| {
        radios
            .iter()
            .find(|r| r.0 == c)
            .map(|r| r.1.clone())
            .unwrap()
    };
    let ready_row = row_of(DiskChoice::Ready);
    let install_row = row_of(DiskChoice::Install);
    ready_row.set_visible(false);
    let size = adw::SpinRow::with_range(1.0, 2048.0, 1.0);
    size.set_title(UiString::DiskSize.text());
    size.add_suffix(
        &gtk::Label::builder()
            .label(UiString::Gb.text())
            .css_classes(["dim-label"])
            .build(),
    );
    size.set_value(f64::from(labels.default_gb));
    size.set_widget_name("new.size");
    size.set_visible(false);
    let choose = adw::ActionRow::builder()
        .title(UiString::ChooseAnImage.text())
        .activatable(true)
        .visible(false)
        .build();
    choose.set_widget_name("new.image");
    let release = adw::ComboRow::builder()
        .title(&labels.release)
        .use_subtitle(true)
        .visible(false)
        .build();
    release.set_widget_name("new.release");
    for w in [
        release.upcast_ref::<gtk::Widget>(),
        size.upcast_ref(),
        choose.upcast_ref(),
    ] {
        disk_group.add(w);
    }
    page.add(&disk_group);
    let image_ok = Rc::new(std::cell::Cell::new(false));
    let identity = IdentityForm::new();
    let create = gtk::Button::builder()
        .label(UiString::Create.text())
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    create.set_widget_name("new.create");
    let ready = {
        let (create, name, choice, image_ok, id_ok) = (
            create.clone(),
            name.clone(),
            choice.clone(),
            image_ok.clone(),
            identity.ok.clone(),
        );
        Rc::new(move || {
            let named = !name.text().trim().is_empty();
            create.set_sensitive(
                named && id_ok.get() && (choice.get() != DiskChoice::Image || image_ok.get()),
            );
        })
    };
    // The preview follows the model, the engine and what is typed in Advanced.
    let refresh = {
        let (app, identity, offerings, engine, combo, ready) = (
            app.clone(),
            identity.clone(),
            offerings.clone(),
            engine.clone(),
            combo.clone(),
            ready.clone(),
        );
        let engines = engines.clone();
        Rc::new(move || {
            let offering = offerings
                .borrow()
                .get(combo.selected() as usize)
                .map(|o| o.id.clone());
            if let Some(offering) = offering {
                identity.refresh(
                    &app,
                    offering,
                    engines.get(engine.selected() as usize).cloned(),
                    ready.clone(),
                );
            }
        })
    };
    identity.watch(refresh.clone());
    {
        let refresh = refresh.clone();
        combo.connect_selected_notify(move |_| refresh());
    }
    {
        let refresh = refresh.clone();
        engine.connect_selected_notify(move |_| refresh());
    }
    refresh();
    let form = {
        let (image_ok, ready) = (image_ok.clone(), ready.clone());
        disks::ImageForm::new(app, None, move |ok| {
            image_ok.set(ok);
            ready();
        })
    };
    form.group.set_visible(false);
    // Which rows the chosen way shows; the rows' own radios set it.
    let sync = {
        let (choice, release, size, choose, form, ready) = (
            choice.clone(),
            release.clone(),
            size.clone(),
            choose.clone(),
            form.clone(),
            ready.clone(),
        );
        Rc::new(move || {
            let c = choice.get();
            release.set_visible(c == DiskChoice::Ready);
            size.set_visible(c == DiskChoice::Install);
            choose.set_visible(c == DiskChoice::Image);
            form.group.set_visible(c == DiskChoice::Image);
            ready();
        })
    };
    // Whether the person has picked a way, so a ready-made disk arriving late does not override it.
    let touched = Rc::new(std::cell::Cell::new(false));
    let auto = Rc::new(std::cell::Cell::new(false));
    for (c, _, radio) in &radios {
        let (choice, sync, c, touched, auto) = (
            choice.clone(),
            sync.clone(),
            *c,
            touched.clone(),
            auto.clone(),
        );
        radio.connect_toggled(move |r| {
            if r.is_active() {
                choice.set(c);
                if !auto.get() {
                    touched.set(true);
                }
                sync();
            }
        });
    }
    // The first way offered is the default.
    let set_default = {
        let (radios, auto) = (radios.clone(), auto.clone());
        Rc::new(move || {
            let at = radios
                .iter()
                .find(|(_, row, _)| row.is_visible())
                .map(|(_, _, r)| r.clone());
            auto.set(true);
            if let Some(r) = at {
                r.set_active(true);
            }
            auto.set(false);
        })
    };
    set_default();
    {
        let (form, parent) = (form.clone(), parent.clone());
        choose.connect_activated(move |_| form.choose(&parent));
    }
    // The disks that fit the chosen model, asked of the server off the UI thread.
    let ready_disks = Rc::new(RefCell::new(Vec::<origami_core::facade::ReadyDisk>::new()));
    let asked = Rc::new(std::cell::Cell::new(0u32));
    // The ready-made row says what the chosen release costs to get.
    let ready_caption = {
        let (ready_row, release, ready_disks) =
            (ready_row.clone(), release.clone(), ready_disks.clone());
        Rc::new(move || {
            let disks = ready_disks.borrow();
            if let Some(d) = disks.get(release.selected() as usize) {
                let base = origami_core::facade::disk_options(true)[0].caption.clone();
                let tail = origami_core::facade::ready_caption(d.bytes, d.cached);
                ready_row.set_subtitle(&format!("{base} {tail}"));
            }
        })
    };
    release.connect_selected_notify({
        let c = ready_caption.clone();
        move |_| c()
    });
    let refresh = {
        let (app, offerings, ready_disks, asked) = (
            app.clone(),
            offerings.clone(),
            ready_disks.clone(),
            asked.clone(),
        );
        let (release, ready_row, install_row, radios, choice, touched) = (
            release.clone(),
            ready_row.clone(),
            install_row.clone(),
            radios.clone(),
            choice.clone(),
            touched.clone(),
        );
        let (set_default, ready_caption) = (set_default.clone(), ready_caption.clone());
        move |combo: &adw::ComboRow| {
            let Some(offering) = offerings.borrow().get(combo.selected() as usize).cloned() else {
                return;
            };
            // Install IRIX applies only to machines that run IRIX.
            install_row.set_visible(offering.runs_irix);
            let id = offering.id;
            asked.set(asked.get() + 1);
            let (turn, core) = (asked.get(), app.core.clone());
            let (asked, ready_disks, release, ready_row, radios, choice, touched) = (
                asked.clone(),
                ready_disks.clone(),
                release.clone(),
                ready_row.clone(),
                radios.clone(),
                choice.clone(),
                touched.clone(),
            );
            let (set_default, ready_caption) = (set_default.clone(), ready_caption.clone());
            glib::spawn_future_local(async move {
                let found =
                    gtk::gio::spawn_blocking(move || core.ready_disks(&id).unwrap_or_default())
                        .await
                        .unwrap_or_default();
                // A later model choice has asked again.
                if turn != asked.get() {
                    return;
                }
                ready_row.set_visible(!found.is_empty());
                release.set_model(Some(&gtk::StringList::new(
                    &found.iter().map(|d| d.label.as_str()).collect::<Vec<_>>(),
                )));
                *ready_disks.borrow_mut() = found;
                ready_caption();
                let gone = radios
                    .iter()
                    .any(|(c, row, _)| *c == choice.get() && !row.is_visible());
                // A ready-made disk leads once it is offered, unless the person chose; a way that went away falls back.
                if gone || !touched.get() {
                    set_default();
                }
            });
        }
    };
    refresh(&combo);
    combo.connect_selected_notify(refresh);
    let err = gtk::Label::builder()
        .wrap(true)
        .css_classes(["error"])
        .visible(false)
        .xalign(0.0)
        .build();
    g.add(&err);
    page.add(&identity.group);
    page.add(&form.group);
    let progress = adw::PreferencesGroup::new();
    let bar = gtk::ProgressBar::builder().visible(false).build();
    let line = gtk::Label::builder().visible(false).build();
    let stop = gtk::Button::builder()
        .label(UiString::Cancel.text())
        .halign(gtk::Align::End)
        .visible(false)
        .build();
    for w in [
        bar.upcast_ref::<gtk::Widget>(),
        line.upcast_ref(),
        stop.upcast_ref(),
    ] {
        progress.add(w);
    }
    page.add(&progress);
    {
        let (ready, name) = (ready.clone(), name.clone());
        name.connect_changed(move |n| {
            n.remove_css_class("error");
            ready();
        });
    }
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    let cancel = gtk::Button::builder()
        .label(UiString::Cancel.text())
        .build();
    cancel.set_widget_name("new.cancel");
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    header.pack_start(&cancel);
    header.pack_end(&create);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));
    dialog.set_default_widget(Some(&create));
    dialog.set_focus(Some(&engine));
    {
        let engines = engines.clone();
        let (app, dialog, engine, combo, name, err) = (
            app.clone(),
            dialog.clone(),
            engine.clone(),
            combo.clone(),
            name.clone(),
            err.clone(),
        );
        let (choice, size, form, identity) =
            (choice.clone(), size.clone(), form.clone(), identity.clone());
        let (release, ready_disks) = (release.clone(), ready_disks.clone());
        let (bar, line, stop) = (bar.clone(), line.clone(), stop.clone());
        let parent = parent.clone();
        create.connect_clicked(move |b| {
            let Some(o) = offerings.borrow().get(combo.selected() as usize).cloned() else {
                return;
            };
            // The list loads in the background: a pick that no longer exists is refused, not made a bare machine.
            if choice.get() == DiskChoice::Ready
                && ready_disks
                    .borrow()
                    .get(release.selected() as usize)
                    .is_none()
            {
                err.set_label("Pick a ready-made disk again.");
                err.set_visible(true);
                return;
            }
            let request =
                CreateRequest {
                    emu_id: Some(identity.id.text().to_string()),
                    mac: identity.mac(),
                    name: name.text().trim().to_string(),
                    offering: o.id.clone(),
                    engine: engines.get(engine.selected() as usize).cloned(),
                    system_disk: match choice.get() {
                        DiskChoice::Install => Some(SystemDisk::Empty {
                            gb: size.value() as u32,
                        }),
                        DiskChoice::Image => {
                            form.request()
                                .map(|(file, copy, overlay)| SystemDisk::Image {
                                    file,
                                    copy,
                                    overlay,
                                })
                        }
                        DiskChoice::Ready => ready_disks
                            .borrow()
                            .get(release.selected() as usize)
                            .map(|d| SystemDisk::Ready {
                                name: d.name.clone(),
                            }),
                        DiskChoice::NoDisk => None,
                    },
                    ..Default::default()
                };
            b.set_sensitive(false);
            let (app, dialog, err, b, name) = (
                app.clone(),
                dialog.clone(),
                err.clone(),
                b.clone(),
                name.clone(),
            );
            let (install, parent) = (choice.get() == DiskChoice::Install, parent.clone());
            let fail = {
                let (err, b, name) = (err.clone(), b.clone(), name.clone());
                move |e: &origami_core::facade::CoreError| {
                    // A taken name belongs on the Name row; anything else under the group.
                    if e.message.starts_with("A machine named") {
                        name.add_css_class("error");
                    }
                    err.set_label(&e.message);
                    err.set_visible(true);
                    b.set_sensitive(true);
                }
            };
            match app.core.create_machine(request) {
                Ok(op) => {
                    let finished = op.clone();
                    disks::follow(op, &bar, &line, &stop, move |state| match state {
                        OperationState::Succeeded => {
                            app::rescan(&app);
                            dialog.close();
                            let opened = finished.result_machine();
                            if let Some(entry) = opened.and_then(|k| app::find_entry(&app, &k)) {
                                let m = app::open(&app, &entry);
                                if let Some(m) = m.filter(|_| install) {
                                    crate::install::open(&app, &parent, &m);
                                }
                            }
                        }
                        // A cancelled copy leaves the machine without a disk; a cancelled download made nothing.
                        OperationState::Cancelled if finished.result_machine().is_some() => {
                            app::rescan(&app);
                            dialog.close();
                        }
                        OperationState::Cancelled => b.set_sensitive(true),
                        OperationState::Failed(e) => fail(&e),
                        OperationState::Running => {}
                    });
                }
                Err(e) => fail(&e),
            }
        });
    }
    // A dropped image is answered through the picker's queue, then used as if chosen.
    if let Some(path) = image {
        if let Some((_, _, radio)) = radios.iter().find(|r| r.0 == DiskChoice::Image) {
            disks::queue_pick(path);
            radio.set_active(true);
            form.choose(parent);
        }
    }
    dialog.present(Some(parent));
}

/// New Machine's Advanced rows: the machine ID and the Ethernet address it makes.
#[derive(Clone)]
struct IdentityForm {
    group: adw::PreferencesGroup,
    id: adw::EntryRow,
    id_note: gtk::Label,
    shown: adw::ActionRow,
    /// From the machine ID, or Custom.
    address: adw::ComboRow,
    mac: adw::EntryRow,
    mac_note: gtk::Label,
    /// Whether the ID and address can be used, so Create may be offered.
    ok: Rc<std::cell::Cell<bool>>,
    /// Set while the form writes a row itself, so that is not read as typing.
    writing: Rc<std::cell::Cell<bool>>,
    /// The latest question put to the core; older answers are dropped.
    round: Rc<std::cell::Cell<u32>>,
}

impl IdentityForm {
    fn new() -> Self {
        let advanced = adw::ExpanderRow::builder()
            .title(UiString::Advanced.text())
            .build();
        advanced.set_widget_name("new.advanced");
        let id = adw::EntryRow::builder()
            .title(UiString::MachineId.text())
            .build();
        id.set_widget_name("new.id");
        let note = || {
            gtk::Label::builder()
                .wrap(true)
                .xalign(0.0)
                .margin_start(12)
                .margin_end(12)
                .margin_bottom(6)
                .css_classes(["caption", "dim-label"])
                .build()
        };
        let id_note = note();
        id_note.set_widget_name("new.id-note");
        let shown = adw::ActionRow::builder()
            .title(UiString::Address.text())
            .subtitle_selectable(true)
            .build();
        shown.set_widget_name("new.mac");
        let address = adw::ComboRow::builder()
            .title(UiString::EthernetAddress.text())
            .model(&gtk::StringList::new(&[
                UiString::FromTheMachineId.text(),
                UiString::Custom.text(),
            ]))
            .build();
        address.set_widget_name("new.mac-custom");
        let mac = adw::EntryRow::builder()
            .title("Address (08:00:69:__:__:__)")
            .visible(false)
            .build();
        mac.set_widget_name("new.mac-entry");
        let mac_note = note();
        mac_note.set_widget_name("new.mac-note");
        mac_note.set_visible(false);
        for row in [
            id.upcast_ref::<gtk::Widget>(),
            id_note.upcast_ref(),
            address.upcast_ref(),
            shown.upcast_ref(),
            mac.upcast_ref(),
            mac_note.upcast_ref(),
        ] {
            advanced.add_row(row);
        }
        // Opening Advanced brings its first field into view, so the rows below the fold are seen.
        {
            let id = id.clone();
            advanced.connect_expanded_notify(move |a| {
                if a.is_expanded() {
                    let id = id.clone();
                    // After the rows have revealed, or there is nothing to scroll to yet.
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(400),
                        move || {
                            id.grab_focus();
                        },
                    );
                }
            });
        }
        let group = adw::PreferencesGroup::new();
        group.add(&advanced);
        let this = Self {
            group,
            id,
            id_note,
            shown,
            address,
            mac,
            mac_note,
            ok: Rc::new(std::cell::Cell::new(true)),
            writing: Rc::new(std::cell::Cell::new(false)),
            round: Rc::new(std::cell::Cell::new(0)),
        };
        this.address.connect_selected_notify({
            let mac = this.mac.clone();
            move |c| mac.set_visible(c.selected() == 1)
        });
        this
    }

    fn custom(&self) -> bool {
        self.address.selected() == 1
    }

    fn watch(&self, refresh: Rc<dyn Fn()>) {
        let (writing, r) = (self.writing.clone(), refresh.clone());
        self.id.connect_changed(move |_| {
            if !writing.get() {
                r()
            }
        });
        let (writing, r) = (self.writing.clone(), refresh.clone());
        self.mac.connect_changed(move |_| {
            if !writing.get() {
                r()
            }
        });
        self.address.connect_selected_notify(move |_| refresh());
    }

    fn mac(&self) -> Option<String> {
        let typed = self.mac.text().trim().to_string();
        (self.custom() && !typed.is_empty()).then_some(typed)
    }

    fn refresh(
        &self,
        app: &Rc<App>,
        offering: String,
        engine: Option<String>,
        ready: Rc<dyn Fn()>,
    ) {
        let (core, id, mac) = (app.core.clone(), self.id.text().to_string(), self.mac());
        let round = self.round.get() + 1;
        self.round.set(round);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let asked = id.clone();
            let preview = gtk::gio::spawn_blocking(move || {
                core.preview_new_identity(&offering, engine.as_deref(), &id, mac.as_deref())
                    .map_err(|e| e.message)
            })
            .await;
            // A later change has asked again; its answer is the one to show.
            if this.round.get() != round {
                return;
            }
            let (shown, problem, field, next) = match preview {
                Ok(Ok(p)) => {
                    // The next free ID fills an empty field, never one the person has started typing.
                    if asked.trim().is_empty() && this.id.text().trim().is_empty() {
                        this.writing.set(true);
                        this.id.set_text(&p.emu_id.to_string());
                        this.writing.set(false);
                    }
                    if !p.from_id {
                        this.address.set_selected(1);
                    }
                    this.address.set_sensitive(p.from_id);
                    let shown = if p.mac.is_empty() {
                        "Set by the machine".into()
                    } else {
                        p.mac
                    };
                    (shown, p.problem, p.problem_field, Some(p.emu_id))
                }
                Ok(Err(e)) => (String::new(), Some(e), None, None),
                Err(_) => (
                    String::new(),
                    Some("The address could not be checked.".into()),
                    None,
                    None,
                ),
            };
            this.shown.set_subtitle(&shown);
            let on_id = problem.is_some() && field != Some(IdentityField::Address);
            let on_address = problem.is_some() && field == Some(IdentityField::Address);
            let set = |row: &adw::EntryRow, note: &gtk::Label, text: String, bad: bool| {
                row.remove_css_class("error");
                note.remove_css_class("error");
                note.remove_css_class("dim-label");
                if bad {
                    row.add_css_class("error");
                }
                note.add_css_class(if bad { "error" } else { "dim-label" });
                note.set_label(&text);
                note.set_visible(!text.is_empty());
            };
            let message = problem.clone().unwrap_or_default();
            let caption = next
                .map(origami_core::facade::machine_id_caption)
                .unwrap_or_default();
            set(
                &this.id,
                &this.id_note,
                if on_id { message.clone() } else { caption },
                on_id,
            );
            set(
                &this.mac,
                &this.mac_note,
                if on_address { message } else { String::new() },
                on_address,
            );
            this.ok.set(problem.is_none());
            ready();
        });
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use origami_core::facade::OfferingSummary;

    fn offering(id: &str, engine: &str) -> OfferingSummary {
        OfferingSummary {
            id: id.into(),
            label: id.into(),
            engine: engine.into(),
            product: String::new(),
            topology: String::new(),
            topology_name: String::new(),
            has_display: false,
            runs_irix: true,
            nodes: 1,
            cpus: 1,
            memory_default_mib: 0,
            memory_accepted_mib: Vec::new(),
        }
    }

    #[test]
    fn a_chosen_engine_offers_only_its_own_models() {
        let all = [offering("octane2", "qemu"), offering("indy", "iris")];
        let ids = |e| {
            for_engine(&all, e)
                .into_iter()
                .map(|o| o.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(Some("iris")), ["indy"]);
        assert_eq!(ids(Some("qemu")), ["octane2"]);
        assert_eq!(ids(None), ["octane2", "indy"]);
    }
}
