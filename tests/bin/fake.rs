//! A stand-in for the QEMU runtime and Instigator, for tests that need the
//! product to launch, talk to and stop a machine without emulating one.
//!
//! One binary plays four programs, chosen by the name it is run under
//! (`qemu-system-mips64`, `qemu-img`, `qemu-sgi-machine-init` or
//! `instigator`, with or without `.exe`). Test support links it into a
//! temporary runtime directory under each name. It is built for tests only
//! and the release archive never carries it.
//!
//! It speaks the four interfaces the product uses:
//!
//! - arguments: `qemu-system-mips64` records its argv in `logs/fake-qemu.log`
//!   in its working directory, which is the machine directory;
//! - QMP: `-qmp stdio` answers the machine list; `-qmp tcp:` answers
//!   `query-name`, `query-status`, `cont`, `stop`, `system_powerdown` and
//!   `quit`;
//! - the console: a `-chardev socket` with `server=on` plays a guest script
//!   (see below) to each client;
//! - stream netdevs: a `-netdev stream` client connects to its address and a
//!   server binds it, and the log says when either happened.
//!
//! Misbehaviour is asked for with `ORIGAMI_FAKE`, a comma-separated list:
//!
//! - `exit-early`: QEMU exits with status 1 before opening anything;
//! - `no-qmp`: QEMU stays alive and never opens QMP;
//! - `hang`: QEMU opens QMP, accepts clients and never answers;
//! - `crash-after=S`: QEMU exits with status 70 S seconds after it is ready;
//! - `fold=MS`: after `quit`, QEMU lingers MS milliseconds before it exits,
//!   as it does while it folds a CHD disk's changes into the image;
//! - `guest=NAME` or `guest-file=PATH`: the guest script, one of the
//!   built-in ones (`login`, `install-6.5.30-minimal`) or a file;
//! - `lifetime=S`: exit after S seconds regardless (default 120), so a
//!   test that fails cannot leave a machine behind;
//! - `check-fails`: `instigator check` rejects its input;
//! - `serve-fails`: `instigator serve` writes to its output and its capture
//!   directory, then exits with status 7.
//!
//! A guest script is one directive per line. `#` starts a comment line.
//! Text takes `\r`, `\n`, `\t`, `\\` and `\xHH`.
//!
//! - `out TEXT`: the guest writes TEXT;
//! - `outln TEXT`: the same, then a carriage return and line feed;
//! - `in TEXT`: the guest waits until the host has sent TEXT, and
//!   consumes the input through it;
//! - `wait MS`: pause;
//! - `close`: the guest closes the console.
//!
//! When the script ends the console stays open and discards input, as a
//! running guest's does.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// The machine list, as QEMU's `query-sgi-machines` returns it.
const CATALOGUE: &str = include_str!("../fixtures/sgi-machines.json");

const GUEST_LOGIN: &str = include_str!("../fixtures/guests/login.txt");
const GUEST_INSTALL: &str = include_str!("../fixtures/guests/install-6.5.30-minimal.txt");

