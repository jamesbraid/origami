use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddrV4, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

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

fn response(reader: &mut BufReader<TcpStream>) -> Result<Value> {
    for _ in 0..64 {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err("QMP connection closed".into());
        }
        let value: Value = serde_json::from_str(&line)?;
        if value.get("return").is_some() || value.get("error").is_some() {
            if value.get("error").is_some() {
                return Err(format!("QMP error: {value}").into());
            }
            return Ok(value);
        }
    }
    Err("QMP sent too many events without a response".into())
}

fn execute(stream: &mut TcpStream, reader: &mut BufReader<TcpStream>, name: &str) -> Result<Value> {
    serde_json::to_writer(&mut *stream, &json!({"execute": name}))?;
    stream.write_all(b"\n")?;
    response(reader)
}

pub fn verified_qmp(record: &Record) -> Result<(TcpStream, BufReader<TcpStream>)> {
    let mut stream = connect(record)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut greeting = String::new();
    if reader.read_line(&mut greeting)? == 0 {
        return Err("QMP greeting missing".into());
    }
    if serde_json::from_str::<Value>(&greeting)?
        .get("QMP")
        .is_none()
    {
        return Err("unexpected QMP greeting".into());
    }
    execute(&mut stream, &mut reader, "qmp_capabilities")?;
    let answer = execute(&mut stream, &mut reader, "query-name")?;
    if answer["return"]["name"].as_str() != Some(&record.name) {
        return Err("QMP endpoint belongs to another machine process".into());
    }
    Ok((stream, reader))
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

pub fn stop(dir: &Path) -> Result<()> {
    let record = read(dir)?;
    let (mut stream, _) = verified_qmp(&record)?;
    serde_json::to_writer(&mut stream, &json!({"execute": "quit"}))?;
    stream.write_all(b"\n")?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn edit_lock_excludes_another_writer() {
        let dir = std::env::temp_dir().join(format!(
            "sgi-edit-lock-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let lock = lock_for_edit(&dir, "changing drives").unwrap();
        assert!(is_locked(&dir).unwrap());
        assert!(lock_for_edit(&dir, "changing drives").is_err());
        assert!(crate::runtime::start_background(&dir, crate::runtime::Display::None).is_err());
        drop(lock);
        assert!(!is_locked(&dir).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
}
