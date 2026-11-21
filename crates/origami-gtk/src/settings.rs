//! Settings for one machine, Preferences for the app, and the New Machine
//! sheet. Machine settings read the machine file; changing them on a stopped
//! machine goes through the core's change operations, which this app does not
//! have yet, so the panes show values and say so instead of faking an edit.
//! Pointer mode is ours and is editable.

use crate::app::{self, App};
use crate::disks;
use crate::library;
use crate::machine::MachineUi;
use crate::prefs::PointerMode;
use crate::{input, window};
use adw::prelude::*;
use gtk::glib;
use origami_core::facade::{CreateRequest, OperationState, SystemDisk};
use origami_core::{Change, NetworkChange, NetworkMode};
use std::cell::RefCell;
use std::rc::Rc;

fn value_row(title: &str, value: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .subtitle_selectable(true)
        .build();
    r.add_css_class("property");
    r
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
        file,
    })
}

pub fn present_machine(app: &Rc<App>, parent: &adw::ApplicationWindow, m: &Rc<MachineUi>) {
    let (app, parent, m) = (app.clone(), parent.clone(), m.clone());
    glib::spawn_future_local(async move {
        let (machine, dir) = (m.machine.clone(), m.dir.clone());
        let loaded = gtk::gio::spawn_blocking(move || {
            // The core reads the machine through the engine, which may start it.
            load(&machine, &dir)
        })
        .await
        .unwrap_or_else(|_| Err("the settings could not be read".into()));
        build_machine_dialog(&app, &parent, &m, loaded);
    });
}