const VERSION: &str = "0.0.0 (origami fake)";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let Some(program) = args.first() else {
        return ExitCode::FAILURE;
    };
    let name = Path::new(program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let result = match name.as_str() {
        "qemu-system-mips64" => qemu(&args),
        "qemu-img" => qemu_img(&args),
        "qemu-sgi-machine-init" => machine_init(&args),
        "instigator" => instigator(&args),
        other => Err(format!("origami-fake runs as one of qemu-system-mips64, qemu-img, qemu-sgi-machine-init or instigator, not {other}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{name}: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The `ORIGAMI_FAKE` switches.
#[derive(Default)]
struct Switches(BTreeMap<String, String>);

impl Switches {
    fn from_environment() -> Self {
        let text = std::env::var("ORIGAMI_FAKE").unwrap_or_default();
        Self(
            text.split(',')
                .filter(|item| !item.is_empty())
                .map(|item| match item.split_once('=') {
                    Some((key, value)) => (key.to_owned(), value.to_owned()),
                    None => (item.to_owned(), String::new()),
                })
                .collect(),
        )
    }

    fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    fn number(&self, key: &str) -> Option<u64> {
        self.0.get(key)?.parse().ok()
    }
}

// ---------------------------------------------------------------- QEMU

/// The parts of QEMU's command line the product depends on.
#[derive(Default)]
struct QemuArgs {
    qmp: Option<String>,
    name: Option<String>,
    paused: bool,
    console: Option<Endpoint>,
    netdevs: Vec<(String, Endpoint)>,
    version: bool,
}

/// A socket address in a `-qmp`, `-chardev` or `-netdev` option.
#[derive(Clone, Debug)]
enum Endpoint {
    Tcp {
        address: String,
        server: bool,
    },
    #[cfg_attr(not(unix), allow(dead_code))]
    Unix {
        path: PathBuf,
        server: bool,
    },
}

/// `key=value,key=value` with an optional leading bare word. QEMU writes a
/// comma inside a value as two.
fn options(text: &str) -> BTreeMap<String, String> {
    let mut items = vec![String::new()];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ',' {
            if chars.peek() == Some(&',') {
                chars.next();
                items.last_mut().unwrap().push(',');
            } else {
                items.push(String::new());
            }
        } else {
            items.last_mut().unwrap().push(c);
        }
    }
    items
        .into_iter()
        .filter(|item| !item.is_empty())
        .map(|item| match item.split_once('=') {
            Some((key, value)) => (key.to_owned(), value.to_owned()),
            None => (item, String::new()),
        })
        .collect()
}

fn parse_qemu(args: &[String]) -> Result<QemuArgs, String> {
    let mut parsed = QemuArgs::default();
    let mut index = 0;
    while index < args.len() {
        let option = args[index].as_str();
        let value = args.get(index + 1);
        match (option, value) {
            ("-S", _) => parsed.paused = true,
            ("--version" | "-version", _) => parsed.version = true,
            ("-qmp", Some(value)) => {
                parsed.qmp = Some(value.clone());
                index += 1;
            }
            ("-name", Some(value)) => {
                parsed.name = Some(value.split(',').next().unwrap_or_default().to_owned());
                index += 1;
            }
            ("-chardev", Some(value)) => {
                let items = options(value);
                if items.contains_key("socket")
                    && items.get("id").is_some_and(|id| id == "origami-console")
                {
                    parsed.console = Some(socket_endpoint(&items)?);
                }
                index += 1;
            }
            ("-netdev", Some(value)) => {
                let items = options(value);
                if items.contains_key("stream") {
                    let id = items.get("id").cloned().unwrap_or_default();
                    parsed.netdevs.push((id, stream_endpoint(&items)?));
                }
                index += 1;
            }
            _ => {}
        }
        index += 1;
    }
    Ok(parsed)
}

fn socket_endpoint(items: &BTreeMap<String, String>) -> Result<Endpoint, String> {
    let server = items.get("server").is_some_and(|value| value != "off");
    if let Some(path) = items.get("path") {
        return Ok(Endpoint::Unix {
            path: path.into(),
            server,
        });
    }
    let host = items.get("host").ok_or("a socket chardev needs a host")?;
    let port = items.get("port").ok_or("a socket chardev needs a port")?;
    Ok(Endpoint::Tcp {
        address: format!("{host}:{port}"),
        server,
    })
}

fn stream_endpoint(items: &BTreeMap<String, String>) -> Result<Endpoint, String> {
    let server = items.get("server").is_some_and(|value| value != "off");
    match items.get("addr.type").map(String::as_str) {
        Some("inet") => Ok(Endpoint::Tcp {
            address: format!(
                "{}:{}",
                items
                    .get("addr.host")
                    .ok_or("a stream netdev needs addr.host")?,
                items
                    .get("addr.port")
                    .ok_or("a stream netdev needs addr.port")?
            ),
            server,
        }),
        Some("unix") => Ok(Endpoint::Unix {
            path: items
                .get("addr.path")
                .ok_or("a stream netdev needs addr.path")?
                .into(),
            server,
        }),
        other => Err(format!("unsupported stream netdev address type {other:?}")),
    }
}

/// Lines describing what the fake did, in `logs/fake-qemu.log`.
#[derive(Clone)]
struct Log(Arc<Mutex<Option<fs::File>>>);

impl Log {
    fn open() -> Self {
        let _ = fs::create_dir_all("logs");
        Self(Arc::new(Mutex::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open("logs/fake-qemu.log")
                .ok(),
        )))
    }

    fn line(&self, text: impl AsRef<str>) {
        if let Some(file) = self.0.lock().unwrap().as_mut() {
            let _ = writeln!(file, "{}", text.as_ref());
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Run {
    Prelaunch,
    Running,
    Paused,
}

impl Run {
    fn status(self) -> &'static str {
        match self {
            Run::Prelaunch => "prelaunch",
            Run::Running => "running",
            Run::Paused => "paused",
        }
    }
}

fn qemu(args: &[String]) -> Result<(), String> {
    let parsed = parse_qemu(args)?;
    if parsed.version {
        println!("QEMU emulator version {VERSION}");
        return Ok(());
    }
    let switches = Switches::from_environment();
    match parsed.qmp.as_deref() {
        Some("stdio") => return catalogue(),
        Some(value) if value.starts_with("tcp:") => {}
        _ => return foreground(&switches),
    }
    let log = Log::open();
    log.line(format!("argv {}", json!(args)));
    if switches.has("exit-early") {
        return Err("exiting early, as asked".into());
    }
    let lifetime = Duration::from_secs(switches.number("lifetime").unwrap_or(120));
    thread::spawn(move || {
        thread::sleep(lifetime);
        std::process::exit(71);
    });
    if switches.has("no-qmp") {
        log.line("no-qmp");
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }
    let qmp_address = parsed
        .qmp
        .as_deref()
        .and_then(|value| value.strip_prefix("tcp:"))
        .and_then(|value| value.split(',').next())
        .ok_or("-qmp tcp: needs an address")?
        .to_owned();
    let listener =
        TcpListener::bind(&qmp_address).map_err(|error| format!("{qmp_address}: {error}"))?;
    if let Some(console) = parsed.console.clone() {
        let script = guest_script(&switches)?;
        let log = log.clone();
        let listener = bind(&console).map_err(|error| format!("console: {error}"))?;
        thread::spawn(move || accept_loop(listener, move |stream| play(stream, &script, &log)));
    }
    for (id, endpoint) in parsed.netdevs.clone() {
        let log = log.clone();
        thread::spawn(move || join_netdev(&id, &endpoint, &log));
    }
    if let Some(seconds) = switches.number("crash-after") {
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(seconds));
            std::process::exit(70);
        });
    }
    log.line("ready");
    let state = Arc::new(Mutex::new(if parsed.paused {
        Run::Prelaunch
    } else {
        Run::Running
    }));
    let name = parsed.name.unwrap_or_default();
    let hang = switches.has("hang");
    let fold = Duration::from_millis(switches.number("fold").unwrap_or(0));
    for stream in listener.incoming().flatten() {
        let (state, name, log) = (state.clone(), name.clone(), log.clone());
        thread::spawn(move || {
            if hang {
                // Accept and say nothing, as QEMU does when it is wedged.
                let mut sink = [0u8; 256];
                let mut stream = stream;
                while stream.read(&mut sink).is_ok_and(|count| count > 0) {}
            } else {
                qmp_session(stream, &state, &name, fold, &log);
            }
        });
    }
    Ok(())
}

/// `-qmp stdio` with no machine: the machine list, and nothing else.
fn catalogue() -> Result<(), String> {
    let catalogue: Value = serde_json::from_str(CATALOGUE).map_err(|error| error.to_string())?;
    let mut out = io::stdout();
    let reply = |out: &mut io::Stdout, value: Value| {
        let _ = writeln!(out, "{value}");
        let _ = out.flush();
    };
    reply(&mut out, greeting());
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let command = command_name(&line);
        match command.as_str() {
            "query-sgi-machines" => reply(&mut out, json!({"return": catalogue})),
            "quit" => {
                reply(&mut out, json!({"return": {}}));
                break;
            }
            _ => reply(&mut out, json!({"return": {}})),
        }
    }
    Ok(())
}

