//! QMP and the QEMU process of a running machine: liveness, stop, reset
//! and the console.

use origami_machine::console::Console;
use origami_machine::control::{is_locked, read, Record};
use origami_machine::Result;
use qapi::qmp;
use std::fs;
use std::io::{self, BufReader};
use std::net::{SocketAddrV4, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

fn connect(record: &Record) -> Result<TcpStream> {
    let address = SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, record.qmp_port);
    let stream = TcpStream::connect_timeout(&address.into(), Duration::from_millis(400))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    Ok(stream)
}

/// A QMP connection to a running machine.
pub type Qmp = qapi::Qmp<qapi::Stream<BufReader<TcpStream>, TcpStream>>;

/// Connect to the QMP endpoint in `record` and check that it belongs to
/// that machine's QEMU, not another process that reused the port.
pub fn verified_qmp(record: &Record) -> Result<Qmp> {
    let stream = connect(record)?;
    let mut qmp = Qmp::new(qapi::Stream::new(
        BufReader::new(stream.try_clone()?),
        stream,
    ));
    qmp.handshake()?;
    if qmp.execute(&qmp::query_name {})?.name.as_deref() != Some(&record.name) {
        return Err("QMP endpoint belongs to another machine process".into());
    }
    Ok(qmp)
}

/// The run record of the machine in `dir`, if its QEMU answers QMP. Use the
/// record this returns rather than reading it again: the runner removes it
/// the moment QEMU exits, so a second read can find nothing.
pub fn running(dir: &Path) -> Result<Option<Record>> {
    match read(dir) {
        Ok(record) => Ok(verified_qmp(&record).is_ok().then_some(record)),
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Ask the machine to quit, then wait until its runner has exited.
///
/// QEMU's quit is a clean exit, and that is when a CHD disk folds its
/// `.diff.chd` back into the image. The fold recompresses the whole image,
/// which takes minutes for a multi-gigabyte one, and killing QEMU meanwhile
/// leaves the diff unfolded. So this never gives up and never kills: it
/// waits for the runner, which holds the machine lock for as long as QEMU
/// lives.
pub fn stop(dir: &Path) -> Result<()> {
    let record = read(dir)?;
    // QEMU may already have taken a quit and stopped answering QMP while it
    // folds, as when an earlier stop was interrupted. The runner's lock says
    // it is still alive, so keep waiting instead of reporting a dead machine.
    if let Err(error) =
        verified_qmp(&record).and_then(|mut qmp| Ok(qmp.write_command(&qmp::quit {})?))
    {
        if !is_locked(dir)? {
            return Err(error);
        }
    }
    let started = Instant::now();
    let log = dir.join("logs/runner.log");
    let log_start = fs::metadata(&log).map_or(0, |meta| meta.len());
    let mut next_note = NOTE_INTERVAL;
    // A fold is slow, so say what the wait is for once it outlasts a plain exit.
    let mut announced = false;
    while is_locked(dir)? {
        std::thread::sleep(POLL_INTERVAL);
        let elapsed = started.elapsed();
        if !announced && elapsed >= Duration::from_secs(2) {
            announced = true;
            eprintln!(
                "waiting for QEMU to exit. A CHD disk folds its changes back into the image first, which can take minutes. Do not interrupt it."
            );
        }
        if elapsed >= next_note {
            next_note += NOTE_INTERVAL;
            let progress = last_line_since(&log, log_start);
            eprintln!(
                "still waiting for QEMU to exit ({}s){}",
                elapsed.as_secs(),
                if progress.is_empty() {
                    String::new()
                } else {
                    format!(": {progress}")
                }
            );
        }
    }
    Ok(())
}

/// End the machine's QEMU at once, without a clean exit. A CHD disk's
/// changes are not folded back into the image (they stay in its `.diff.chd`),
/// and the guest gets no chance to sync its disks. For a machine that does
/// not answer `stop`.
pub fn force_stop(dir: &Path) -> Result<()> {
    let record = read(dir)?;
    // The lock alone does not say QEMU is up: an edit command holds it too.
    // A live runner (the record's pid) does. The runner reaps QEMU just
    // before it exits, so a kill landing in that instant could reach a reused
    // pid; the window is the runner's last microseconds.
    if !is_locked(dir)? || !process_alive(record.pid) {
        return Err("the machine is not running".into());
    }
    if record.qemu_pid == 0 {
        return Err("this machine's run record does not name QEMU's process (an older origami started it); use stop".into());
    }
    kill(record.qemu_pid)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while is_locked(dir)? {
        if Instant::now() >= deadline {
            return Err("QEMU was killed but its runner has not exited after 30 seconds".into());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(unix)]
fn kill(pid: u32) -> Result<()> {
    // SAFETY: kill(2) takes no pointers; the caller checked the runner lives.
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) } != 0 {
        let error = io::Error::last_os_error();
        // Already gone: the stop has happened.
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    Ok(())
}

#[cfg(windows)]
fn kill(pid: u32) -> Result<()> {
    let status = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .status()?;
    if !status.success() {
        return Err(format!("taskkill exited with {status}").into());
    }
    Ok(())
}

/// Reset the guest as its reset button would, leaving QEMU running.
pub fn reset(dir: &Path) -> Result<()> {
    let record = read(dir)?;
    verified_qmp(&record)?.execute(&qmp::system_reset {})?;
    Ok(())
}

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const NOTE_INTERVAL: Duration = Duration::from_secs(30);

/// The last non-empty line the runner log gained after `start`: QEMU's
/// progress report from a fold, when one is running.
fn last_line_since(path: &Path, start: u64) -> String {
    let text = fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(text.get(start as usize..).unwrap_or_default());
    let last = text
        .split(['\n', '\r'])
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or_default();
    last.trim().to_string()
}

pub fn console(dir: &Path) -> Result<()> {
    let record = read(dir)?;
    verified_qmp(&record)?;
    let address = SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, record.console_port);
    let mut stream = TcpStream::connect(address)?;
    let mut input = stream.try_clone()?;
    std::thread::spawn(move || {
        let _ = io::copy(&mut io::stdin(), &mut input);
    });
    io::copy(&mut stream, &mut io::stdout())?;
    Ok(())
}

/// Connect to the console of the machine running from `dir`.
pub fn connect_console(dir: &Path) -> Result<Console> {
    let record = read(dir).map_err(|error| {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
        {
            "install commands require a background run; start with origami run DIR --background"
                .into()
        } else {
            error
        }
    })?;
    verified_qmp(&record)?;
    Console::open(&record)
}
