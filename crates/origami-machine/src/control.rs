use crate::Result;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io;
use std::net::TcpListener;
use std::path::Path;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Record {
    /// The runner's process id.
    pub pid: u32,
    /// QEMU's own QMP endpoint. The runner never holds it open.
    pub qmp_port: u16,
    /// The runner's raw console port. An older origami's record has QEMU's
    /// own console here.
    pub console_port: u16,
    pub name: String,
    /// QEMU's own process id. Zero in a record an older origami wrote.
    #[serde(default)]
    pub qemu_pid: u32,
    /// The runner's control socket, loopback TCP. Zero when absent.
    #[serde(default)]
    pub control_port: u16,
    /// QEMU's console chardev, which only the runner connects to. Zero
    /// when absent.
    #[serde(default)]
    pub engine_console: u16,
    /// `cli` or `app`; empty in a record an older origami wrote.
    #[serde(default)]
    pub started_by: String,
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