/// A run without a QMP endpoint is a foreground run: the serial console is
/// this process's standard streams.
fn foreground(switches: &Switches) -> Result<(), String> {
    if switches.has("exit-early") {
        return Err("exiting early, as asked".into());
    }
    let script = guest_script(switches)?;
    let log = Log::open();
    log.line("foreground");
    let mut input = Vec::new();
    let _ = io::stdin().read_to_end(&mut input);
    // Standard input is not a socket, so play the script against what it
    // held: enough for a run that only shows the guest's output.
    let mut sink = io::stdout();
    for step in parse_script(&script)? {
        match step {
            Step::Out(text) => {
                let _ = sink.write_all(&text);
                let _ = sink.flush();
            }
            Step::Close => break,
            Step::In(_) | Step::Wait(_) => {}
        }
    }
    Ok(())
}

fn greeting() -> Value {
    json!({"QMP": {"version": {"qemu": {"major": 0, "minor": 0, "micro": 0}, "package": ""}, "capabilities": []}})
}

fn command_name(line: &str) -> String {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|value| value["execute"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn qmp_session(stream: TcpStream, state: &Mutex<Run>, name: &str, fold: Duration, log: &Log) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut send = |value: Value| {
        let _ = writeln!(writer, "{value}");
        let _ = writer.flush();
    };
    send(greeting());
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        let command = command_name(&line);
        log.line(format!("qmp {command}"));
        match command.as_str() {
            "query-name" => send(json!({"return": {"name": name}})),
            "query-status" => {
                let run = *state.lock().unwrap();
                send(json!({"return": {
                    "status": run.status(),
                    "running": run == Run::Running,
                    "singlestep": false,
                }}));
            }
            "cont" => {
                *state.lock().unwrap() = Run::Running;
                send(json!({"return": {}}));
            }
            "stop" => {
                *state.lock().unwrap() = Run::Paused;
                send(json!({"return": {}}));
            }
            "quit" | "system_powerdown" => {
                send(json!({"return": {}}));
                // Like QEMU folding a CHD disk: the process outlives the
                // reply, and QMP stops answering.
                thread::sleep(fold);
                log.line("exit");
                std::process::exit(0);
            }
            "" => send(json!({"error": {"class": "GenericError", "desc": "not a QMP command"}})),
            "qmp_capabilities" => send(json!({"return": {}})),
            other => send(json!({"error": {
                "class": "CommandNotFound",
                "desc": format!("The command {other} has not been found"),
            }})),
        }
    }
}