/// Apply one edit to the stopped machine on a worker thread, then show the
/// result: the dialog is rebuilt from the file so it never shows a guess.
fn apply(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    dialog: &adw::PreferencesDialog,
    change: Change,
) {
    let machine = m.machine.clone();
    let (app, parent, m, dialog) = (app.clone(), parent.clone(), m.clone(), dialog.clone());
    glib::spawn_future_local(async move {
        let r = gtk::gio::spawn_blocking(move || {
            machine.apply_change(&change).map_err(|e| e.to_string())
        })
        .await;
        match r {
            Ok(Ok(_)) => reopen(&app, &parent, &m, &dialog),
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
    dialog: &adw::PreferencesDialog,
) {
    *m.entry.borrow_mut() = library::read(&m.dir);
    crate::window::rebuild_hero(m);
    crate::window::refresh(app);
    dialog.close();
    present_machine(app, parent, m);
}

fn build_machine_dialog(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    loaded: Result<Loaded, String>,
) {
    let dialog = adw::PreferencesDialog::builder()
        .title(format!("{} Settings", m.name))
        .build();
    dialog.set_widget_name("settings.dialog");
    let running = m.state.get().is_up();

    let general = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();
    if running {
        general.add(&adw::PreferencesGroup::builder().description("This machine is running. Hardware settings are locked until it is powered off.").build());
    }
    general.add(&group(
        "Machine",
        vec![
            value_row("Name", &m.name),
            value_row("Location", &m.dir.display().to_string()),
            value_row("Model", &m.entry.borrow().model),
        ],
    ));
    dialog.add(&general);

    match &loaded {
        Ok(l) => {
            let f = &l.file;
            let hw = adw::PreferencesPage::builder()
                .title("Processor & Memory")
                .icon_name("system-run-symbolic")
                .build();
            let g = adw::PreferencesGroup::builder()
                .title("Processor and memory")
                .build();
            if let Some(p) = &f.machine.processor {
                g.add(&value_row("Processor", p));
            }
            let cpus = adw::ComboRow::builder()
                .title("Processors per node")
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
                .title("Memory per node")
                .sensitive(!running)
                .build();
            let labels: Vec<String> = l.memory.iter().map(|v| format!("{v} MiB")).collect();
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
            g.add(&value_row("Nodes", &f.machine.nodes.to_string()));
            hw.add(&g);
            dialog.add(&hw);

            let gfx = adw::PreferencesPage::builder()
                .title("Graphics")
                .icon_name("video-display-symbolic")
                .build();
            let rows: Vec<_> = if f.pipe.is_empty() {
                vec![value_row("Graphics", "None")]
            } else {
                f.pipe
                    .iter()
                    .map(|p| {
                        value_row(
                            &format!("Pipe in slot {}", p.slot),
                            &format!("{} ({})", p.product, p.family),
                        )
                    })
                    .collect()
            };
            gfx.add(&group("Graphics pipes", rows));
            dialog.add(&gfx);

            let drives = adw::PreferencesPage::builder()
                .title("Drives")
                .icon_name("drive-harddisk-symbolic")
                .build();
            let g = adw::PreferencesGroup::builder().title("Drives").build();
            if f.drive.is_empty() {
                g.add(&value_row("Drives", "None"));
            }
            for d in &f.drive {
                let r = value_row(
                    &format!("{} ({}, bus {} target {})", d.name, d.kind, d.bus, d.target),
                    &format!(
                        "{}{}",
                        d.image,
                        if d.read_only { " (read-only)" } else { "" }
                    ),
                );
                if d.kind == "disk" {
                    let badge = disks::badge(&format!("disk.badge.{}", d.name));
                    let (machine, name) = (m.machine.clone(), d.name.clone());
                    disks::fill_badge(&badge, move || {
                        machine.drive_info(&name).map_err(|e| e.to_string())
                    });
                    r.add_suffix(&badge);
                }
                if d.kind == "cdrom" {
                    cd_buttons(app, parent, m, &dialog, &r, &d.name);
                }
                let b = gtk::Button::builder()
                    .icon_name("list-remove-symbolic")
                    .tooltip_text("Detach")
                    .valign(gtk::Align::Center)
                    .sensitive(!running)
                    .css_classes(["flat"])
                    .build();
                b.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Detach {}",
                    d.name
                ))]);
                let (app, parent, m, dlg, name) = (
                    app.clone(),
                    parent.clone(),
                    m.clone(),
                    dialog.clone(),
                    d.name.clone(),
                );
                b.connect_clicked(move |_| {
                    apply(&app, &parent, &m, &dlg, Change::DetachDrive(name.clone()))
                });
                r.add_suffix(&b);
                g.add(&r);
            }
            let mut adds = vec![(
                "drive.add-image",
                "Add a disk image…",
                l.disk_target.filter(|_| !running),
            )];
            if !f.drive.iter().any(|d| d.kind == "cdrom") {
                adds.push((
                    "drive.insert-cd",
                    "Insert a CD-ROM image…",
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
                        disks::pick_path(&parent.clone(), "Choose a CD-ROM Image", move |path| {
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
            let g = adw::PreferencesGroup::builder().title("Network").build();
            let modes = [
                (NetworkMode::User, "user"),
                (NetworkMode::Private, "private"),
                (NetworkMode::None, "none"),
            ];
            let mode = adw::ComboRow::builder()
                .title("Mode")
                .sensitive(!running)
                .build();
            mode.set_model(Some(&gtk::StringList::new(&[
                "Shared (NAT)",
                "Private",
                "Off",
            ])));
            let at = modes.iter().position(|(_, n)| *n == f.network.mode);
            match at {
                Some(i) => mode.set_selected(i as u32),
                None => {
                    // tap and vmnet need an endpoint this sheet does not ask for.
                    mode.set_sensitive(false);
                    mode.set_subtitle(&format!("Currently {}", f.network.mode));
                }
            }
            let start = mode.selected();
            {
                let (app, parent, m, d) = (app.clone(), parent.clone(), m.clone(), dialog.clone());
                mode.connect_selected_notify(move |c| {
                    if c.selected() != start {
                        if let Some((mode, _)) = modes.get(c.selected() as usize) {
                            let change = NetworkChange {
                                mode: *mode,
                                endpoint: None,
                                interface: None,
                                share_address: None,
                                subnet: None,
                                mac: None,
                            };
                            apply(&app, &parent, &m, &d, Change::SetNetwork(change));
                        }
                    }
                });
            }
            g.add(&mode);
            net.add(&g);
            let fg = adw::PreferencesGroup::builder()
                .title("Port forwards")
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
                    .tooltip_text("Remove")
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
                .title("Add a forward…")
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
            net.add(&fg);
            let sg = adw::PreferencesGroup::builder()
                .title("Shared folders")
                .build();
            for sh in &f.share {
                let r = value_row(&sh.name, &sh.path.display().to_string());
                let b = gtk::Button::builder()
                    .icon_name("list-remove-symbolic")
                    .tooltip_text("Stop sharing")
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
                sg.add(&r);
            }
            let add = adw::ActionRow::builder()
                .title("Share a folder…")
                .activatable(!running)
                .sensitive(!running)
                .build();
            add.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
            {
                let (app, parent, m, dlg) =
                    (app.clone(), parent.clone(), m.clone(), dialog.clone());
                add.connect_activated(move |_| share_folder(&app, &parent, &m, &dlg));
            }
            sg.add(&add);
            net.add(&sg);
            dialog.add(&net);
            let startup = adw::PreferencesPage::builder()
                .title("Startup & Firmware")
                .icon_name("system-shutdown-symbolic")
                .build();
            let g = adw::PreferencesGroup::builder()
                .title("Startup and firmware")
                .description("The Console window and a headless machine's tab use this line. On a machine with graphics, the PROM may print on the screen instead.")
                .build();
            let line = adw::ComboRow::builder()
                .title("Console serial line")
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
        .title("Input")
        .icon_name("input-mouse-symbolic")
        .build();
    let g = adw::PreferencesGroup::builder()
        .title("Pointer")
        .description(
            "How the mouse reaches the guest. This machine overrides the default in Preferences.",
        )
        .build();
    let combo = adw::ComboRow::builder()
        .title("Mode")
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
    core: &origami_core::facade::Core,
    group: &adw::PreferencesGroup,
    rows: Rc<RefCell<Vec<adw::ActionRow>>>,
) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }
    let found = core.cached_disks().unwrap_or_default();
    if found.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No downloaded disks")
            .build();
        group.add(&row);
        rows.borrow_mut().push(row);
    }
    for disk in found {
        let row = adw::ActionRow::builder()
            .title(&disk.label)
            .subtitle(origami_core::facade::size_label(disk.bytes))
            .build();
        let remove = gtk::Button::builder()
            .label("Remove")
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&remove);
        let (again, group_again, rows_again, key) =
            (core.clone(), group.clone(), rows.clone(), disk.key);
        remove.connect_clicked(move |_| {
            let _ = again.remove_cached_disk(&key);
            list_downloaded(&again, &group_again, rows_again.clone());
        });
        group.add(&row);
        rows.borrow_mut().push(row);
    }
}

/// Change and Eject for a CD-ROM row. They work while the machine runs, too.
fn cd_buttons(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    dialog: &adw::PreferencesDialog,
    row: &adw::ActionRow,
    drive: &str,
) {
    for (icon, tip, change) in [
        ("media-optical-symbolic", "Change CD", true),
        ("media-eject-symbolic", "Eject CD", false),
    ] {
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
                disks::pick_path(&parent.clone(), "Choose a CD-ROM Image", move |path| {
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
    dialog: &adw::PreferencesDialog,
    drive: &str,
    image: Option<std::path::PathBuf>,
) {
    let (machine, name) = (m.machine.clone(), drive.to_string());
    let (app, parent, m, dialog) = (app.clone(), parent.clone(), m.clone(), dialog.clone());
    glib::spawn_future_local(async move {
        let r = gtk::gio::spawn_blocking(move || {
            machine
                .change_media(&name, image.as_deref())
                .map_err(|e| e.to_string())
        })
        .await;
        match r {
            Ok(Ok(())) => reopen(&app, &parent, &m, &dialog),
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
        .label("Cancel")
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
    settings: &adw::PreferencesDialog,
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
        edit_dialog("Add a Disk Image", "Add Disk", &[&form.group]);
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
                            reopen(&app, &parent, &m, &settings);
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
    dialog.present(Some(&present_on));
}

fn add_forward(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    settings: &adw::PreferencesDialog,
) {
    let dialog = adw::Dialog::builder()
        .title("Add a Port Forward")
        .content_width(420)
        .build();
    let page = adw::PreferencesPage::new();
    let g = adw::PreferencesGroup::new();
    let name = adw::EntryRow::builder().title("Name").build();
    let proto = adw::ComboRow::builder()
        .title("Protocol")
        .model(&gtk::StringList::new(&["tcp", "udp"]))
        .build();
    let host = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    host.set_title("Host port");
    host.set_value(2222.0);
    let guest = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    guest.set_title("Guest port");
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
        .label("Add")
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
    dialog.present(Some(settings));
}

fn share_folder(
    app: &Rc<App>,
    parent: &adw::ApplicationWindow,
    m: &Rc<MachineUi>,
    settings: &adw::PreferencesDialog,
) {
    let chooser = gtk::FileDialog::builder().title("Share a Folder").build();
    let (app, parent, m, settings2) = (app.clone(), parent.clone(), m.clone(), settings.clone());
    chooser.select_folder(
        Some(&parent.clone()),
        gtk::gio::Cancellable::NONE,
        move |r| {
            if let Ok(file) = r {
                if let Some(path) = file.path() {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "share".into());
                    apply(
                        &app,
                        &parent,
                        &m,
                        &settings2,
                        Change::AddShare {
                            name,
                            host_dir: path,
                        },
                    );
                }
            }
        },
    );
}

pub fn preferences(app: &Rc<App>, parent: &adw::ApplicationWindow) {
    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
        .build();
    let general = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();
    let folder = value_row(
        "Machines are kept in",
        &app.core.root().display().to_string(),
    );
    let choose = gtk::Button::builder()
        .label("Choose…")
        .valign(gtk::Align::Center)
        .build();
    folder.add_suffix(&choose);
    {
        let (app, parent, folder) = (app.clone(), parent.clone(), folder.clone());
        choose.connect_clicked(move |_| {
            let (app, folder) = (app.clone(), folder.clone());
            gtk::FileDialog::builder()
                .title("Choose the library folder")
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
        .title("Input")
        .icon_name("input-mouse-symbolic")
        .build();
    let g = adw::PreferencesGroup::builder()
        .title("Pointer")
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
        .title("Storage")
        .icon_name("drive-harddisk-symbolic")
        .build();
    let downloaded = adw::PreferencesGroup::builder()
        .title("Downloaded disks")
        .build();
    downloaded.set_widget_name("prefs.downloaded");
    storage.add(&downloaded);
    dialog.add(&storage);
    list_downloaded(&app.core, &downloaded, Rc::new(RefCell::new(Vec::new())));
    dialog.present(Some(parent));
}

pub fn new_machine(app: &Rc<App>, parent: &adw::ApplicationWindow) {
    let (app, parent) = (app.clone(), parent.clone());
    glib::spawn_future_local(async move {
        // The first catalogue call may start the engine's program.
        let core = app.core.clone();
        let offerings =
            gtk::gio::spawn_blocking(move || core.catalogue().map_err(|e| e.to_string()))
                .await
                .unwrap_or_else(|_| Err("the catalogue could not be read".into()));
        match offerings {
            Ok(o) => new_machine_dialog(&app, &parent, o),
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
) {
    let dialog = adw::Dialog::builder()
        .title("New Machine")
        .content_width(460)
        .build();
    let page = adw::PreferencesPage::new();
    let g = adw::PreferencesGroup::new();
    // The engine comes first: it decides which models are offered, and the
    // machine keeps it for life.
    let engines: Vec<String> = app.core.engines().into_iter().map(|e| e.id).collect();
    let engine = adw::ComboRow::builder()
        .title("Engine")
        .model(&gtk::StringList::new(
            &engines.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .sensitive(engines.len() > 1)
        .build();
    let shown_offerings = Rc::new(RefCell::new(Vec::new()));
    let combo = adw::ComboRow::builder().title("Model").build();
    let fill = {
        let (offerings, engines, shown) =
            (offerings.clone(), engines.clone(), shown_offerings.clone());
        move |engine: &adw::ComboRow, combo: &adw::ComboRow| {
            let id = engines.get(engine.selected() as usize).map(String::as_str);
            let list = for_engine(&offerings, id);
            let labels: Vec<String> = list
                .iter()
                .map(|o| {
                    format!(
                        "{} ({} CPU{})",
                        o.id,
                        o.cpus,
                        if o.cpus == 1 { "" } else { "s" }
                    )
                })
                .collect();
            combo.set_model(Some(&gtk::StringList::new(
                &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            )));
            combo.set_selected(
                list.iter()
                    .position(|o| o.id == "octane2-vpro")
                    .unwrap_or(0) as u32,
            );
            *shown.borrow_mut() = list;
        }
    };
    fill(&engine, &combo);
    let offerings = shown_offerings;
    let name = adw::EntryRow::builder()
        .title("Name")
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
    // Install from discs joins this list later; a ready-made disk is added while one fits.
    let labels = origami_core::facade::disk_labels();
    let disk_model = gtk::StringList::new(
        &labels
            .choices
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    let base_choices = disk_model.n_items();
    let disk = adw::ComboRow::builder()
        .title("Disk")
        .model(&disk_model)
        .build();
    disk.set_widget_name("new.disk");
    let size = adw::SpinRow::with_range(1.0, 2048.0, 1.0);
    size.set_title("Disk size (GB)");
    size.set_value(f64::from(origami_core::facade::disk_labels().default_gb));
    size.set_widget_name("new.size");
    size.set_visible(false);
    let choose = adw::ActionRow::builder()
        .title("Choose an image…")
        .activatable(true)
        .visible(false)
        .build();
    choose.set_widget_name("new.image");
    let release = adw::ComboRow::builder()
        .title(&labels.release)
        .visible(false)
        .build();
    release.set_widget_name("new.release");
    g.add(&disk);
    g.add(&release);
    g.add(&size);
    g.add(&choose);
    let image_ok = Rc::new(std::cell::Cell::new(false));
    let create = gtk::Button::builder()
        .label("Create")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    create.set_widget_name("new.create");
    let ready = {
        let (create, name, disk, image_ok) =
            (create.clone(), name.clone(), disk.clone(), image_ok.clone());
        Rc::new(move || {
            let named = !name.text().trim().is_empty();
            create.set_sensitive(named && (disk.selected() != 2 || image_ok.get()));
        })
    };
    let form = {
        let (image_ok, ready) = (image_ok.clone(), ready.clone());
        disks::ImageForm::new(app, None, move |ok| {
            image_ok.set(ok);
            ready();
        })
    };
    form.group.set_visible(false);
    {
        let (size, choose, form, ready, release) = (
            size.clone(),
            choose.clone(),
            form.clone(),
            ready.clone(),
            release.clone(),
        );
        disk.connect_selected_notify(move |d| {
            release.set_visible(d.selected() == base_choices);
            size.set_visible(d.selected() == 1);
            choose.set_visible(d.selected() == 2);
            form.group.set_visible(d.selected() == 2);
            ready();
        });
    }
    {
        let (form, parent) = (form.clone(), parent.clone());
        choose.connect_activated(move |_| form.choose(&parent));
    }
    // The disks that fit the chosen model, asked of the server off the UI thread.
    let ready_disks = Rc::new(RefCell::new(Vec::<origami_core::facade::ReadyDisk>::new()));
    let asked = Rc::new(std::cell::Cell::new(0u32));
    let refresh = {
        let (app, offerings, ready_disks, asked) = (
            app.clone(),
            offerings.clone(),
            ready_disks.clone(),
            asked.clone(),
        );
        let (disk, disk_model, release, ready_label) = (
            disk.clone(),
            disk_model.clone(),
            release.clone(),
            labels.ready.clone(),
        );
        move |combo: &adw::ComboRow| {
            let Some(id) = offerings
                .borrow()
                .get(combo.selected() as usize)
                .map(|o| o.id.clone())
            else {
                return;
            };
            asked.set(asked.get() + 1);
            let (turn, core) = (asked.get(), app.core.clone());
            let (asked, ready_disks, disk, disk_model, release, ready_label) = (
                asked.clone(),
                ready_disks.clone(),
                disk.clone(),
                disk_model.clone(),
                release.clone(),
                ready_label.clone(),
            );
            glib::spawn_future_local(async move {
                let found =
                    gtk::gio::spawn_blocking(move || core.ready_disks(&id).unwrap_or_default())
                        .await
                        .unwrap_or_default();
                // A later model choice has asked again.
                if turn != asked.get() {
                    return;
                }
                let extra = disk_model.n_items().saturating_sub(base_choices);
                if disk.selected() >= base_choices {
                    disk.set_selected(0);
                }
                let label: Vec<&str> = if found.is_empty() {
                    vec![]
                } else {
                    vec![ready_label.as_str()]
                };
                disk_model.splice(base_choices, extra, &label);
                release.set_model(Some(&gtk::StringList::new(
                    &found.iter().map(|d| d.label.as_str()).collect::<Vec<_>>(),
                )));
                *ready_disks.borrow_mut() = found;
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
    page.add(&g);
    page.add(&form.group);
    let progress = adw::PreferencesGroup::new();
    let bar = gtk::ProgressBar::builder().visible(false).build();
    let line = gtk::Label::builder().visible(false).build();
    let stop = gtk::Button::builder()
        .label("Cancel")
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
        let ready = ready.clone();
        name.connect_changed(move |_| ready());
    }
    let header = adw::HeaderBar::new();
    header.pack_end(&create);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));
    dialog.set_default_widget(Some(&create));
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
        let (disk, size, form) = (disk.clone(), size.clone(), form.clone());
        let (release, ready_disks) = (release.clone(), ready_disks.clone());
        let (bar, line, stop) = (bar.clone(), line.clone(), stop.clone());
        create.connect_clicked(move |b| {
            let Some(o) = offerings.borrow().get(combo.selected() as usize).cloned() else {
                return;
            };
            // The list loads in the background: a pick that no longer exists is refused, not made a bare machine.
            if disk.selected() == base_choices
                && ready_disks
                    .borrow()
                    .get(release.selected() as usize)
                    .is_none()
            {
                err.set_label("Pick a ready-made disk again.");
                err.set_visible(true);
                return;
            }
            let request = CreateRequest {
                name: name.text().trim().to_string(),
                offering: o.id.clone(),
                engine: engines.get(engine.selected() as usize).cloned(),
                system_disk: match disk.selected() {
                    1 => Some(SystemDisk::Empty {
                        gb: size.value() as u32,
                    }),
                    2 => form
                        .request()
                        .map(|(file, copy, overlay)| SystemDisk::Image {
                            file,
                            copy,
                            overlay,
                        }),
                    n if n == base_choices => ready_disks
                        .borrow()
                        .get(release.selected() as usize)
                        .map(|d| SystemDisk::Ready {
                            name: d.name.clone(),
                        }),
                    _ => None,
                },
                ..Default::default()
            };
            b.set_sensitive(false);
            let (app, dialog, err, b) = (app.clone(), dialog.clone(), err.clone(), b.clone());
            match app.core.create_machine(request) {
                Ok(op) => {
                    let finished = op.clone();
                    disks::follow(op, &bar, &line, &stop, move |state| match state {
                        OperationState::Succeeded => {
                            app::rescan(&app);
                            dialog.close();
                            let opened = finished.result_machine();
                            if let Some(entry) = opened.and_then(|k| app::find_entry(&app, &k)) {
                                let _ = app::open(&app, &entry);
                            }
                        }
                        // A cancelled copy leaves the machine without a disk; a cancelled download made nothing.
                        OperationState::Cancelled if finished.result_machine().is_some() => {
                            app::rescan(&app);
                            dialog.close();
                        }
                        OperationState::Cancelled => b.set_sensitive(true),
                        OperationState::Failed(e) => {
                            err.set_label(&e.to_string());
                            err.set_visible(true);
                            b.set_sensitive(true);
                        }
                        OperationState::Running => {}
                    });
                }
                Err(e) => {
                    err.set_label(&e.to_string());
                    err.set_visible(true);
                    b.set_sensitive(true);
                }
            }
        });
    }
    dialog.present(Some(parent));
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use origami_core::facade::OfferingSummary;

    fn offering(id: &str, engine: &str) -> OfferingSummary {
        OfferingSummary {
            id: id.into(),
            engine: engine.into(),
            product: String::new(),
            topology: String::new(),
            topology_name: String::new(),
            has_display: false,
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
