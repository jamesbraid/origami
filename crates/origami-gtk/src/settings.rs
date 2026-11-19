//! Settings for one machine, Preferences for the app, and the New Machine
//! sheet. Machine settings read the machine file; changing them on a stopped
//! machine goes through the core's change operations, which this app does not
//! have yet, so the panes show values and say so instead of faking an edit.
//! Pointer mode is ours and is editable.

use crate::app::{self, App};
use crate::library;
use crate::machine::MachineUi;
use crate::prefs::PointerMode;
use crate::{input, window};
use adw::prelude::*;
use gtk::glib;
use origami_core::facade::CreateRequest;
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
}

/// Through the machine's own engine, so an iris machine is read by iris.
fn load(machine: &origami_core::facade::Machine, dir: &std::path::Path) -> Result<Loaded, String> {
    let settings = machine.settings().map_err(|e| e.to_string())?;
    let file = origami_machine::read_machine(dir).map_err(|e| e.to_string())?;
    Ok(Loaded {
        memory: settings.memory_allowed_mib,
        cpus: settings.cpus_per_node_allowed,
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
            Ok(Ok(_)) => {
                *m.entry.borrow_mut() = library::read(&m.dir);
                crate::window::rebuild_hero(&m);
                crate::window::refresh(&app);
                dialog.close();
                present_machine(&app, &parent, &m);
            }
            Ok(Err(e)) => dialog.add_toast(adw::Toast::new(&e)),
            Err(_) => dialog.add_toast(adw::Toast::new("the change could not be applied")),
        }
    });
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
    let err = gtk::Label::builder()
        .wrap(true)
        .css_classes(["error"])
        .visible(false)
        .xalign(0.0)
        .build();
    g.add(&err);
    page.add(&g);
    let create = gtk::Button::builder()
        .label("Create")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    {
        let create = create.clone();
        name.connect_changed(move |e| create.set_sensitive(!e.text().trim().is_empty()));
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
        create.connect_clicked(move |b| {
            let Some(o) = offerings.borrow().get(combo.selected() as usize).cloned() else {
                return;
            };
            let request = CreateRequest {
                name: name.text().trim().to_string(),
                offering: o.id.clone(),
                engine: engines.get(engine.selected() as usize).cloned(),
                ..Default::default()
            };
            b.set_sensitive(false);
            let (app, dialog, err, b) = (app.clone(), dialog.clone(), err.clone(), b.clone());
            let op = app.core.create_machine(request);
            glib::spawn_future_local(async move {
                let r = match op {
                    Ok(op) => gtk::gio::spawn_blocking(move || match op.wait(None) {
                        origami_core::facade::OperationState::Succeeded => Ok(op.result_machine()),
                        origami_core::facade::OperationState::Failed(e) => Err(e.to_string()),
                        _ => Err("cancelled".into()),
                    })
                    .await
                    .unwrap_or_else(|_| Err("creation failed".into())),
                    Err(e) => Err(e.to_string()),
                };
                match r {
                    Ok(key) => {
                        app::rescan(&app);
                        dialog.close();
                        if let Some(entry) = key.and_then(|k| app::find_entry(&app, &k)) {
                            let _ = app::open(&app, &entry);
                        }
                    }
                    Err(e) => {
                        err.set_label(&e);
                        err.set_visible(true);
                        b.set_sensitive(true);
                    }
                }
            });
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