enum Listener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener),
}

enum Connection {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

fn bind(endpoint: &Endpoint) -> io::Result<Listener> {
    match endpoint {
        Endpoint::Tcp { address, .. } => Ok(Listener::Tcp(TcpListener::bind(address)?)),
        #[cfg(unix)]
        Endpoint::Unix { path, .. } => {
            let _ = fs::remove_file(path);
            Ok(Listener::Unix(std::os::unix::net::UnixListener::bind(
                path,
            )?))
        }
        #[cfg(not(unix))]
        Endpoint::Unix { .. } => Err(io::Error::other("unix sockets need a unix host")),
    }
}

fn connect(endpoint: &Endpoint) -> io::Result<Connection> {
    match endpoint {
        Endpoint::Tcp { address, .. } => Ok(Connection::Tcp(TcpStream::connect(address)?)),
        #[cfg(unix)]
        Endpoint::Unix { path, .. } => Ok(Connection::Unix(
            std::os::unix::net::UnixStream::connect(path)?,
        )),
        #[cfg(not(unix))]
        Endpoint::Unix { .. } => Err(io::Error::other("unix sockets need a unix host")),
    }
}

/// Serve every client of `listener` with `serve`, one thread each.
fn accept_loop(listener: Listener, serve: impl Fn(TcpStream) + Send + Sync + 'static) {
    let serve = Arc::new(serve);
    match listener {
        Listener::Tcp(listener) => {
            for stream in listener.incoming().flatten() {
                let serve = serve.clone();
                thread::spawn(move || serve(stream));
            }
        }
        #[cfg(unix)]
        Listener::Unix(listener) => {
            // Only TCP carries a console or a script; hold the others open.
            for stream in listener.incoming().flatten() {
                thread::spawn(move || {
                    let mut stream = stream;
                    let mut sink = [0u8; 4096];
                    while stream.read(&mut sink).is_ok_and(|count| count > 0) {}
                });
            }
        }
    }
}

/// Attach to a stream netdev: connect to its address, retrying as QEMU does,
/// or bind it and wait for the peer. Frames are read and dropped.
fn join_netdev(id: &str, endpoint: &Endpoint, log: &Log) {
    let server = matches!(
        endpoint,
        Endpoint::Tcp { server: true, .. } | Endpoint::Unix { server: true, .. }
    );
    let connection = if server {
        let Ok(listener) = bind(endpoint) else {
            log.line(format!("netdev {id} cannot bind"));
            return;
        };
        match listener {
            Listener::Tcp(listener) => listener
                .accept()
                .ok()
                .map(|(stream, _)| Connection::Tcp(stream)),
            #[cfg(unix)]
            Listener::Unix(listener) => listener
                .accept()
                .ok()
                .map(|(stream, _)| Connection::Unix(stream)),
        }
    } else {
        loop {
            match connect(endpoint) {
                Ok(connection) => break Some(connection),
                Err(_) => thread::sleep(Duration::from_millis(50)),
            }
        }
    };
    let Some(connection) = connection else { return };
    log.line(format!("netdev {id} connected"));
    let mut sink = [0u8; 4096];
    match connection {
        Connection::Tcp(mut stream) => while stream.read(&mut sink).is_ok_and(|count| count > 0) {},
        #[cfg(unix)]
        Connection::Unix(mut stream) => {
            while stream.read(&mut sink).is_ok_and(|count| count > 0) {}
        }
    }
    log.line(format!("netdev {id} closed"));
}

// ------------------------------------------------------- guest scripts

enum Step {
    Out(Vec<u8>),
    In(Vec<u8>),
    Wait(u64),
    Close,
}

fn guest_script(switches: &Switches) -> Result<String, String> {
    if let Some(path) = switches.0.get("guest-file") {
        return fs::read_to_string(path).map_err(|error| format!("{path}: {error}"));
    }
    match switches
        .0
        .get("guest")
        .map(String::as_str)
        .unwrap_or("login")
    {
        "login" => Ok(GUEST_LOGIN.to_owned()),
        "install-6.5.30-minimal" => Ok(GUEST_INSTALL.to_owned()),
        other => Err(format!("no built-in guest named {other}")),
    }
}

fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buffer = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            continue;
        }
        match chars.next() {
            Some('r') => bytes.push(b'\r'),
            Some('n') => bytes.push(b'\n'),
            Some('t') => bytes.push(b'\t'),
            Some('\\') => bytes.push(b'\\'),
            Some('x') => {
                let digits: String = chars.by_ref().take(2).collect();
                bytes.push(
                    u8::from_str_radix(&digits, 16)
                        .map_err(|_| format!("bad escape \\x{digits}"))?,
                );
            }
            other => return Err(format!("bad escape \\{}", other.unwrap_or(' '))),
        }
    }
    Ok(bytes)
}

