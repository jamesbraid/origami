//! The automation socket: with `--automation SOCKET` the app serves one
//! request per line and answers each with one JSON line. Tests drive the real
//! app through it (invoke a command by its identifier, ask for its state,
//! compare its pixels with the emulator's own screendump) and use the OS's
//! own input injection for keys and the pointer, so the app's input handling
//! is what is tested. The core's shared automation socket replaces this one.
//!
//! Requests:
//!   ping | behaviour | state | stats | cursor [MACHINE] | release-keys [MACHINE]
//!   invoke COMMAND [MACHINE]      run a command as a menu item would
//!   open MACHINE | select MACHINE
//!   clipboard | clipboard-set JSON_STRING   read or set the clipboard text
//!   pick PATH                     answer the next file chooser with PATH
//!   activate WIDGET [VALUE]       use a control by its widget name, as a click, a
//!                                 pick of the combo item labelled VALUE, or a
//!                                 dialog's close would
//!   audit WIDGET                  a dialog's labels, the cut ones and its page, field and step names
//!   widget WIDGET                 whether a control is shown and usable, and a check's state
//!   change memory N | cpus N | network MODE | forward NAME HOST GUEST | share NAME DIR
//!   compare NAME [MACHINE]        the picture shown, against QMP screendump
//!   quit                          stop every machine and exit

use crate::app::{self, App, Msg};
use crate::machine::MachineUi;
use crate::window;
use adw::prelude::*;
use origami_core::{Change, NetworkChange, NetworkMode};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

pub struct Request {
    line: String,
    reply: mpsc::Sender<String>,
}

pub fn serve(app: &Rc<App>, sock: std::path::PathBuf) {
    let _ = std::fs::remove_file(&sock);
    let listener = match UnixListener::bind(&sock) {
        Ok(l) => {
            // The socket can change a machine; only its owner may use it.
            let _ = std::fs::set_permissions(
                &sock,
                std::os::unix::fs::PermissionsExt::from_mode(0o600),
            );
            l
        }
        Err(e) => {
            eprintln!("origami-gtk: automation socket {}: {e}", sock.display());
            return;
        }
    };
    let tx = app.tx.clone();
    std::thread::Builder::new()
        .name("automation".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let mut out = match conn.try_clone() {
                        Ok(o) => o,
                        Err(_) => return,
                    };
                    for line in BufReader::new(conn).lines().map_while(Result::ok) {
                        let (rtx, rrx) = mpsc::channel();
                        if tx
                            .send_blocking(Msg::Auto(Request { line, reply: rtx }))
                            .is_err()
                        {
                            return;
                        }
                        let answer = rrx
                            .recv_timeout(Duration::from_secs(180))
                            .unwrap_or_else(|_| json!({"error": "timed out"}).to_string());
                        if writeln!(out, "{answer}").is_err() {
                            return;
                        }
                    }
                });
            }
        })
        .expect("spawn automation thread");
}

pub fn handle(app: &Rc<App>, req: Request) {
    if req.line.trim() == "clipboard" {
        // The read is asynchronous, so the answer is sent when it completes.
        let cb = gtk::gdk::Display::default().map(|d| d.clipboard());
        gtk::glib::spawn_future_local(async move {
            let text = match cb {
                Some(cb) => cb.read_text_future().await.ok().flatten(),
                None => None,
            };
            let _ = req
                .reply
                .send(json!({"text": text.map(|t| t.to_string())}).to_string());
        });
        return;
    }
    if let Some(text) = req.line.trim().strip_prefix("clipboard-set ") {
        // A JSON string, so the text can hold spaces and line ends.
        let ok = serde_json::from_str::<String>(text).map(|t| {
            if let Some(d) = gtk::gdk::Display::default() {
                d.clipboard().set_text(&t);
            }
        });
        let _ = req.reply.send(json!({"ok": ok.is_ok()}).to_string());
        return;
    }
    let v = run(app, req.line.trim());
    let _ = req.reply.send(v.to_string());
}

fn cpu_seconds() -> f64 {
    // utime + stime of this process, in seconds.
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = s.rsplit(')').next().unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let g = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (g(11) + g(12)) / tck
}

