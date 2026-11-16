//! The command line's side of the runner: `origami console` and `origami
//! stop` through the control socket, with the fallbacks for a runner that
//! is gone and for a record an older origami wrote.

use origami_machine::control::{is_locked, read, Record};
use origami_machine::Result;
use origami_protocol::{Client, ConsoleData, Event, Request};
use origami_qemu::control as qemu;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::path::Path;
use std::thread;
use std::time::Duration;

const CONNECT: Duration = Duration::from_secs(2);

/// A control connection to the machine's runner, or None when the record
/// names none or nothing answers there.
fn connect(record: &Record) -> Option<Client> {
    if record.control_port == 0 {
        return None;
    }
    let mut client = Client::connect(record.control_port, CONNECT).ok()?;
    client.ping().ok()?;
    Some(client)
}

/// Whether QEMU is up and its runner is not: the record names QEMU's
/// process and nothing holds the machine lock.
pub fn runner_gone(dir: &Path, record: &Record) -> Result<bool> {
    Ok(record.qemu_pid != 0 && !is_locked(dir)?)
}

/// Ask the runner to stop the machine and wait for it to exit. With no
/// runner to ask, quit QEMU over QMP as before.
pub fn stop(dir: &Path) -> Result<()> {
    let record = read(dir)?;
    if let Some(mut client) = connect(&record) {
        let request = Request::Stop {
            clean: true,
            timeout: None,
        };
        if client.call_data(&request).is_ok() {
            return qemu::wait_for_exit(dir);
        }
    }
    qemu::stop(dir)
}

/// Connect this terminal to the machine's console. A lease notice from the
/// broker goes to standard error.
pub fn console(dir: &Path) -> Result<()> {
    let record = read(dir)?;
    qemu::verified_qmp(&record)?;
    if record.control_port == 0 {
        // A record from an older origami: QEMU's own console is the one.
        return qemu::console_direct(&record);
    }
    let Some(client) = connect(&record) else {
        eprintln!(
            "origami: the runner for this machine is gone; connecting to QEMU's console directly, which nothing else can share"
        );
        return qemu::console_direct(&record);
    };
    let (data, stream) = client
        .console("", false, None)
        .map_err(|e| format!("console: {e}"))?;
    let data: ConsoleData = serde_json::from_value(data)?;
    watch_notices(&record, data.conn);
    let mut output = stream.try_clone_writer()?;
    let mut input = stream.try_clone_writer()?;
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        let mut stdin = io::stdin();
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || input.write_all(&buf[..n]).is_err() {
                break;
            }
        }
    });
    let mut stream = stream;
    let result = copy_flushing(&mut stream, &mut io::stdout());
    let _ = output.flush();
    let _ = output.shutdown(Shutdown::Both);
    result?;
    Ok(())
}

// A raw connection cannot carry notices, so a second control connection
// listens for those about this one and prints them.
fn watch_notices(record: &Record, conn: u64) {
    let Some(mut client) = connect(record) else {
        return;
    };
    // The runner pushes events to the connections that have made a request.
    if client.call_data(&Request::Status {}).is_err() {
        return;
    }
    thread::spawn(move || {
        while let Ok(ev) = client.next_event(Duration::from_secs(3600)) {
            match ev {
                Some(Event::Notice { text, conn: c, .. }) if c == conn => {
                    eprintln!("origami: {text}");
                }
                Some(Event::LeaseLost {
                    reason, conn: c, ..
                }) if c == conn => eprintln!("origami: console lease ended: {reason}"),
                _ => {}
            }
        }
    });
}

/// Standard output holds a partial line until a newline, so a prompt such
/// as `login: ` would never show; flush after every read.
fn copy_flushing(from: &mut impl Read, to: &mut impl Write) -> io::Result<()> {
    let mut buf = [0u8; 8192];
    loop {
        match from.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                to.write_all(&buf[..n])?;
                to.flush()?;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}