fn parse_script(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (number, line) in script.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (directive, text) = line.split_once(' ').unwrap_or((line, ""));
        let at = |error: String| format!("guest script line {}: {error}", number + 1);
        steps.push(match directive {
            "out" => Step::Out(unescape(text).map_err(at)?),
            "outln" => {
                let mut bytes = unescape(text).map_err(at)?;
                bytes.extend_from_slice(b"\r\n");
                Step::Out(bytes)
            }
            "in" => Step::In(unescape(text).map_err(at)?),
            "wait" => Step::Wait(
                text.parse()
                    .map_err(|_| at("wait needs milliseconds".into()))?,
            ),
            "close" => Step::Close,
            other => return Err(at(format!("unknown directive {other}"))),
        });
    }
    Ok(steps)
}

/// Run the script against one console client.
fn play(mut stream: TcpStream, script: &str, log: &Log) {
    let steps = match parse_script(script) {
        Ok(steps) => steps,
        Err(error) => {
            log.line(format!("console {error}"));
            return;
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    log.line("console connected");
    let mut pending: Vec<u8> = Vec::new();
    let read = |stream: &mut TcpStream, pending: &mut Vec<u8>| -> bool {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => false,
            Ok(count) => {
                pending.extend_from_slice(&chunk[..count]);
                true
            }
            Err(error) => matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
            ),
        }
    };
    for step in steps {
        match step {
            Step::Out(bytes) => {
                if stream
                    .write_all(&bytes)
                    .and_then(|_| stream.flush())
                    .is_err()
                {
                    return;
                }
            }
            Step::Wait(milliseconds) => thread::sleep(Duration::from_millis(milliseconds)),
            Step::Close => {
                log.line("console closed by the guest");
                return;
            }
            Step::In(wanted) => {
                let started = Instant::now();
                loop {
                    if let Some(at) = find(&pending, &wanted) {
                        pending.drain(..at + wanted.len());
                        break;
                    }
                    if !read(&mut stream, &mut pending) {
                        log.line("console client left");
                        return;
                    }
                    if started.elapsed() > Duration::from_secs(3600) {
                        return;
                    }
                }
            }
        }
    }
    log.line("script finished");
    while read(&mut stream, &mut pending) {
        pending.clear();
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// -------------------------------------------------------------- qemu-img

fn qemu_img(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("qemu-img version {VERSION}");
            Ok(())
        }
        Some("create") => {
            let mut format = "raw".to_owned();
            let mut positional = Vec::new();
            let mut index = 1;
            while index < args.len() {
                match args[index].as_str() {
                    "-f" => {
                        format = args.get(index + 1).ok_or("-f needs a format")?.clone();
                        index += 1;
                    }
                    other if other.starts_with('-') => {}
                    other => positional.push(other.to_owned()),
                }
                index += 1;
            }
            let [path, size] = positional.as_slice() else {
                return Err("usage: create [-f FORMAT] FILE SIZE".into());
            };
            let size = parse_size(size)?;
            match format.as_str() {
                "raw" => {
                    let file =
                        fs::File::create(path).map_err(|error| format!("{path}: {error}"))?;
                    file.set_len(size).map_err(|error| error.to_string())?;
                }
                "qcow2" => fs::write(path, qcow2_header(size))
                    .map_err(|error| format!("{path}: {error}"))?,
                other => return Err(format!("unsupported format {other}")),
            }
            println!("Formatting '{path}', fmt={format} size={size}");
            Ok(())
        }
        Some("info") => {
            let path = args.last().ok_or("usage: info FILE")?;
            let mut head = [0u8; 32];
            let mut file = fs::File::open(path).map_err(|error| format!("{path}: {error}"))?;
            let length = file.metadata().map_err(|error| error.to_string())?.len();
            let read = file.read(&mut head).map_err(|error| error.to_string())?;
            let (format, size) = if read >= 32 && head[..4] == *b"QFI\xfb" {
                (
                    "qcow2",
                    u64::from_be_bytes(head[24..32].try_into().unwrap()),
                )
            } else {
                ("raw", length)
            };
            if args
                .iter()
                .any(|arg| arg.starts_with("--output=json") || arg == "json")
            {
                println!(
                    "{}",
                    json!({"filename": path, "format": format, "virtual-size": size})
                );
            } else {
                println!("image: {path}\nfile format: {format}\nvirtual size: {size} bytes");
            }
            Ok(())
        }
        other => Err(format!("unsupported command {other:?}")),
    }
}