fn pct(v: &[u32], p: f64) -> u32 {
    if v.is_empty() {
        return 0;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    s[((s.len() - 1) as f64 * p) as usize]
}

fn machine(app: &App, name: Option<&str>) -> Option<Rc<MachineUi>> {
    match name {
        Some(n) => app::by_name(app, n),
        None => {
            let w = window::active(app)?;
            window::current(app, &w).or_else(|| app.machines.borrow().first().cloned())
        }
    }
}

pub fn descendants(
    w: &gtk::Widget,
    keep: &dyn Fn(&gtk::Widget) -> bool,
    out: &mut Vec<gtk::Widget>,
) {
    let mut c = w.first_child();
    while let Some(child) = c {
        if keep(&child) {
            out.push(child.clone());
        }
        descendants(&child, keep, out);
        c = child.next_sibling();
    }
}

/// Every widget named `name`, in any window, the window itself included.
fn find(name: &str) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    for top in gtk::Window::list_toplevels() {
        if top.widget_name() == name {
            found.push(top.clone().upcast());
        }
        descendants(top.upcast_ref(), &|c| c.widget_name() == name, &mut found);
    }
    found
}

fn size(w: &gtk::Widget) -> Value {
    json!([w.width(), w.height()])
}

/// What the main window's sidebar, row menu and tab bar show, for the scenarios.
fn chrome(app: &App) -> Value {
    let Some(w) = app.windows.borrow().iter().find(|w| w.has_library).cloned() else {
        return Value::Null;
    };
    let rows: Vec<Value> = w
        .rows
        .borrow()
        .iter()
        .map(|r| {
            let b = r
                .row
                .root()
                .and_then(|root| r.row.compute_bounds(root.upcast_ref::<gtk::Widget>()));
            json!({"name": r.dir.file_name().map(|n| n.to_string_lossy().into_owned()),
                   "bounds": b.map(|b| json!([b.x(), b.y(), b.width(), b.height()]))})
        })
        .collect();
    let selected = w.list.selected_row().and_then(|s| {
        w.rows
            .borrow()
            .iter()
            .find(|r| r.row == s)
            .and_then(|r| r.dir.file_name().map(|n| n.to_string_lossy().into_owned()))
    });
    let mut tabs = Vec::new();
    descendants(w.bar.upcast_ref(), &|c| c.css_name() == "tab", &mut tabs);
    let mut pills = Vec::new();
    descendants(
        w.list.upcast_ref(),
        &|c| c.css_classes().iter().any(|k| k.starts_with("pill")),
        &mut pills,
    );
    let names: Vec<Value> = w
        .rows
        .borrow()
        .iter()
        .map(|r| {
            json!({"text": r.name.label().as_str(), "mode": format!("{:?}", r.name.ellipsize()).to_lowercase(),
                   "ellipsized": r.name.layout().is_ellipsized(), "word": r.word.label().as_str(),
                   "dot": r.dot.is_visible(), "tooltip": r.row.tooltip_text().map(|t| t.to_string())})
        })
        .collect();
    // Each tab's tooltip (the state word first) and the colour at the centre of its dot.
    let pages = w.tv.pages();
    let tabs_state: Vec<Value> = (0..pages.n_items())
        .filter_map(|i| pages.item(i).and_downcast::<adw::TabPage>())
        .map(|p| {
            let dot = p
                .indicator_icon()
                .and_then(|g| g.downcast::<gtk::gdk::Texture>().ok())
                .map(|t| {
                    let mut px = vec![0u8; (t.width() * t.height() * 4) as usize];
                    t.download(&mut px, (t.width() * 4) as usize);
                    let at = ((t.height() / 2) * t.width() + t.width() / 2) as usize * 4;
                    // Premultiplied BGRA to RGB, the dot being opaque at its centre.
                    json!([px[at + 2], px[at + 1], px[at]])
                });
            json!({"title": p.title().as_str(), "tooltip": p.tooltip().map(|t| t.to_string()), "dot": dot})
        })
        .collect();
    let mut pane = Vec::new();
    descendants(
        w.side.upcast_ref(),
        &|c| c.is::<adw::WindowTitle>() && c.is_mapped(),
        &mut pane,
    );
    let guide_header = pane
        .first()
        .and_then(|t| t.downcast_ref::<adw::WindowTitle>())
        .map(|t| t.title().to_string());
    json!({"rows": rows, "selected": selected, "row_menu": w.row_menu.is_visible(), "row_menu_items": row_menu_labels(&w),
           "tab_bar": size(w.bar.upcast_ref()), "tabs": tabs.iter().map(size).collect::<Vec<_>>(),
           "tabs_revealed": w.bar.is_tabs_revealed(), "autohide": w.bar.is_autohide(),
           "row_pills": pills.len(), "row_names": names,
           "title": [w.title.title().as_str(), w.title.subtitle().as_str()],
           "bottom_bar": {"revealed": w.content.reveals_bottom_bars(), "size": w.size_label.label().as_str(), "pointer": w.pointer_label.label().as_str()},
           "power": [w.power_btn.label().map(|l| l.to_string()), w.power_btn.has_css_class("suggested-action")],
           "sidebar": w.side.visible_child_name().map(|n| n.to_string()),
           "guide_header": guide_header, "tab_states": tabs_state, "pointer_toggle": w.pointer_btn.is_active(), "toast": window::last_toast(),
           "card": w.card_for.borrow().as_ref().map(|d| d.display().to_string())})
}

