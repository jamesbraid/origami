use crate::Result;
use qapi::qmp;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, BufReader};
use std::net::{SocketAddrV4, TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Record {
    pub pid: u32,
    pub qmp_port: u16,
    pub console_port: u16,
    pub name: String,
}

pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

pub fn path(dir: &Path) -> std::path::PathBuf {
    dir.join("state/run.json")
}

pub fn read(dir: &Path) -> Result<Record> {
    Ok(serde_json::from_slice(&fs::read(path(dir))?)?)
}

pub fn write(dir: &Path, record: &Record) -> Result<()> {
    let target = path(dir);
    let temporary = dir.join("state/run.json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
    fs::rename(temporary, target)?;
    Ok(())
}

pub fn clear_if_current(dir: &Path, record: &Record) {
    if read(dir).is_ok_and(|current| current.name == record.name) {
        let _ = fs::remove_file(path(dir));
    }
}

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

/// Hold the returned file for the edit; dropping it releases the lock. A running
/// machine holds the same lock for its whole life, so no separate liveness probe is needed.
pub fn lock_for_edit(dir: &Path, action: &str) -> Result<std::fs::File> {
    fs::create_dir_all(dir.join("state"))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("state/machine.lock"))?;
    if file.try_lock().is_err() {
        return Err(format!("stop the machine before {action}").into());
    }
    Ok(file)
}

pub fn is_locked(dir: &Path) -> Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .open(dir.join("state/machine.lock"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if file.try_lock().is_err() {
        return Ok(true);
    }
    file.unlock()?;
    Ok(false)
}

pub fn is_running(dir: &Path) -> Result<bool> {
    match read(dir) {
        Ok(record) => Ok(verified_qmp(&record).is_ok()),
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(false)
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
