//! The automation socket: with `--automation SOCKET` the app serves one
//! request per line and answers each with one JSON line. Tests drive the real
//! app through it (invoke a command by its identifier, ask for its state,
//! compare its pixels with the emulator's own screendump) and use the OS's
//! own input injection for keys and the pointer, so the app's input handling
//! is what is tested. The core's shared automation socket replaces this one.
//!
//! Requests:
//!   ping | state | stats | cursor [MACHINE] | release-keys [MACHINE]
//!   invoke COMMAND [MACHINE]      run a command as a menu item would
//!   open MACHINE | select MACHINE
//!   compare NAME [MACHINE]        the picture shown, against QMP screendump
//!   quit                          stop every machine and exit

use crate::app::{self, App, Msg};
use crate::machine::MachineUi;
use crate::window;
use adw::prelude::*;
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

fn describe(m: &MachineUi) -> Value {
    json!({
        "name": m.name, "state": m.state.get().pill().0, "seq": m.seq.get(),
        "size": [m.size.get().0, m.size.get().1],
        "idle_ms": m.last_frame.get().elapsed().as_millis() as u64,
        "imported": m.stats.borrow().imported, "inputs": m.inputs.get(),
        "visible": m.root.is_mapped(), "captured": m.captured.get(),
        "pointer": format!("{:?}", m.pointer.get()).to_lowercase(),
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
            json!({"machines": ms, "library": lib, "windows": app.windows.borrow().len(),
                   "enabled": enabled, "cpu_s": cpu_seconds(),
                   "status_pills": app.windows.borrow().iter().map(|w| w.pill.label().to_string()).collect::<Vec<_>>()})
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