fn row_menu_labels(w: &window::Win) -> Vec<String> {
    let Some(menu) = w.row_menu.menu_model() else {
        return Vec::new();
    };
    (0..menu.n_items())
        .filter_map(|i| menu.item_attribute_value(i, "label", None)?.get::<String>())
        .collect()
}

fn describe(m: &MachineUi) -> Value {
    let mut cards = Vec::new();
    descendants(
        m.hero.upcast_ref(),
        &|c| c.has_css_class("tile"),
        &mut cards,
    );
    json!({
        "name": m.name, "state": m.label().word, "stages": m.stages_done(), "seq": m.seq.get(),
        "size": [m.size.get().0, m.size.get().1],
        "idle_ms": m.last_frame.get().elapsed().as_millis() as u64,
        "imported": m.stats.borrow().imported, "inputs": m.inputs.get(),
        "visible": m.root.is_mapped(), "captured": m.captured.get(), "motion_total": [m.motion_total.get().0, m.motion_total.get().1], "pointer_locked": m.lock.borrow().is_some(),
        "pointer": format!("{:?}", m.pointer.get()).to_lowercase(),
        "cpu_panel": json!({"visible": m.cpu.root.is_visible(), "rows": m.cpu.shown(), "legend": m.cpu.legend(), "note": m.cpu.note(), "title": crate::cpu_panel::TITLE}),
        "pointer_at": [m.pointer_at.get().0, m.pointer_at.get().1],
        "scale_factor": m.scale_factor(),
        "picture": m.layout().map(|l| json!({"rect": [l.alloc().0, l.alloc().1, l.alloc().2, l.alloc().3], "device": [l.dev.0, l.dev.1, l.dev.2, l.dev.3], "step": l.step, "nearest": l.nearest})),
        "diagnostics": m.diagnostics.borrow().as_ref().map(|d| json!({
            "report": d.report.label().to_string(),
            "copy_bounds": d.copy.root().and_then(|r| d.copy.compute_bounds(r.upcast_ref::<gtk::Widget>())).map(|b| json!([b.x(), b.y(), b.width(), b.height()])),
        })),
        "console": m.console.borrow().as_ref().map(|c| c.probe()),
        "console_tab": m.console_tab.borrow().as_ref().map(|c| c.probe()),
        "page": m.root.visible_child_name().map(|n| n.to_string()),
        "cards": cards.iter().filter_map(|c| c.parent()).map(|c| size(&c)).collect::<Vec<_>>(),
        "presents_while_hidden": m.stats.borrow().presents_while_hidden,
        "screen_bounds": m.screen.root().and_then(|r| m.screen.compute_bounds(r.upcast_ref::<gtk::Widget>())).map(|b| json!([b.x(), b.y(), b.width(), b.height()])),
    })
}

