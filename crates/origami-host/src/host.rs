//! What the runner answers on its control socket.

use crate::broker::{pump, Broker, ConnId, Connection, InputError, Notice, Notifier, Reader};
use crate::link::{Link, LinkSink};
use origami_engine::{Control, RunState, Session, Stop};
use origami_protocol::{
    BootOffsetData, Conn, ConsoleData, ErrorKind, Event, EventSender, Handler, Outcome, RawStream,
    Reply, Request,
};
use serde_json::json;
use std::collections::HashMap;
use std::net::Shutdown;
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// The only console a QEMU machine has.
const LINE: &str = "console";

/// Bytes `serial-read` returns when the caller names no limit.
const READ_DEFAULT: usize = 64 * 1024;

type Slot = Arc<ConsoleSlot>;

/// One control connection's place at the console: its broker connection,
/// made on first use, and the reader a `console` request set up.
struct ConsoleSlot {
    conn: Mutex<Option<Connection<LinkSink>>>,
    reader: Mutex<Option<Reader>>,
}

pub struct Host {
    session: Arc<dyn Session>,
    broker: Arc<Broker<LinkSink>>,
    link: Arc<Link>,
    /// Removable drives by guest name; the first is the default.
    cdroms: Vec<String>,
    engine: String,
    me: Weak<Host>,
    slots: Mutex<HashMap<u64, Slot>>,
    watchers: Mutex<HashMap<u64, EventSender>>,
}

impl Host {
    pub fn new(
        engine: &str,
        session: Arc<dyn Session>,
        broker: Arc<Broker<LinkSink>>,
        link: Arc<Link>,
        cdroms: Vec<String>,
    ) -> Arc<Host> {
        Arc::new_cyclic(|me| Host {
            session,
            broker,
            link,
            cdroms,
            engine: engine.into(),
            me: me.clone(),
            slots: Mutex::default(),
            watchers: Mutex::default(),
        })
    }

    pub fn broker(&self) -> &Arc<Broker<LinkSink>> {
        &self.broker
    }

    pub fn link(&self) -> &Arc<Link> {
        &self.link
    }

    /// Tell every connected control client the machine's state changed.
    pub fn state_changed(&self, state: &str) {
        let ev = Event::StateChanged {
            state: state.into(),
        };
        for (_, w) in self.watcher_list() {
            let _ = w.send(&ev);
        }
    }

