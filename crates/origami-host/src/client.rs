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

/// The record of the machine in `dir` if it is running: QEMU answers QMP, or,
/// for an engine with no QMP port in its record, the runner answers.
pub fn running(dir: &Path) -> Result<Option<Record>> {
    match read(dir) {
        Ok(record) if record.qmp_port == 0 => Ok(connect(&record).map(|_| record)),
        _ => qemu::running(dir),
    }
}

/// Reset the guest: through the runner for an engine with no QMP, else QMP.
pub fn reset(dir: &Path) -> Result<()> {
    let record = read(dir)?;
    if record.qmp_port != 0 {
        return qemu::reset(dir);
    }
    let mut client = connect(&record).ok_or("the machine's runner does not answer")?;
    client
        .call_data(&Request::Reset {})
        .map_err(|e| format!("reset: {e}"))?;
    Ok(())
}

/// End the machine at once: QEMU's process, or the runner itself for an
/// engine that runs inside it.
pub fn force_stop(dir: &Path) -> Result<()> {
    let record = read(dir)?;
    if record.qmp_port != 0 {
        return qemu::force_stop(dir);
    }
    if !is_locked(dir)? {
        return Err("the machine is not running".into());
    }
    #[cfg(unix)]
    {
        // An edit command holds the lock too, and a dead runner's pid may be reused.
        // SAFETY: signal 0 only checks that the process exists.
        if unsafe { libc::kill(record.pid as libc::pid_t, 0) } != 0 {
            return Err("the machine is not running".into());
        }
        // SAFETY: kill(2) takes no pointers; the lock says the runner lives.
        if unsafe { libc::kill(record.pid as libc::pid_t, libc::SIGKILL) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while is_locked(dir)? {
            if std::time::Instant::now() >= deadline {
                return Err("the runner was killed but has not exited after 30 seconds".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    Err("forcing a stop of this engine is not supported here".into())
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
    // The runner reads this at the exit, so `status` can say the stop was asked for.
    origami_machine::control::note_stop(dir, "stop");
    if let Some(mut client) = connect(&record) {
        let request = Request::Stop {
            clean: true,
            timeout: None,
        };
        if client.call_data(&request).is_ok() {
            return qemu::wait_for_exit(dir);
        }
    }
    // A stop that fails leaves the machine running; a later exit is not ours.
    qemu::stop(dir).inspect_err(|_| origami_machine::control::clear_exit(dir))
}

/// Connect this terminal to the machine's console. A lease notice from the
/// broker goes to standard error.
pub fn console(dir: &Path) -> Result<()> {
    let record = read(dir)?;
    if record.qmp_port != 0 {
        qemu::verified_qmp(&record)?;
    }
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
