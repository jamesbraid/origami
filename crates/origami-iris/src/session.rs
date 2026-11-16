//! [`IrisSession`]: a machine hosted by a runner process, driven over the
//! runner's control socket.

use crate::engine::IrisPlan;
use origami_engine::{
    ConsoleInfo, ConsoleIo, Control, DisplayEndpoint, Facts, MachineId, Result, RunState, Session,
    Stop, Wake,
};
use origami_machine::control::{read, Record};
use origami_protocol::{Client, Request};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The only console name: the primary serial line.
pub const CONSOLE: &str = "console";

const POLL: Duration = Duration::from_millis(25);
const CONNECT: Duration = Duration::from_secs(2);
/// How long the runner has to build the machine and answer.
const READY: Duration = Duration::from_secs(60);

struct Exit {
    code: Option<i32>,
    last_error: Option<String>,
}

struct Inner {
    child: Mutex<Child>,
    exit: Mutex<Option<Exit>>,
    exited: Condvar,
}

pub struct IrisSession {
    id: MachineId,
    inner: Arc<Inner>,
    record: Record,
    removable: Vec<String>,
    display: Option<PathBuf>,
    console_taken: AtomicBool,
    client: Mutex<Option<Client>>,
}

/// Start `_serve` for the machine in `dir` and return once it answers.
pub(crate) fn launch(
    id: MachineId,
    dir: &Path,
    plan: &IrisPlan,
    wake: Wake,
) -> Result<Arc<IrisSession>> {
    if !plan.runner.is_file() {
        return Err(format!("the runner is missing: {}", plan.runner.display()).into());
    }
    fs::create_dir_all(dir.join("logs"))?;
    let machine_file = dir.join("state/iris-launch.toml");
    fs::write(&machine_file, &plan.machine)?;
    let log_path = dir.join("logs/runner.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_start = log.metadata()?.len();
    let mut command = Command::new(&plan.runner);
    command
        .arg("_serve")
        .arg(dir)
        .args(["--display", if plan.display { "shm" } else { "none" }])
        .args(["--started-by", "app", "--machine-file"])
        .arg(&machine_file);
    if plan.hold {
        command.arg("--hold");
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()?;
    let pid = child.id();
    let inner = Arc::new(Inner {
        child: Mutex::new(child),
        exit: Mutex::new(None),
        exited: Condvar::new(),
    });
    let ended = |inner: &Inner| -> Option<String> {
        let status = inner.child.lock().unwrap().try_wait().ok().flatten()?;
        Some(exit_text(&status, &log_path, log_start))
    };
    let deadline = Instant::now() + READY;
    let (record, client) = loop {
        if let Ok(record) = read(dir) {
            if record.pid == pid && record.control_port != 0 {
                if let Ok(mut client) = Client::connect(record.control_port, CONNECT) {
                    if client.ping().is_ok() {
                        break (record, client);
                    }
                }
            }
        }
        if let Some(why) = ended(&inner) {
            return Err(format!("the iris runner failed to start: {why}").into());
        }
        if Instant::now() >= deadline {
            let _ = inner.child.lock().unwrap().kill();
            let _ = inner.child.lock().unwrap().wait();
            return Err(
                format!("the iris runner did not answer; see {}", log_path.display()).into(),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let session = Arc::new(IrisSession {
        id,
        inner: inner.clone(),
        record,
        removable: plan.removable.clone(),
        display: plan.display.then(|| crate::display_socket(dir)),
        console_taken: AtomicBool::new(false),
        client: Mutex::new(Some(client)),
    });
    std::thread::spawn(move || monitor(&inner, &log_path, log_start, &wake, id));
    Ok(session)
}

/// How the runner ended, for a person: its last line of output, else the
/// signal or status.
fn exit_text(status: &std::process::ExitStatus, log: &Path, start: u64) -> String {
    match (last_line(log, start), signal_of(status)) {
        (Some(line), Some(signal)) => {
            format!("the runner was ended by signal {signal}; its last line: {line}")
        }
        (None, Some(signal)) => format!("the runner was ended by signal {signal}"),
        (Some(line), None) => line,
        (None, None) => format!("the runner exited with {status}"),
    }
}

#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

fn last_line(path: &Path, start: u64) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(bytes.get(start as usize..)?);
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// Watch the runner until it exits, record how, and say so.
fn monitor(inner: &Inner, log: &Path, start: u64, wake: &Wake, id: MachineId) {
    let status = loop {
        match inner.child.lock().unwrap().try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => break Some(status),
            Err(_) => break None,
        }
        std::thread::sleep(POLL);
    };
    let success = status.is_some_and(|status| status.success());
    let exit = Exit {
        code: status.and_then(|status| status.code()),
        last_error: status
            .filter(|_| !success)
            .map(|status| exit_text(&status, log, start)),
    };
    *inner.exit.lock().unwrap() = Some(exit);
    inner.exited.notify_all();
    wake(id);
}

impl IrisSession {
    fn exited(&self) -> bool {
        self.inner.exit.lock().unwrap().is_some()
    }

    fn call(&self, request: &Request) -> Result<Value> {
        self.call_for(request, Duration::from_secs(10))
    }

    /// One request to the runner, on a connection kept between calls.
    fn call_for(&self, request: &Request, timeout: Duration) -> Result<Value> {
        if self.exited() {
            return Err("the machine has exited".into());
        }
        let mut slot = self.client.lock().unwrap();
        for attempt in 0..2 {
            if slot.is_none() {
                *slot = Some(Client::connect(self.record.control_port, CONNECT)?);
            }
            let client = slot.as_mut().expect("connected above");
            let result = client.call_data_timeout(request, timeout);
            client.take_events();
            match result {
                Ok(data) => return Ok(data),
                // An engine's refusal is an answer; a dead connection is not.
                Err(error)
                    if attempt == 1
                        || matches!(error, origami_protocol::ClientError::Remote { .. }) =>
                {
                    return Err(error.into())
                }
                Err(_) => *slot = None,
            }
        }
        unreachable!("the second attempt returns")
    }

    /// Wait for the exit up to `timeout`; true when it happened.
    fn wait_exit(&self, timeout: Option<Duration>) -> bool {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let mut exit = self.inner.exit.lock().unwrap();
        while exit.is_none() {
            let wait = match deadline {
                None => Duration::from_secs(3600),
                Some(deadline) => match deadline.checked_duration_since(Instant::now()) {
                    Some(left) => left,
                    None => return false,
                },
            };
            exit = self.inner.exited.wait_timeout(exit, wait).unwrap().0;
        }
        true
    }
}

impl Session for IrisSession {
    fn id(&self) -> MachineId {
        self.id
    }

    fn display(&self) -> DisplayEndpoint {
        match &self.display {
            Some(socket) => DisplayEndpoint::Socket(socket.clone()),
            None => DisplayEndpoint::None,
        }
    }

    fn consoles(&self) -> Vec<ConsoleInfo> {
        vec![ConsoleInfo {
            name: CONSOLE.into(),
            primary: true,
        }]
    }

    fn open_console(&self, name: &str) -> Result<Box<dyn ConsoleIo>> {
        if name != CONSOLE {
            return Err(format!("no console named {name}").into());
        }
        if self.console_taken.swap(true, Ordering::SeqCst) {
            return Err("the console is already open".into());
        }
        // The runner's raw console port: it connects iris's serial line on
        // the first client and shares it.
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, self.record.console_port);
        match TcpStream::connect_timeout(&address.into(), Duration::from_secs(5)) {
            Ok(stream) => Ok(Box::new(stream)),
            Err(error) => {
                self.console_taken.store(false, Ordering::SeqCst);
                Err(error.into())
            }
        }
    }

    fn control(&self, c: Control) -> Result<()> {
        let request = match c {
            Control::Reset => Request::Reset {},
            Control::Pause => Request::Pause {},
            Control::Resume => Request::Resume {},
            Control::PowerButton => return Err("iris does not model a power button".into()),
            Control::Nmi => return Err("iris does not model an NMI".into()),
        };
        self.call(&request).map(|_| ())
    }

    fn media(&self, drive: &str, image: Option<&Path>) -> Result<()> {
        if !self.removable.iter().any(|name| name == drive) {
            return Err(format!("{drive} is not a removable drive of this machine").into());
        }
        let drive = Some(drive.to_owned());
        self.call(&match image {
            Some(image) => Request::CdromLoad {
                path: image.display().to_string(),
                drive,
            },
            None => Request::CdromEject { drive },
        })
        .map(|_| ())
    }

    fn screendump(&self, head: usize, path: &Path) -> Result<()> {
        // The first picture waits for the PROM to set a video mode.
        let request = Request::Screenshot {
            head: head as u32,
            path: path.display().to_string(),
        };
        self.call_for(&request, Duration::from_secs(40)).map(|_| ())
    }

    fn stop(&self, how: Stop) -> Result<()> {
        match how {
            Stop::Clean { timeout } => {
                if !self.exited() {
                    let request = Request::Stop {
                        clean: true,
                        timeout: timeout.map(|t| t.as_secs()),
                    };
                    let _ = self.call(&request);
                }
                if !self.wait_exit(timeout) {
                    return Err("the runner is still exiting; it was not killed".into());
                }
                match &*self.inner.exit.lock().unwrap() {
                    Some(Exit { code: Some(0), .. }) => Ok(()),
                    Some(Exit { last_error, .. }) => Err(last_error
                        .clone()
                        .unwrap_or_else(|| "the runner did not exit cleanly".into())
                        .into()),
                    None => Ok(()),
                }
            }
            Stop::Force => {
                let _ = self.inner.child.lock().unwrap().kill();
                if self.wait_exit(Some(Duration::from_secs(30))) {
                    Ok(())
                } else {
                    Err("the runner was killed but has not exited after 30 seconds".into())
                }
            }
        }
    }

    fn status(&self) -> Facts {
        if let Some(exit) = &*self.inner.exit.lock().unwrap() {
            return Facts {
                state: RunState::Exited {
                    code: exit.code,
                    last_error: exit.last_error.clone(),
                },
                pid: None,
            };
        }
        let state = match self.call(&Request::Status {}) {
            Ok(data) => match data["state"].as_str() {
                Some("prelaunch") => RunState::Prelaunch,
                Some("paused") => RunState::Paused,
                Some("unknown") | None => RunState::Unknown("the runner gave no state".into()),
                _ => RunState::Running,
            },
            Err(error) => RunState::Unknown(error.to_string()),
        };
        Facts {
            state,
            pid: Some(self.record.pid),
        }
    }
}