    fn watcher_list(&self) -> Vec<(u64, EventSender)> {
        self.watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, w)| (*id, w.clone()))
            .collect()
    }

    /// Deliver a broker notice about control connection `id`: on that
    /// connection if it can still take lines, else on every other one,
    /// tagged with `id`, because a raw console connection cannot.
    fn deliver(&self, id: u64, n: Notice) {
        let ev = match n {
            Notice::InputDropped { holder } => Event::Notice {
                line: LINE.into(),
                text: format!(
                    "another client holds the console (connection {}); your input is ignored",
                    holder.0
                ),
                conn: id,
            },
            Notice::LeaseBroken { ctrl_u } => Event::LeaseLost {
                line: LINE.into(),
                reason: "another client broke the lease".into(),
                ctrl_u,
                conn: id,
            },
        };
        let all = self.watcher_list();
        if all
            .iter()
            .find(|(i, _)| *i == id)
            .is_some_and(|(_, w)| w.send(&ev).unwrap_or(false))
        {
            return;
        }
        for (i, w) in all {
            if i != id {
                let _ = w.send(&ev);
            }
        }
    }

    fn slot(&self, conn: &Conn) -> Slot {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots
            .entry(conn.id)
            .or_insert_with(|| {
                let (me, id) = (self.me.clone(), conn.id);
                let notifier: Arc<dyn Notifier> = Arc::new(move |n: Notice| {
                    if let Some(host) = me.upgrade() {
                        host.deliver(id, n);
                    }
                });
                Arc::new(ConsoleSlot {
                    conn: Mutex::new(Some(self.broker.connect(Some(notifier)))),
                    reader: Mutex::new(None),
                })
            })
            .clone()
    }

    fn on_console<T>(
        &self,
        conn: &Conn,
        f: impl FnOnce(&Connection<LinkSink>) -> Result<T, InputError>,
    ) -> Result<T, Reply> {
        self.link
            .ensure()
            .map_err(|e| Reply::err(ErrorKind::Io, format!("console: {e}")))?;
        let slot = self.slot(conn);
        let guard = slot.conn.lock().unwrap_or_else(|e| e.into_inner());
        let c = guard
            .as_ref()
            .ok_or_else(|| Reply::err(ErrorKind::Io, "this connection is in raw console mode"))?;
        f(c).map_err(input_error)
    }

    fn state(&self) -> RunState {
        self.session.status().state
    }

    fn handle_inner(&self, conn: &Conn, req: Request) -> Result<Outcome, Reply> {
        let ok = |r: Reply| Ok(Outcome::Reply(r));
        match req {
            Request::Ping {} => ok(Reply::ok()),
            Request::Status {} => {
                let facts = self.session.status();
                ok(Reply::data(json!({
                    "state": state_name(&facts.state),
                    "qemu_pid": facts.pid,
                    "pid": std::process::id(),
                    "engine": self.engine,
                    "boot_offset": self.broker.boot_offset(),
                })))
            }
            Request::Quit {} => {
                self.stop_in_background(Stop::Clean { timeout: None });
                ok(Reply::ok())
            }
            Request::Stop { clean, timeout } => {
                self.stop_in_background(if clean {
                    Stop::Clean {
                        timeout: timeout.map(Duration::from_secs),
                    }
                } else {
                    Stop::Force
                });
                ok(Reply::ok())
            }
            Request::SerialSend { data } => {
                let sent = self.on_console(conn, |c| c.send(data.as_bytes()))?;
                match sent {
                    crate::broker::SendOutcome::Sent => ok(Reply::ok()),
                    crate::broker::SendOutcome::Dropped => ok(Reply::err(
                        ErrorKind::LeaseHeld,
                        "another client holds the console",
                    )),
                }
            }
            Request::SerialRead { since, max } => {
                let mut reader = self
                    .broker
                    .reader_at(since.unwrap_or(self.broker.ring().head()));
                self.link_ensure()?;
                let mut buf = vec![0u8; max.unwrap_or(READ_DEFAULT).min(1 << 20)];
                let o = reader.read(&mut buf);
                ok(Reply::data(json!({
                    "data": String::from_utf8_lossy(&buf[..o.n]),
                    "next": reader.offset(),
                    "dropped": o.dropped,
                    "eof": o.eof,
                })))
            }
            Request::WaitSerial {
                pattern,
                timeout_ms,
                since,
            } => {
                let mut reader = self
                    .broker
                    .reader_at(since.unwrap_or(self.broker.ring().head()));
                self.link_ensure()?;
                match reader.wait_for(pattern.as_bytes(), Duration::from_millis(timeout_ms)) {
                    crate::broker::WaitResult::Found(offset) => {
                        ok(Reply::data(json!({ "offset": offset })))
                    }
                    crate::broker::WaitResult::Timeout => {
                        ok(Reply::err(ErrorKind::Timeout, "the pattern did not appear"))
                    }
                    crate::broker::WaitResult::Eof => {
                        ok(Reply::err(ErrorKind::NotRunning, "the console ended"))
                    }
                }
            }
            Request::Screenshot { head, path } => {
                self.session
                    .screendump(head as usize, Path::new(&path))
                    .map_err(engine_error)?;
                ok(Reply::ok())
            }
            Request::CdromLoad { path, drive } => {
                let drive = self.drive(drive)?;
                self.session
                    .media(&drive, Some(Path::new(&path)))
                    .map_err(engine_error)?;
                ok(Reply::ok())
            }
            Request::CdromEject { drive } => {
                let drive = self.drive(drive)?;
                self.session.media(&drive, None).map_err(engine_error)?;
                ok(Reply::ok())
            }
            Request::Reset {} => {
                self.link_ensure()?;
                let boot_offset = self.broker.mark_boot();
                self.session.control(Control::Reset).map_err(engine_error)?;
                ok(Reply::data(BootOffsetData { boot_offset }))
            }
            Request::PowerButton {} => self.control(Control::PowerButton),
            Request::Nmi {} => self.control(Control::Nmi),
            Request::Pause {} => self.control(Control::Pause),
            Request::Resume {} => self.control(Control::Resume),
            Request::Cont {} => {
                if self.state() != RunState::Prelaunch {
                    return ok(Reply::err(
                        ErrorKind::Engine,
                        "the machine was not started paused",
                    ));
                }
                self.link_ensure()?;
                // Mark first: output the guest writes the instant it runs
                // belongs to the boot.
                let boot_offset = self.broker.mark_boot();
                self.session
                    .control(Control::Resume)
                    .map_err(engine_error)?;
                ok(Reply::data(BootOffsetData { boot_offset }))
            }
            Request::Console { line, lease, from } => {
                if !line.is_empty() && line != LINE {
                    return ok(Reply::err(
                        ErrorKind::BadRequest,
                        format!("no console named {line}"),
                    ));
                }
                // Place the reader before connecting: the guest may speak the
                // moment the runner does.
                let reader = self
                    .broker
                    .reader_at(from.unwrap_or(self.broker.ring().head()));
                let leased = self.on_console(conn, |c| {
                    if lease {
                        c.lease()?;
                    }
                    Ok(c.holds_lease())
                })?;
                let start = reader.offset();
                *self
                    .slot(conn)
                    .reader
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(reader);
                Ok(Outcome::Raw(Reply::data(ConsoleData {
                    start,
                    live_from: self.broker.live_from(),
                    leased,
                    conn: conn.id,
                })))
            }
            Request::Lease { .. } => {
                let g = self.on_console(conn, |c| c.lease())?;
                ok(Reply::data(
                    json!({"offset": g.offset, "input_seq": g.input_seq}),
                ))
            }
            Request::Release { .. } => {
                let i = self.on_console(conn, |c| c.release())?;
                ok(Reply::data(json!({ "ctrl_u": i.ctrl_u })))
            }
            Request::BreakLease { .. } => {
                let old = self.broker.break_lease().map_err(input_error)?;
                ok(Reply::data(json!({ "broken": old.map(|c: ConnId| c.0) })))
            }
        }
    }

    fn link_ensure(&self) -> Result<(), Reply> {
        self.link
            .ensure()
            .map_err(|e| Reply::err(ErrorKind::Io, format!("console: {e}")))
    }

    fn control(&self, c: Control) -> Result<Outcome, Reply> {
        self.session.control(c).map_err(engine_error)?;
        Ok(Outcome::Reply(Reply::ok()))
    }

    fn drive(&self, drive: Option<String>) -> Result<String, Reply> {
        drive
            .or_else(|| self.cdroms.first().cloned())
            .ok_or_else(|| Reply::err(ErrorKind::Unsupported, "this machine has no CD-ROM drive"))
    }

    // A stop can take minutes (a CHD disk folds into its image), so the
    // reply does not wait for it; the caller waits on the runner's exit.
    fn stop_in_background(&self, how: Stop) {
        let session = self.session.clone();
        let _ = std::thread::Builder::new()
            .name("stop".into())
            .spawn(move || {
                let _ = session.stop(how);
            });
    }
}