fn parse_size(text: &str) -> Result<u64, String> {
    let digits = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let number: u64 = digits.parse().map_err(|_| format!("bad size {text}"))?;
    let unit = match text[digits.len()..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        "t" => 1 << 40,
        other => return Err(format!("bad size unit {other}")),
    };
    Ok(number * unit)
}

/// A qcow2 version 3 header for an empty image of `size` bytes: four 64 KiB
/// clusters holding the header, the refcount table, one refcount block and
/// the L1 table at the offsets the header names. QEMU opens it and finds
/// no data clusters.
fn qcow2_header(size: u64) -> Vec<u8> {
    const CLUSTER: u64 = 1 << 16;
    let l2_entries = CLUSTER / 8;
    let l1_size = size.div_ceil(CLUSTER * l2_entries).max(1);
    let mut header = Vec::with_capacity(4 * CLUSTER as usize);
    header.extend_from_slice(b"QFI\xfb");
    header.extend_from_slice(&3u32.to_be_bytes()); // version
    header.extend_from_slice(&0u64.to_be_bytes()); // backing file offset
    header.extend_from_slice(&0u32.to_be_bytes()); // backing file size
    header.extend_from_slice(&16u32.to_be_bytes()); // cluster bits
    header.extend_from_slice(&size.to_be_bytes());
    header.extend_from_slice(&0u32.to_be_bytes()); // no encryption
    header.extend_from_slice(&(l1_size as u32).to_be_bytes());
    header.extend_from_slice(&(3 * CLUSTER).to_be_bytes()); // L1 table
    header.extend_from_slice(&CLUSTER.to_be_bytes()); // refcount table
    header.extend_from_slice(&1u32.to_be_bytes()); // refcount table clusters
    header.extend_from_slice(&0u32.to_be_bytes()); // snapshots
    header.extend_from_slice(&0u64.to_be_bytes()); // snapshots offset
    header.extend_from_slice(&0u64.to_be_bytes()); // incompatible features
    header.extend_from_slice(&0u64.to_be_bytes()); // compatible features
    header.extend_from_slice(&0u64.to_be_bytes()); // autoclear features
    header.extend_from_slice(&4u32.to_be_bytes()); // refcount order
    header.extend_from_slice(&104u32.to_be_bytes()); // header length
    header.resize(4 * CLUSTER as usize, 0);
    header
}