fn run(app: &Rc<App>, line: &str) -> Value {
    let mut it = line.split_whitespace();
    let cmd = it.next().unwrap_or("");
    let a1 = it.next();
    let a2 = it.next();
    match cmd {
        "ping" => json!({"ok": true}),
        #[cfg(feature = "selftest")]
        "behaviour" => serde_json::from_str(origami_core::behaviour::JSON).unwrap_or(Value::Null),
        // The rest of the line, with no spaces in it: one JSON request for `behaviour::evaluate`.
        #[cfg(feature = "selftest")]
        "behaviour-evaluate" => {
            serde_json::from_str(&origami_core::behaviour::evaluate(a1.unwrap_or("")))
                .unwrap_or(Value::Null)
        }
        "state" => {
            let ms: Vec<Value> = app.machines.borrow().iter().map(|m| describe(m)).collect();
            let w = window::active(app);
            let enabled: Value = match &w {
                Some(w) => app
                    .commands
                    .iter()
                    .map(|c| (c.id.clone(), json!(window::enabled(app, w, &c.id))))
                    .collect::<serde_json::Map<_, _>>()
                    .into(),
                None => Value::Null,
            };
            let lib: Vec<String> = app
                .library
                .borrow()
                .iter()
                .map(|e| e.name.clone())
                .collect();
            // Where each library row is, for tests that click them.
            let rows: Vec<Value> = app
                .windows
                .borrow()
                .iter()
                .filter(|w| w.has_library)
                .flat_map(|w| {
                    w.rows.borrow().iter().filter_map(|r| {
                        let b = r.row.root().and_then(|root| r.row.compute_bounds(root.upcast_ref::<gtk::Widget>()))?;
                        Some(json!({"dir": r.dir, "bounds": [b.x(), b.y(), b.width(), b.height()]}))
                    }).collect::<Vec<_>>()
                })
                .collect();
            json!({"machines": ms, "library": lib, "library_rows": rows, "chrome": chrome(app), "windows": app.windows.borrow().len(),
                   "enabled": enabled, "trashed": trashed(), "cpu_s": cpu_seconds(), "settings_builds": crate::settings::builds(),
                   "toolbar_tips": window::active(app).map(|w| w.toolbar.iter().map(|b| b.tooltip_text().map(|t| t.to_string())).collect::<Vec<_>>()),
                   "pause_tip": window::active(app).and_then(|w| w.pause_btn.tooltip_text()).map(|t| t.to_string())})
        }
        "open" | "select" => {
            let Some(name) = a1 else {
                return json!({"error": "needs a machine"});
            };
            let Some(entry) = app::find_entry(app, name) else {
                return json!({"error": format!("no machine {name}")});
            };
            match app::open(app, &entry) {
                Some(m) => json!({"ok": true, "machine": m.name}),
                None => json!({"error": "cannot open"}),
            }
        }
        "invoke" => {
            let Some(id) = a1 else {
                return json!({"error": "needs a command"});
            };
            if !app.commands.iter().any(|c| c.id == id) {
                return json!({"error": format!("unknown command {id}")});
            }
            if let Some(name) = a2 {
                if let Some(entry) = app::find_entry(app, name) {
                    let _ = app::open(app, &entry);
                }
            }
            let Some(w) = window::active(app) else {
                return json!({"error": "no window"});
            };
            if !window::enabled(app, &w, id) {
                return json!({"error": "disabled", "command": id});
            }
            window::run(app, &w, id);
            json!({"ok": true, "command": id})
        }
        "credits" => {
            let Some(w) = window::active(app) else {
                return json!({"error": "no window"});
            };
            match a1 {
                Some("acknowledgements") => crate::credits::acknowledgements(&w.win),
                Some("licences") => crate::credits::licences(&w.win),
                _ => return json!({"error": "needs acknowledgements or licences"}),
            }
            json!({"ok": true})
        }
        "cursor" => {
            let Some(m) = machine(app, a1) else {
                return json!({"error": "no machine"});
            };
            match m.machine.cursor(0, u32::MAX).ok().flatten() {
                Some(c) => {
                    json!({"on": c.on, "x": c.x, "y": c.y, "hot": [c.hot_x, c.hot_y], "size": [c.width, c.height]})
                }
                None => json!({"error": "engine reports no cursor"}),
            }
        }
        "compare" => {
            let Some(name) = a1 else {
                return json!({"error": "needs a name"});
            };
            let Some(m) = machine(app, a2) else {
                return json!({"event": "compare", "error": "no machine"});
            };
            let (w, h) = m.size.get();
            let Some(slot) = m.shown_slot.get().filter(|_| m.scanout.borrow().is_some()) else {
                // The copying path: compare the fallback buffer.
                let full = m.fallback.borrow().clone();
                return compare_pixels(&m, name, w, h, &full, None, 0);
            };
            let mut so = m.scanout.borrow_mut();
            let so = so.as_mut().unwrap();
            let modifier = so.slots[slot].modifier;
            match so.readback(slot) {
                Ok(px) => compare_pixels(&m, name, w, h, &px, Some(slot), modifier),
                Err(e) => json!({"event": "compare", "error": e}),
            }
        }
        "stats" => {
            let ms: Vec<Value> = app
                .machines
                .borrow()
                .iter()
                .map(|m| {
                    let es = app.engine.stats(&m.dir);
                    let st = m.stats.borrow();
                    let so = m.scanout.borrow();
                    json!({
                        "name": m.name,
                        "engine_copied_bytes": es.as_ref().map(|s| s.copied_bytes),
                        "engine_imported_lines": es.as_ref().map(|s| s.imported_lines),
                        "engine_frames": es.as_ref().map(|s| s.frames),
                        "backend": app.engine.display_backend(&m.dir),
                        "frames_imported": st.imported,
                        "rows_copied_fallback": st.copied_rows,
                        "retries_busy": st.retries,
                        "presents_while_hidden": st.presents_while_hidden,
                        "gpu_copy_us": {"n": st.copy_us.len(), "p50": pct(&st.copy_us, 0.5),
                                        "p95": pct(&st.copy_us, 0.95), "max": pct(&st.copy_us, 1.0)},
                        "pump_us": {"n": st.pump_us.len(), "p50": pct(&st.pump_us, 0.5),
                                    "p95": pct(&st.pump_us, 0.95), "max": pct(&st.pump_us, 1.0)},
                        "scanout": so.as_ref().map(|s| json!({
                            "gpu_frames": s.counters.gpu_frames, "gpu_lines": s.counters.gpu_lines,
                            "gpu_bytes": s.counters.gpu_bytes, "full_frame_bytes": s.counters.full_frame_bytes,
                            "cpu_pixel_bytes": s.counters.cpu_pixel_bytes,
                            "readback_bytes_test_only": s.counters.readback_bytes,
                            "skipped_busy": s.counters.skipped_busy,
                            "modifiers": s.modifier_note,
                        })),
                    })
                })
                .collect();
            json!({"machines": ms, "cpu_s": cpu_seconds(), "gpu": app.dev.as_ref().map(|d| d.describe())})
        }
        "change" => {
            // The same core call Settings makes, for tests that cannot click
            // through a dialog: change memory|cpus|network|forward|share ...
            let Some(m) = machine(app, None) else {
                return json!({"error": "no machine"});
            };
            let args: Vec<&str> = line.split_whitespace().skip(2).collect();
            let num = |i: usize| args.get(i).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            let change = match (a1, args.first()) {
                (Some("memory"), Some(_)) => Change::SetMemory(num(0)),
                (Some("cpus"), Some(_)) => Change::SetProcessor {
                    module: None,
                    cpus_per_node: Some(num(0)),
                },
                (Some("network"), Some(mode)) => {
                    let mode = match *mode {
                        "private" => NetworkMode::Private,
                        "none" => NetworkMode::None,
                        _ => NetworkMode::User,
                    };
                    Change::SetNetwork(NetworkChange {
                        mode,
                        endpoint: None,
                        interface: None,
                        share_address: None,
                        subnet: None,
                        mac: None,
                    })
                }
                (Some("forward"), Some(name)) => Change::AddForward(origami_machine::PortForward {
                    name: (*name).into(),
                    protocol: "tcp".into(),
                    host_port: num(1) as u16,
                    guest_port: num(2) as u16,
                }),
                (Some("share"), Some(name)) => Change::AddShare {
                    name: (*name).into(),
                    host_dir: args.get(1).unwrap_or(&".").into(),
                },
                _ => {
                    return json!({"error": "change memory N | cpus N | network MODE | forward NAME HOST GUEST | share NAME DIR"})
                }
            };
            let r = origami_core::apply_change(&*app.engine, &m.dir, &change);
            *m.entry.borrow_mut() = crate::library::fresh(&app.core, &m.dir);
            window::rebuild_hero(&m);
            match r {
                Ok(_) => json!({"ok": true}),
                Err(e) => json!({"error": e.to_string()}),
            }
        }
        "pick" => match a1 {
            // The answer for the next file chooser, which a test cannot open.
            Some(path) => {
                crate::disks::queue_pick(path.into());
                json!({"ok": true})
            }
            None => json!({"error": "pick PATH"}),
        },
        "activate" => {
            // Through the widget, so the dialog's own handler does the work.
            let Some(name) = a1 else {
                return json!({"error": "activate WIDGET [VALUE]"});
            };
            let found = find(name);
            let Some(widget) = found.first() else {
                return json!({"error": format!("no widget {name}")});
            };
            if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                return json!({"ok": true, "text": label.label().as_str()});
            }
            let value = line.splitn(3, ' ').nth(2);
            // An alert's response, as its button gives it.
            if let (Some(alert), Some(id)) = (widget.downcast_ref::<adw::AlertDialog>(), value) {
                alert.emit_by_name::<()>("response", &[&id]);
                // The answer is given; a real button would have closed it.
                alert.force_close();
                return json!({"ok": true});
            }
            if let (Some(row), Some(open)) = (widget.downcast_ref::<adw::ExpanderRow>(), value) {
                row.set_expanded(open == "open");
                return json!({"ok": true});
            }
            if let (Some(row), Some(value)) = (widget.downcast_ref::<adw::EntryRow>(), value) {
                row.set_text(value);
                // A row with an apply button applies on this, as the button does.
                row.emit_by_name::<()>("apply", &[]);
                return json!({"ok": true});
            }
            if let (Some(row), Some(value)) = (widget.downcast_ref::<adw::SpinRow>(), value) {
                row.set_value(value.parse().unwrap_or_default());
                return json!({"ok": true});
            }
            match (widget.downcast_ref::<adw::ComboRow>(), value) {
                (Some(row), Some(value)) => {
                    let list = row.model().and_downcast::<gtk::StringList>();
                    let at = (0..list.as_ref().map_or(0, |l| l.n_items())).find(|&i| {
                        list.as_ref().and_then(|l| l.string(i)).as_deref() == Some(value)
                    });
                    match at {
                        Some(i) => {
                            row.set_selected(i);
                            json!({"ok": true})
                        }
                        None => json!({"error": format!("{name} has no item {value}")}),
                    }
                }
                _ => match widget.downcast_ref::<adw::Dialog>() {
                    Some(dialog) => {
                        dialog.close();
                        json!({"ok": true})
                    }
                    None => match widget.downcast_ref::<gtk::Button>() {
                        // A button's own handler, now: GTK's activate waits out a press animation.
                        Some(button) => {
                            button.emit_clicked();
                            json!({"ok": true})
                        }
                        None => json!({"ok": widget.activate()}),
                    },
                },
            }
        }
        "audit" => {
            // The labels shown in a named dialog or window, the ones the layout cut short with an
            // ellipsis, and the page, field and step names a parity check compares across apps.
            let Some(name) = a1 else {
                return json!({"error": "audit WIDGET"});
            };
            let mut roots = Vec::new();
            for top in gtk::Window::list_toplevels() {
                if top.widget_name() == name {
                    roots.push(top.clone().upcast::<gtk::Widget>());
                }
                descendants(top.upcast_ref(), &|c| c.widget_name() == name, &mut roots);
            }
            if roots.is_empty() {
                return json!({"found": false});
            }
            let (mut labels, mut rows, mut pages) = (Vec::new(), Vec::new(), Vec::new());
            let (mut cut, mut steps, mut own) = (Vec::new(), Vec::new(), Vec::new());
            let mut current = Vec::new();
            for r in &roots {
                if let Some(l) = r.downcast_ref::<gtk::Label>() {
                    own.push(l.label().to_string());
                }
                if let Some(t) = r.downcast_ref::<adw::WindowTitle>() {
                    own.push(t.title().to_string());
                }
                descendants(r, &|c| c.is::<gtk::Label>() && c.is_mapped(), &mut labels);
                // A field is a row, or the Disk group whose rows are the ways to get a disk: those vary with
                // what the server offers, so only their heading is compared.
                descendants(
                    r,
                    &|c| {
                        let n = c.widget_name();
                        n == "new.disk-group"
                            || (c.is::<adw::PreferencesRow>()
                                && c.is_mapped()
                                && !n.starts_with("new.disk-row.")
                                && !matches!(n.as_str(), "new.size" | "new.image" | "new.release"))
                    },
                    &mut rows,
                );
                descendants(
                    r,
                    &|c| c.widget_name().starts_with("prefs.page."),
                    &mut pages,
                );
                descendants(r, &|c| c.widget_name() == "install.step-row", &mut steps);
                descendants(
                    r,
                    &|c| c.widget_name() == "install.step-row" && c.has_css_class("current"),
                    &mut current,
                );
            }
            for l in labels.iter().filter_map(|w| w.downcast_ref::<gtk::Label>()) {
                if l.layout().is_ellipsized() {
                    cut.push(l.text().to_string());
                }
            }
            let text = |w: &gtk::Widget| {
                let mut found = Vec::new();
                descendants(w, &|c| c.is::<gtk::Label>(), &mut found);
                found
                    .iter()
                    .filter_map(|l| l.downcast_ref::<gtk::Label>())
                    .map(|l| l.text().to_string())
                    .next()
                    .unwrap_or_default()
            };
            json!({"found": true, "own": own, "ellipsized": cut,
                   "labels": labels.iter().filter_map(|w| w.downcast_ref::<gtk::Label>()).map(|l| l.text().to_string()).collect::<Vec<_>>(),
                   "fields": rows.iter().filter_map(|w| match w.downcast_ref::<adw::PreferencesRow>() {
                       Some(r) => Some(r.title().to_string()),
                       None => w.downcast_ref::<adw::PreferencesGroup>().map(|g| g.title().to_string()),
                   }).collect::<Vec<_>>(),
                   "pages": pages.iter().map(text).collect::<Vec<_>>(),
                   "steps": steps.iter().filter_map(|w| w.downcast_ref::<gtk::Label>()).map(|l| l.text().to_string()).collect::<Vec<_>>(),
                   "current": current.iter().filter_map(|w| w.downcast_ref::<gtk::Label>()).map(|l| l.text().to_string()).collect::<Vec<_>>()})
        }
        "about-text" => {
            // What the About window and the dialog titles say, for the parity check.
            use origami_core::facade::UiString as U;
            let version = option_env!("ORIGAMI_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
            json!({"about": [U::AboutOrigami.text(), origami_core::facade::version_line(version)],
                   "dialog_titles": [U::NewMachine.text(), U::Settings.text(), U::Diagnostics.text(), U::InstallIrix.text()],
                   "toolbar_tooltips": [U::ShowOrHideTheEmulated.text(), U::OpenTheMainMenu.text()]})
        }
        "alert" => {
            // The open alert's words, buttons and defaults, as the self-test checks them.
            let Some(d) = find("alert.dialog")
                .first()
                .and_then(|w| w.clone().downcast::<adw::AlertDialog>().ok())
            else {
                return json!({"found": false});
            };
            let label = |id: Option<gtk::glib::GString>| {
                id.filter(|i| !i.is_empty())
                    .map(|i| d.response_label(&i).to_string())
            };
            let labels: Vec<String> = (0..6)
                .map(|i| i.to_string())
                .filter(|id| d.has_response(id))
                .map(|id| d.response_label(&id).to_string())
                .collect();
            json!({"found": true, "heading": d.heading().map(|h| h.to_string()), "body": d.body().to_string(),
                   "default": label(d.default_response()), "close": label(Some(d.close_response())), "buttons": labels})
        }
        "rows" => {
            // The rows whose widget names start with a prefix: name, title and subtitle.
            let prefix = a1.unwrap_or_default();
            let mut rows = Vec::new();
            for top in gtk::Window::list_toplevels() {
                descendants(
                    top.upcast_ref(),
                    &|c| c.is::<adw::ActionRow>() && c.widget_name().starts_with(prefix),
                    &mut rows,
                );
            }
            let rows: Vec<Value> = rows
                .iter()
                .filter_map(|r| r.downcast_ref::<adw::ActionRow>())
                .map(|r| json!({"name": r.widget_name().as_str(), "title": r.title().as_str(), "subtitle": r.subtitle().map(|s| s.to_string())}))
                .collect();
            json!({"rows": rows})
        }
        "drop" => {
            // What a drop does, without a drag: drop page|library PATH.
            let (Some(w), Some(path)) = (window::active(app), a2) else {
                return json!({"error": "drop page|library PATH"});
            };
            let files = vec![std::path::PathBuf::from(path)];
            let ok = match a1 {
                Some("page") => window::drop_on_page(app, &w, files),
                Some("library") => window::drop_on_library(app, &w, files),
                _ => false,
            };
            json!({"ok": ok})
        }
        "about" => {
            // The About dialog's version and the inventory the hinv egg gave it.
            let mut found = Vec::new();
            for top in gtk::Window::list_toplevels() {
                descendants(
                    top.upcast_ref(),
                    &|c| c.is::<adw::AboutDialog>(),
                    &mut found,
                );
            }
            match found
                .first()
                .and_then(|d| d.downcast_ref::<adw::AboutDialog>())
            {
                Some(d) => {
                    json!({"found": true, "version": d.version().as_str(), "debug_info": d.debug_info().as_str(), "name": d.application_name().as_str()})
                }
                None => json!({"found": false}),
            }
        }
        "scheme" => {
            // Light, dark or default, as the system's setting would be.
            let scheme = match a1 {
                Some("dark") => adw::ColorScheme::ForceDark,
                Some("light") => adw::ColorScheme::ForceLight,
                _ => adw::ColorScheme::Default,
            };
            adw::StyleManager::default().set_color_scheme(scheme);
            json!({"ok": true})
        }
        "menu" => {
            // The main menu, open or closed.
            let Some(w) = window::active(app) else {
                return json!({"error": "no window"});
            };
            let button = w
                .toolbar
                .last()
                .and_then(|b| b.downcast_ref::<gtk::MenuButton>());
            match (button, a1) {
                (Some(b), Some("open")) => b.popup(),
                (Some(b), _) => b.popdown(),
                _ => return json!({"error": "no menu"}),
            }
            json!({"ok": true})
        }
        "alerts" => {
            crate::alerts::show_in_automation(a1 == Some("on"));
            json!({"ok": true})
        }
        "widget" => {
            let found = find(a1.unwrap_or(""));
            match found.first() {
                Some(w) => {
                    json!({"found": true, "mapped": w.is_mapped(), "sensitive": w.is_sensitive(),
                                  "active": w.downcast_ref::<gtk::CheckButton>().map(|c| c.is_active())})
                }
                None => json!({"found": false}),
            }
        }
        "library" => match a1 {
            // The call the Preferences folder chooser makes.
            Some(dir) => {
                app::set_library(app, dir.into());
                json!({"ok": true})
            }
            None => json!({"error": "library DIR"}),
        },
        "release-keys" => {
            let Some(m) = machine(app, a1) else {
                return json!({"error": "no machine"});
            };
            json!({"released": m.machine.release_all_keys().unwrap_or(0)})
        }
        "quit" => {
            let app2 = app.clone();
            gtk::glib::idle_add_local_once(move || app::quit_stopping(&app2));
            json!({"ok": true})
        }
        other => json!({"error": format!("unknown request {other}")}),
    }
}

