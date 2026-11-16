//! The runner: starts a machine and hosts it until it exits.
//!
//! It is the one client of QEMU's console chardev, serves the machine's
//! control socket and the legacy raw console port, and writes the run
//! record that tells every other process where they are. QMP stays with
//! QEMU: requests open a connection each, so a user's own QMP session is
//! never locked out by the runner.

use crate::broker::{Broker, Config, RawPort};
use crate::host::{state_text, Host};
use crate::link::{Link, LinkSink};
use origami_engine::{RunState, Stop, Wake};
use origami_machine::control as record;
use origami_machine::{MachineFile, Machines, Offering, Result};
use origami_protocol::Server;
use origami_qemu::runtime::{self, Display, LaunchOptions, Runtime};
use origami_qemu::QemuEngine;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How often the runner asks QEMU for its run state.
const POLL: Duration = Duration::from_secs(1);

pub struct ServeArgs<'a> {
    pub rt: &'a Runtime,
    pub machines: &'a Machines,
    pub dir: &'a Path,
    pub file: &'a MachineFile,
    pub offering: &'a Offering,
    pub display: Display,
    pub once: &'a [String],
    /// `cli` or `app`, for the run record.
    pub started_by: &'a str,
    /// The terminal is the console too: a run in the foreground.
    pub foreground: bool,
    /// What the launch is asked for besides the machine: `hold` starts QEMU
    /// paused, until a client sends `cont`.
    pub options: LaunchOptions,
}

/// How QEMU ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    /// None when a signal ended it.
    pub code: Option<i32>,
}

impl Exit {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code {
            Some(code) => write!(f, "exit status: {code}"),
            None => write!(f, "signal"),
        }
    }
}

/// Run the machine in this process until it exits. The background runner
/// and `origami run` in the foreground are this same function.
pub fn serve(a: ServeArgs) -> Result<Exit> {
    let (link, tail) = Link::new(a.dir);
    let (broker, writer) = Broker::new(Config::default(), &tail, LinkSink(link.clone()));
    link.set_writer(writer);
    let mut launched = runtime::launch(
        a.rt, a.machines, a.dir, a.file, a.offering, a.display, a.once, &a.options,
    )?;
    link.set_port(launched.engine_console);
    let woken = Arc::new((Mutex::new(false), Condvar::new()));
    let wake: Wake = {
        let woken = woken.clone();
        Arc::new(move |_| {
            *woken.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
            woken.1.notify_all();
        })
    };
    let engine = QemuEngine::new(a.rt.clone());
    let session = engine.adopt(&mut launched, wake);
    let cdroms = launched.removable.iter().map(|(n, _)| n.clone()).collect();
    let host = Host::new(
        "qemu",
        session.clone(),
        broker.clone(),
        link.clone(),
        cdroms,
    );

    let bound = (|| -> std::io::Result<_> {
        let attach = link.clone();
        let raw = RawPort::bind_with((Ipv4Addr::LOCALHOST, 0), broker.clone(), move || {
            // A failure shows as a console that stays silent; the next
            // client or request tries again.
            let _ = attach.ensure();
        })?;
        let server = Server::bind((Ipv4Addr::LOCALHOST, 0), host.clone())?;
        Ok((raw, server))
    })();
    let (mut raw, mut server) = match bound {
        Ok(b) => b,
        Err(e) => {
            let _ = session.stop(Stop::Force);
            return Err(format!("cannot open the console and control sockets: {e}").into());
        }
    };
    let rec = record::Record {
        pid: std::process::id(),
        qmp_port: launched.qmp_port,
        console_port: raw.local_addr().port(),
        name: launched.name.clone(),
        qemu_pid: launched.pid(),
        control_port: server.local_addr().port(),
        engine_console: launched.engine_console,
        started_by: a.started_by.into(),
    };
    if let Err(e) = record::write(a.dir, &rec) {
        let _ = session.stop(Stop::Force);
        return Err(e);
    }
    let terminal = a
        .foreground
        .then(|| crate::tty::attach(&broker, &link))
        .transpose()?;

    let mut shown = state_text(&session.status().state);
    let code = loop {
        let facts = session.status();
        if let RunState::Exited { code, .. } = facts.state {
            break code;
        }
        let now = state_text(&facts.state);
        if now != shown {
            shown = now;
            host.state_changed(now);
        }
        let mut flag = woken.0.lock().unwrap_or_else(|e| e.into_inner());
        if !*flag {
            flag = woken
                .1
                .wait_timeout(flag, POLL)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *flag = false;
    };
    // Remove the record before the lock goes: anyone who sees the machine
    // stopped must not find a record of it.
    record::clear_if_current(a.dir, &rec);
    host.state_changed("exited");
    server.shutdown();
    raw.shutdown();
    if let Some(t) = terminal {
        t.finish();
    }
    drop(launched);
    Ok(Exit { code })
}