// ------------------------------------------------- qemu-sgi-machine-init

/// Create zero-filled storage for the offering named by `--machine`, as
/// the init tool does, and record the arguments beside the tool.
fn machine_init(args: &[String]) -> Result<(), String> {
    // The tool is run by its path in the runtime directory; a link to the
    // fake would otherwise record beside the fake's own binary.
    let tool = std::env::args_os()
        .next()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::current_exe().ok());
    if let Some(directory) = tool.as_deref().and_then(Path::parent) {
        let _ = fs::write(
            directory.join("qemu-sgi-machine-init.args"),
            args.join("\n") + "\n",
        );
    }
    let mut machine = None;
    let mut proms = Vec::new();
    let mut state = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--machine" => {
                machine = args.get(index + 1);
                index += 1;
            }
            "--boot-prom" | "--io-prom" => {
                proms.push(args.get(index + 1).ok_or("a PROM option needs an image")?);
                index += 1;
            }
            other => state = Some(other),
        }
        index += 1;
    }
    let machine = machine.ok_or("--machine is required")?;
    let state = state.ok_or("a state directory is required")?;
    for prom in proms {
        if !Path::new(prom).is_file() {
            return Err(format!("cannot read {prom}"));
        }
    }
    let catalogue: Value = serde_json::from_str(CATALOGUE).map_err(|error| error.to_string())?;
    let offering = catalogue["offerings"]
        .as_array()
        .and_then(|all| {
            all.iter()
                .find(|o| o["machine-options"].as_str() == Some(machine))
        })
        .ok_or_else(|| format!("unknown machine {machine}"))?;
    fs::create_dir(state).map_err(|error| format!("{state}: {error}"))?;
    for item in offering["storage"].as_array().into_iter().flatten() {
        let name = item["name"].as_str().ok_or("storage without a name")?;
        let size = item["size"].as_u64().ok_or("storage without a size")?;
        fs::File::create(Path::new(state).join(format!("{name}.raw")))
            .and_then(|file| file.set_len(size))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

// ------------------------------------------------------------- Instigator

fn instigator(args: &[String]) -> Result<(), String> {
    let switches = Switches::from_environment();
    match args.first().map(String::as_str) {
        Some("--version") | Some("version") => {
            println!("instigator version {VERSION}");
            Ok(())
        }
        Some("check") => {
            let mut input = String::new();
            io::stdin()
                .read_to_string(&mut input)
                .map_err(|error| error.to_string())?;
            serde_json::from_str::<Value>(&input)
                .map_err(|error| format!("configuration is not JSON: {error}"))?;
            if switches.has("check-fails") {
                return Err("install media rejected, as asked".into());
            }
            println!("ok");
            Ok(())
        }
        Some("serve") => {
            let mut endpoint = None;
            let mut capture = None;
            let mut index = 1;
            while index < args.len() {
                match args[index].as_str() {
                    "--network-tcp" => {
                        endpoint = Some(Endpoint::Tcp {
                            address: args
                                .get(index + 1)
                                .ok_or("--network-tcp needs an address")?
                                .clone(),
                            server: true,
                        });
                        index += 1;
                    }
                    "--network-socket" => {
                        endpoint = Some(Endpoint::Unix {
                            path: args
                                .get(index + 1)
                                .ok_or("--network-socket needs a path")?
                                .into(),
                            server: true,
                        });
                        index += 1;
                    }
                    "--capture-dir" => {
                        capture = args.get(index + 1).map(PathBuf::from);
                        index += 1;
                    }
                    _ => {}
                }
                index += 1;
            }
            let endpoint = endpoint.ok_or("serve needs --network-tcp or --network-socket")?;
            if switches.has("serve-fails") {
                // As a server does that dies after leaving a capture behind.
                println!("server stdout");
                eprintln!("server stderr");
                if let Some(capture) = capture {
                    let events = "{\"event\":\"command_exit\",\"elapsed_ms\":125}\n";
                    fs::write(capture.join("events.jsonl"), events)
                        .map_err(|error| error.to_string())?;
                }
                std::process::exit(7);
            }
            let listener = bind(&endpoint).map_err(|error| format!("cannot listen: {error}"))?;
            println!("serving");
            let _ = io::stdout().flush();
            let lifetime = Duration::from_secs(switches.number("lifetime").unwrap_or(120));
            thread::spawn(move || {
                thread::sleep(lifetime);
                std::process::exit(71);
            });
            accept_loop(listener, |stream| {
                println!("peer connected");
                let mut stream = stream;
                let mut sink = [0u8; 4096];
                while stream.read(&mut sink).is_ok_and(|count| count > 0) {}
            });
            Ok(())
        }
        other => Err(format!("unsupported command {other:?}")),
    }
}