fn compare_pixels(
    m: &Rc<MachineUi>,
    name: &str,
    w: u32,
    h: u32,
    bgra: &[u8],
    slot: Option<usize>,
    modifier: u64,
) -> Value {
    let Some(port) = origami_machine::control::read(&m.dir)
        .ok()
        .map(|r| r.qmp_port)
    else {
        return json!({"event": "compare", "error": "no QMP port"});
    };
    let out = m.dir.join(format!("{name}.ppm"));
    let (qw, qh, want) = match screendump(port, &out) {
        Ok(v) => v,
        Err(e) => return json!({"event": "compare", "error": e}),
    };
    if (qw, qh) != (w, h) {
        return json!({"event": "compare", "name": name, "error": format!("size {w}x{h} vs QMP {qw}x{qh}")});
    }
    let mut differing = 0usize;
    let mut colours = std::collections::HashSet::new();
    for (g, wnt) in bgra.chunks(4).zip(want.chunks(3)) {
        if g[2] != wnt[0] || g[1] != wnt[1] || g[0] != wnt[2] {
            differing += 1;
        }
        colours.insert([wnt[0], wnt[1], wnt[2]]);
    }
    json!({"event": "compare", "name": name, "machine": m.name, "width": w, "height": h,
           "differing_pixels": differing, "colours": colours.len(), "slot": slot,
           "modifier": format!("{modifier:#x}"), "ppm": out.display().to_string(), "seq": m.seq.get()})
}

