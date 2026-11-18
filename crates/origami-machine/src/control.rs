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
    /// The shared-memory display protocol the machine speaks, 3 or 4; zero
    /// when it has none or an older origami wrote the record.
    #[serde(default)]
    pub display_protocol: u32,
    /// How long `logs/serial.log` was when this run began, so a console that
    /// connects later replays only this run's output. Zero when absent.
    #[serde(default)]
    pub serial_from: u64,
}

/// How a runner's machine ended, left in `state/exit.json` for a viewer; the
/// next run removes it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub last_error: Option<String>,
    /// `stop` or `force` when something asked for the end, from any process.
    #[serde(default)]
    pub requested: Option<String>,
}

fn stop_note_path(dir: &Path) -> std::path::PathBuf {
    dir.join("state/stop-request")
}

/// Record that a process is about to stop the machine, `stop` or `force`, so
/// the runner can say so when it leaves its exit note.
pub fn note_stop(dir: &Path, how: &str) {
    if let Err(error) = fs::write(stop_note_path(dir), how) {
        eprintln!("origami: cannot note the stop request: {error}");
    }
}

/// The stop a process asked for during this run, if any.
pub fn read_stop_note(dir: &Path) -> Option<String> {
    fs::read_to_string(stop_note_path(dir)).ok()
}

fn exit_path(dir: &Path) -> std::path::PathBuf {
    dir.join("state/exit.json")
}

pub fn write_exit(dir: &Path, exit: &Exit) -> Result<()> {
    Ok(fs::write(exit_path(dir), serde_json::to_vec(exit)?)?)
}

pub fn read_exit(dir: &Path) -> Option<Exit> {
    serde_json::from_slice(&fs::read(exit_path(dir)).ok()?).ok()
}

pub fn clear_exit(dir: &Path) {
    let _ = fs::remove_file(exit_path(dir));
    let _ = fs::remove_file(stop_note_path(dir));
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

/// A lock on a machine or the serial minting, released when dropped.
pub struct EditLock(pub(crate) fs::File);

impl Drop for EditLock {
    // Closing alone is not enough: a child forked meanwhile holds the
    // descriptor, and with it the lock, until it execs.
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Hold the returned lock for the edit. A running machine holds the same lock
/// for its whole life, so no separate liveness probe is needed.
pub fn lock_for_edit(dir: &Path, action: &str) -> Result<EditLock> {
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
    Ok(EditLock(file))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exit_note_is_read_back_and_cleared() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("state")).unwrap();
        assert_eq!(read_exit(root.path()), None);
        let exit = Exit {
            code: None,
            signal: Some(9),
            last_error: Some("killed".into()),
            requested: Some("force".into()),
        };
        write_exit(root.path(), &exit).unwrap();
        assert_eq!(read_exit(root.path()), Some(exit));
        clear_exit(root.path());
        assert_eq!(read_exit(root.path()), None);
    }

    /// A child forked by another thread shares the lock until it execs.
    #[cfg(unix)]
    #[test]
    fn a_child_forked_during_an_edit_does_not_keep_its_lock() {
        use std::io::{Read, Write};
        use std::os::unix::process::CommandExt;
        let root = tempfile::tempdir().unwrap();
        let lock = lock_for_edit(root.path(), "t").unwrap();
        let (mut forked, forked_tx) = io::pipe().unwrap();
        let (go_rx, mut go) = io::pipe().unwrap();
        let mut command = std::process::Command::new("true");
        // SAFETY: the hook only reads and writes pipes.
        unsafe {
            command.pre_exec(move || {
                (&forked_tx).write_all(b"x")?;
                (&go_rx).read_exact(&mut [0])
            });
        }
        let child = std::thread::spawn(move || command.status());
        forked.read_exact(&mut [0]).unwrap();
        drop(lock);
        let again = lock_for_edit(root.path(), "t");
        go.write_all(b"x").unwrap();
        assert!(child.join().unwrap().unwrap().success());
        again.unwrap();
    }
}