impl Handler for Host {
    fn engine(&self) -> String {
        self.engine.clone()
    }

    fn handle(&self, conn: &Conn, req: Request) -> Outcome {
        self.watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(conn.id)
            .or_insert_with(|| conn.events());
        match self.handle_inner(conn, req) {
            Ok(o) => o,
            Err(r) => Outcome::Reply(r),
        }
    }

    fn raw(&self, conn: &Conn, stream: RawStream) {
        let slot = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&conn.id)
            .cloned();
        let Some(slot) = slot else { return };
        let reader = slot.reader.lock().unwrap_or_else(|e| e.into_inner()).take();
        let connection = slot.conn.lock().unwrap_or_else(|e| e.into_inner()).take();
        let (Some(reader), Some(connection)) = (reader, connection) else {
            return;
        };
        let (Ok(out), Ok(closer)) = (stream.try_clone_writer(), stream.try_clone_writer()) else {
            return;
        };
        pump(connection, reader, stream, out, move || {
            let _ = closer.shutdown(Shutdown::Both);
        });
    }

    fn closed(&self, conn: &Conn) {
        self.watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&conn.id);
        // Dropping the slot ends a lease this connection still held.
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&conn.id);
    }
}

fn state_name(state: &RunState) -> &'static str {
    match state {
        RunState::Prelaunch => "prelaunch",
        RunState::Running => "running",
        RunState::Paused => "paused",
        RunState::Unknown(_) => "unknown",
        RunState::Exited { .. } => "exited",
    }
}

/// The name a state change event carries.
pub fn state_text(state: &RunState) -> &'static str {
    state_name(state)
}

fn input_error(e: InputError) -> Reply {
    match e {
        InputError::LeaseHeld(_) => Reply::err(ErrorKind::LeaseHeld, e.to_string()),
        InputError::NotHolder => Reply::err(ErrorKind::LeaseHeld, e.to_string()),
        InputError::Stalled => Reply::err(ErrorKind::Timeout, e.to_string()),
        InputError::Io(_) => Reply::err(ErrorKind::Io, e.to_string()),
    }
}

/// QMP is single-client: while another client holds it, a request waits for
/// the greeting until its read times out, which reads as `Busy`.
fn engine_error(e: Box<dyn std::error::Error + Send + Sync>) -> Reply {
    let text = e.to_string();
    let busy = ["timed out", "temporarily unavailable", "os error 11"]
        .iter()
        .any(|s| text.contains(s));
    Reply::err(
        if busy {
            ErrorKind::Busy
        } else {
            ErrorKind::Engine
        },
        text,
    )
}