/// QEMU's own idea of the screen: a QMP `screendump` as PPM.
fn screendump(port: u16, to: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("QMP: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut out = stream;
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let mut send = |cmd: &str| -> Result<(), String> {
        writeln!(out, "{cmd}").map_err(|e| e.to_string())?;
        loop {
            line.clear();
            reader.read_line(&mut line).map_err(|e| e.to_string())?;
            if line.contains("\"return\"") {
                return Ok(());
            }
            if line.contains("\"error\"") {
                return Err(format!("{cmd}: {line}"));
            }
        }
    };
    send(r#"{"execute":"qmp_capabilities"}"#)?;
    send(&format!(
        r#"{{"execute":"screendump","arguments":{{"filename":"{}","format":"ppm"}}}}"#,
        to.display()
    ))?;
    let mut ppm = Vec::new();
    std::fs::File::open(to)
        .and_then(|mut f| f.read_to_end(&mut ppm))
        .map_err(|e| e.to_string())?;
    let mut at = 0;
    let mut field = || {
        while ppm[at].is_ascii_whitespace() {
            at += 1;
        }
        let s = at;
        while !ppm[at].is_ascii_whitespace() {
            at += 1;
        }
        String::from_utf8_lossy(&ppm[s..at]).into_owned()
    };
    if field() != "P6" {
        return Err("not a PPM".into());
    }
    let (w, h): (u32, u32) = (
        field().parse().map_err(|_| "w")?,
        field().parse().map_err(|_| "h")?,
    );
    field(); // the maximum value
    at += 1;
    Ok((w, h, ppm[at..].to_vec()))
}

/// What the core's self-test "trash" has taken so far.
fn trashed() -> Vec<String> {
    #[cfg(feature = "selftest")]
    return origami_core::facade::selftest_trashed();
    #[cfg(not(feature = "selftest"))]
    Vec::new()
}
