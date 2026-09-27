use crate::{control, resolve, tcp_endpoint, MachineFile, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallMedia {
    pub format: u32,
    pub media: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addons: Vec<InstallAddon>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallAddon {
    pub name: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dist: Option<String>,
    pub install: Vec<String>,
}

struct Layer {
    set: &'static str,
    name: &'static str,
    path: &'static str,
    boot: bool,
    base: Option<&'static str>,
    dist: Option<&'static str>,
}

const LAYERS: &[Layer] = &[
    Layer {
        set: "6.5.30",
        name: "overlays1",
        path: "6.5.30/overlays1.image",
        boot: true,
        base: None,
        dist: None,
    },
    Layer {
        set: "6.5.30",
        name: "overlays2",
        path: "6.5.30/overlays2.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "6.5.30",
        name: "overlays3",
        path: "6.5.30/overlays3.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "foundations",
        name: "foundation1",
        path: "6.5-base/foundation1.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "foundations",
        name: "foundation2",
        path: "6.5-base/foundation2.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "foundations",
        name: "onc3-nfs",
        path: "6.5-base/nfs.image",
        boot: false,
        base: None,
        dist: Some("dist6.5"),
    },
    Layer {
        set: "development",
        name: "devlibs",
        path: "6.5-base/devlibs.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "development",
        name: "devfoundation",
        path: "6.5-base/devfoundation.image",
        boot: false,
        base: None,
        dist: Some("dist/dist6.5"),
    },
    Layer {
        set: "development",
        name: "mipspro744update",
        path: "mipspro/7.4.4/mipspro744update.tar.gz",
        boot: false,
        base: Some("MIPSPro7.4.4"),
        dist: Some("."),
    },
    Layer {
        set: "development",
        name: "mipspro_c",
        path: "mipspro/7.4.4/mipspro_c.tar.gz",
        boot: false,
        base: Some("mipspro_c"),
        dist: Some("dist"),
    },
    Layer {
        set: "applications",
        name: "applications",
        path: "6.5.30/applications.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "complementary",
        name: "complementary",
        path: "6.5.30/complementary.image",
        boot: false,
        base: None,
        dist: None,
    },
];

const RAD4_FINISH_SCRIPT: &str = include_str!("../guest/irix/finish-rad4.sh");

const MIPSPRO_INSTALL: &[&str] = &[
    "c_fe.sw.c",
    "c_dev.sw.c",
    "compiler_dev.sw.base",
    "compiler_dev.sw.ld",
    "dev.sw.lib",
];

const MIPSPRO_KEEP: &[&str] = &[
    "java2_plugin.sw32.mozilla_freeware",
    "java_dev.sw32.binaries",
];

const SETS: &[&str] = &[
    "6.5.30",
    "foundations",
    "development",
    "applications",
    "complementary",
];

pub fn init(dir: &Path, media_root: &Path, mac: &str, file: &mut MachineFile) -> Result<PathBuf> {
    let root = media_root.canonicalize()?;
    let install_dir = dir.join("install");
    let manifest_path = install_dir.join("media.toml");
    if manifest_path.exists() {
        return Err(format!(
            "install media configuration already exists: {}",
            manifest_path.display()
        )
        .into());
    }
    let media = LAYERS
        .iter()
        .map(|layer| {
            (
                layer.name.into(),
                root.join(layer.path).display().to_string(),
            )
        })
        .collect();
    let manifest = InstallMedia {
        format: 1,
        media,
        addons: vec![],
    };
    let catalog = crate::catalogue()?;
    let old_network = file.network.clone();
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("tcp:{}", listener.local_addr()?);
    file.network = crate::Network {
        mode: "private".into(),
        endpoint: Some(endpoint),
        mac: Some(mac.into()),
        forward: old_network.forward.clone(),
    };
    if let Err(error) = crate::validate(&catalog, dir, file) {
        file.network = old_network;
        return Err(error);
    }
    fs::create_dir_all(&install_dir)?;
    fs::write(&manifest_path, toml::to_string_pretty(&manifest)?)?;
    fs::write(dir.join("machine.toml"), toml::to_string_pretty(file)?)?;
    Ok(manifest_path)
}

fn valid_addon_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn valid_install_selection(item: &str) -> bool {
    !item.is_empty()
        && item
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

pub fn add_addon(
    dir: &Path,
    name: &str,
    source: &Path,
    base: Option<&str>,
    dist: Option<&str>,
    install: &[String],
) -> Result<PathBuf> {
    if !valid_addon_name(name) {
        return Err("add-on name must use letters, digits, hyphens, or underscores".into());
    }
    if install.is_empty() || install.iter().any(|item| !valid_install_selection(item)) {
        return Err("add-on needs one or more package names using letters, digits, dots, underscores, hyphens, or plus signs".into());
    }
    let source = source.canonicalize()?;
    if !source.is_file() && !source.is_dir() {
        return Err(format!(
            "add-on source is not a file or directory: {}",
            source.display()
        )
        .into());
    }
    let mut media = read_media(dir)?;
    if media.addons.iter().any(|addon| addon.name == name) {
        return Err(format!("add-on already configured: {name}").into());
    }
    media.addons.push(InstallAddon {
        name: name.into(),
        source: source.display().to_string(),
        base: base.map(str::to_owned),
        dist: dist.map(str::to_owned),
        install: install.to_vec(),
    });
    let path = dir.join("install/media.toml");
    fs::write(&path, toml::to_string_pretty(&media)?)?;
    Ok(path)
}

pub fn read_media(dir: &Path) -> Result<InstallMedia> {
    let file: InstallMedia = toml::from_str(&fs::read_to_string(dir.join("install/media.toml"))?)?;
    if file.format != 1 {
        return Err(format!("unsupported install media format {}", file.format).into());
    }
    Ok(file)
}

pub fn config(dir: &Path, file: &MachineFile, media: &InstallMedia) -> Result<Value> {
    if file.network.mode != "private" {
        return Err("installation requires a private network".into());
    }
    let mac = file
        .network
        .mac
        .as_deref()
        .ok_or("private network needs mac")?;
    let mut sets = Vec::new();
    for name in SETS {
        let mut layers = Vec::new();
        for layer in LAYERS.iter().filter(|layer| layer.set == *name) {
            let path = media
                .media
                .get(layer.name)
                .ok_or_else(|| format!("missing install media: {}", layer.name))?;
            let source = resolve(dir, path);
            if !source.is_file() && !source.is_dir() {
                return Err(
                    format!("missing install media {}: {}", layer.name, source.display()).into(),
                );
            }
            let mut entry = json!({ "name": layer.name, "source": source });
            if layer.boot {
                entry["boot"] = json!(true);
            }
            if let Some(base) = layer.base {
                entry["base"] = json!(base);
            }
            if let Some(dist) = layer.dist {
                entry["dist"] = json!(dist);
            }
            layers.push(entry);
        }
        let mut set = json!({ "name": name, "layers": layers });
        if *name == "development" {
            set["collisions"] = json!({ "development/dist/inst.README": "mipspro744update" });
            set["replacements"] = json!({ "devfoundation": "mipspro744update" });
        }
        sets.push(set);
    }
    let mut scripts = vec![json!({
        "name": "mipspro",
        "install": MIPSPRO_INSTALL,
        "keep": MIPSPRO_KEEP
    })];
    let mut addon_names = HashSet::new();
    for addon in &media.addons {
        if !addon_names.insert(&addon.name) {
            return Err(format!("duplicate add-on name: {}", addon.name).into());
        }
        if !valid_addon_name(&addon.name) {
            return Err(format!("invalid add-on name: {}", addon.name).into());
        }
        if addon.install.is_empty()
            || addon
                .install
                .iter()
                .any(|item| !valid_install_selection(item))
        {
            return Err(format!("add-on {} has invalid install selections", addon.name).into());
        }
        let source = resolve(dir, &addon.source);
        if !source.is_file() && !source.is_dir() {
            return Err(format!("missing add-on {}: {}", addon.name, source.display()).into());
        }
        let mut layer = json!({ "name": addon.name, "source": source });
        if let Some(base) = &addon.base {
            layer["base"] = json!(base);
        }
        if let Some(dist) = &addon.dist {
            layer["dist"] = json!(dist);
        }
        let mut layers = vec![layer];
        if addon.install.iter().any(|selection| selection == "rad4x") {
            layers.push(json!({
                "name": "sgi-rad4-finish",
                "source": dir.join("install/generated/rad4"),
                "dist": "dist",
            }));
        }
        sets.push(json!({
            "name": format!("addon-{}", addon.name),
            "layers": layers,
        }));
        let mut selected: Vec<&str> = MIPSPRO_INSTALL.to_vec();
        selected.extend(addon.install.iter().map(String::as_str));
        scripts.push(json!({
            "name": format!("addon-{}", addon.name),
            "install": selected,
            "keep": MIPSPRO_KEEP,
        }));
    }
    Ok(json!({
        "server_ip": "10.98.0.2",
        "netmask": "10.98.0.0/24",
        "cache_dir": dir.join("install/cache"),
        "clients": [{ "name": "sgi", "mac": mac, "ip": "10.98.0.65" }],
        "services": {
            "bootp": true, "tftp": { "port_range": [2048, 32767] }, "rsh": true
        },
        "install_scripts": scripts,
        "install_sets": sets
    }))
}

fn prepare_guest_scripts(dir: &Path, media: &InstallMedia) -> Result<()> {
    if media
        .addons
        .iter()
        .any(|addon| addon.install.iter().any(|selection| selection == "rad4x"))
    {
        let path = dir.join("install/generated/rad4/dist/finish-rad4.sh");
        fs::create_dir_all(path.parent().ok_or("RAD4 script has no parent directory")?)?;
        fs::write(path, RAD4_FINISH_SCRIPT)?;
    }
    Ok(())
}

fn instigator_path() -> Result<PathBuf> {
    let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "instigator.exe"
    } else {
        "instigator"
    });
    if !executable.is_file() {
        return Err(format!("packaged Instigator missing: {}", executable.display()).into());
    }
    Ok(executable)
}

pub fn check(dir: &Path, file: &MachineFile) -> Result<()> {
    let media = read_media(dir)?;
    prepare_guest_scripts(dir, &media)?;
    let document = config(dir, file, &media)?;
    fs::create_dir_all(dir.join("install/cache"))?;
    let mut child = Command::new(instigator_path()?)
        .args(["check", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("Instigator check has no stdin")?
        .write_all(&serde_json::to_vec(&document)?)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "Instigator rejected install media: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

pub fn serve(dir: &Path, file: &MachineFile) -> Result<ExitStatus> {
    let media = read_media(dir)?;
    prepare_guest_scripts(dir, &media)?;
    let document = config(dir, file, &media)?;
    let install_dir = dir.join("install");
    fs::create_dir_all(install_dir.join("cache"))?;
    let config_path = install_dir.join("instigator.json");
    fs::write(&config_path, serde_json::to_vec_pretty(&document)?)?;
    let endpoint = file
        .network
        .endpoint
        .as_deref()
        .ok_or("private network needs endpoint")?;
    let mut command = Command::new(instigator_path()?);
    command.arg("serve");
    if let Some(address) = tcp_endpoint(endpoint)? {
        command.arg("--network-tcp").arg(address.to_string());
    } else {
        command.arg("--network-socket").arg(resolve(dir, endpoint));
    }
    command.arg(config_path).current_dir(dir);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec().into())
    }
    #[cfg(not(unix))]
    {
        Ok(command.status()?)
    }
}

const INSTALL_PROMPT: &[u8] = b"Inst> ";
const SHELL_PROMPT: &[u8] = b"# ";
const TFTP_PROMPT: &[u8] = b"tftp> ";

struct InstallConsole {
    stream: TcpStream,
}

impl InstallConsole {
    fn connect(dir: &Path) -> Result<Self> {
        let record = control::read(dir)?;
        control::verified_qmp(&record)?;
        let address = SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, record.console_port);
        let stream = TcpStream::connect_timeout(&address.into(), Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self { stream })
    }

    fn wait_for(&mut self, prompt: &[u8], timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        let mut chunk = [0u8; 4096];
        while Instant::now() < deadline {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("guest serial console closed".into()),
                Ok(count) => {
                    io::stdout().write_all(&chunk[..count])?;
                    io::stdout().flush()?;
                    output.extend_from_slice(&chunk[..count]);
                    if output.len() > 65536 {
                        output.drain(..output.len() - 65536);
                    }
                    if output.ends_with(prompt) {
                        return Ok(String::from_utf8_lossy(&output).into_owned());
                    }
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(format!(
            "guest did not reach {} within {} seconds; machine remains running",
            String::from_utf8_lossy(prompt),
            timeout.as_secs()
        )
        .into())
    }

    fn command(&mut self, command: &str, prompt: &[u8], timeout: Duration) -> Result<String> {
        self.stream.write_all(command.as_bytes())?;
        self.stream.write_all(b"\n")?;
        self.wait_for(prompt, timeout)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InstallPhase {
    ReadingSources,
    Transferring,
    Checking,
    Finalizing,
}

struct InstallProgress {
    phase: InstallPhase,
    pending: Vec<u8>,
    menu_pending: Vec<u8>,
    menu_answered: bool,
}

impl InstallProgress {
    fn new() -> Self {
        Self {
            phase: InstallPhase::ReadingSources,
            pending: Vec::new(),
            menu_pending: Vec::new(),
            menu_answered: false,
        }
    }

    fn distribution_choice(&mut self, bytes: &[u8]) -> Option<u8> {
        if self.menu_answered || self.phase != InstallPhase::ReadingSources {
            return None;
        }
        self.menu_pending.extend_from_slice(bytes);
        if self.menu_pending.len() > 4096 {
            self.menu_pending.drain(..self.menu_pending.len() - 4096);
        }
        if !self.menu_pending.ends_with(b"] ") {
            return None;
        }
        let output = String::from_utf8_lossy(&self.menu_pending);
        let menu = output.rfind("Install software from: [")?;
        let item = output[..menu]
            .lines()
            .rev()
            .find(|line| line.contains(" done (distribution information read"))?;
        let choice = item.split_whitespace().next()?.parse().ok()?;
        self.menu_answered = true;
        Some(choice)
    }

    fn observe(&mut self, bytes: &[u8]) -> bool {
        self.pending.extend_from_slice(bytes);
        loop {
            let marker: &[u8] = match self.phase {
                InstallPhase::ReadingSources => b"Installing/removing files ..",
                InstallPhase::Transferring => b"Checking dependencies ..",
                InstallPhase::Checking => b"Calculating sizes ..",
                InstallPhase::Finalizing => {
                    if self.pending.ends_with(INSTALL_PROMPT) {
                        return true;
                    }
                    let keep = INSTALL_PROMPT.len() - 1;
                    if self.pending.len() > keep {
                        self.pending.drain(..self.pending.len() - keep);
                    }
                    return false;
                }
            };
            let Some(position) = self.pending.windows(marker.len()).position(|w| w == marker)
            else {
                let keep = marker.len().saturating_sub(1);
                if self.pending.len() > keep {
                    self.pending.drain(..self.pending.len() - keep);
                }
                return false;
            };
            self.pending.drain(..position + marker.len());
            self.phase = match self.phase {
                InstallPhase::ReadingSources => InstallPhase::Transferring,
                InstallPhase::Transferring => InstallPhase::Checking,
                InstallPhase::Checking => InstallPhase::Finalizing,
                InstallPhase::Finalizing => unreachable!(),
            };
        }
    }
}

impl InstallConsole {
    fn wait_for_install(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut progress = InstallProgress::new();
        let mut chunk = [0u8; 4096];
        while Instant::now() < deadline {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("guest serial console closed during installation".into()),
                Ok(count) => {
                    io::stdout().write_all(&chunk[..count])?;
                    io::stdout().flush()?;
                    if let Some(choice) = progress.distribution_choice(&chunk[..count]) {
                        self.stream.write_all(format!("{choice}\n").as_bytes())?;
                    }
                    if progress.observe(&chunk[..count]) {
                        return Ok(());
                    }
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(format!(
            "guest installation did not finish after {} seconds ({:?}); machine remains running",
            timeout.as_secs(),
            progress.phase
        )
        .into())
    }
}

pub fn apply(dir: &Path, file: &MachineFile, addon: Option<&str>) -> Result<()> {
    if file.network.mode != "private" {
        return Err("package installation requires private installation networking".into());
    }
    let media = read_media(dir)?;
    let script = if let Some(name) = addon {
        if !valid_addon_name(name) || !media.addons.iter().any(|item| item.name == name) {
            return Err(format!("unknown install add-on: {name}").into());
        }
        format!("addon-{name}")
    } else {
        "mipspro".into()
    };
    let mut console = InstallConsole::connect(dir)?;
    println!("Load the package script only after the guest reaches Inst>. Disconnect any interactive console first.");
    console.command("", INSTALL_PROMPT, Duration::from_secs(30))?;
    console
        .stream
        .write_all(format!("admin source 10.98.0.2:/{script}.cmds\n").as_bytes())?;
    console.wait_for_install(Duration::from_secs(4 * 60 * 60))?;
    println!("Package transfer returned to Inst>. Review the serial output for package errors before finishing the install.");
    Ok(())
}

// POSIX cksum includes the byte length after the data and complements the CRC.
fn posix_cksum(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    let mut feed = |byte: u8| {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            crc = (crc << 1)
                ^ if crc & 0x8000_0000 != 0 {
                    0x04c1_1db7
                } else {
                    0
                };
        }
    };
    for byte in bytes {
        feed(*byte);
    }
    let mut length = bytes.len();
    while length > 0 {
        feed(length as u8);
        length >>= 8;
    }
    !crc
}

fn received_expected(output: &str, size: usize) -> bool {
    output
        .lines()
        .any(|line| line.trim().starts_with(&format!("Received {size} bytes ")))
}

fn checksum_matches(output: &str, crc: u32, size: usize) -> bool {
    output.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let parsed = (
            fields.next().and_then(|value| value.parse::<u32>().ok()),
            fields.next().and_then(|value| value.parse::<usize>().ok()),
            fields.next(),
        );
        parsed == (Some(crc), Some(size), Some("/tmp/finish-rad4.sh"))
    })
}

pub fn finish_rad4(dir: &Path, file: &MachineFile) -> Result<()> {
    if file.machine.model != "origin200" || file.machine.graphics != "rad4" {
        return Err("RAD4 finish requires an Origin 200 RAD4 machine".into());
    }
    if file.network.mode != "private" {
        return Err("RAD4 finish requires private installation networking".into());
    }
    let media = read_media(dir)?;
    if !media
        .addons
        .iter()
        .any(|addon| addon.install.iter().any(|item| item == "rad4x"))
    {
        return Err("RAD4 add-on is not configured for this machine".into());
    }
    let generated = dir.join("install/generated/rad4/dist/finish-rad4.sh");
    if fs::read(&generated)? != RAD4_FINISH_SCRIPT.as_bytes() {
        return Err(format!(
            "RAD4 helper differs from packaged source: {}",
            generated.display()
        )
        .into());
    }

    let mut console = InstallConsole::connect(dir)?;
    println!("Finish the installation only after its Inst> prompt appears.");
    console.command("", INSTALL_PROMPT, Duration::from_secs(30))?;
    console.command("shroot", SHELL_PROMPT, Duration::from_secs(60))?;
    console.command(
        "ifconfig ef0 10.98.0.65 netmask 255.255.255.0 up",
        SHELL_PROMPT,
        Duration::from_secs(30),
    )?;
    console.command("tftp 10.98.0.2", TFTP_PROMPT, Duration::from_secs(30))?;
    console.command("binary", TFTP_PROMPT, Duration::from_secs(30))?;
    let received = console.command(
        "get /addon-rad4/dist/finish-rad4.sh /tmp/finish-rad4.sh",
        TFTP_PROMPT,
        Duration::from_secs(60),
    )?;
    console.command("quit", SHELL_PROMPT, Duration::from_secs(30))?;
    if !received_expected(&received, RAD4_FINISH_SCRIPT.len()) {
        return Err(
            "RAD4 helper TFTP transfer did not report its expected size; machine remains running"
                .into(),
        );
    }
    let checksum = console.command(
        "cksum /tmp/finish-rad4.sh",
        SHELL_PROMPT,
        Duration::from_secs(30),
    )?;
    if !checksum_matches(
        &checksum,
        posix_cksum(RAD4_FINISH_SCRIPT.as_bytes()),
        RAD4_FINISH_SCRIPT.len(),
    ) {
        return Err(
            "RAD4 helper guest checksum differs from packaged source; machine remains running"
                .into(),
        );
    }
    let syntax = console.command(
        "/bin/sh -c '/bin/sh -n /tmp/finish-rad4.sh; echo SGI_RAD4_SYNTAX_STATUS=$?'",
        SHELL_PROMPT,
        Duration::from_secs(30),
    )?;
    if !syntax
        .lines()
        .any(|line| line.trim() == "SGI_RAD4_SYNTAX_STATUS=0")
    {
        return Err("RAD4 helper failed guest shell syntax check; machine remains running".into());
    }
    let result = console.command(
        "/bin/sh -c '/bin/sh /tmp/finish-rad4.sh /; echo SGI_RAD4_FINISH_STATUS=$?'",
        SHELL_PROMPT,
        Duration::from_secs(900),
    )?;
    if !result
        .lines()
        .any(|line| line.trim() == "SGI_RAD4_FINISH_STATUS=0")
    {
        return Err("RAD4 helper did not report successful exit; machine remains running".into());
    }
    console.command("exit", INSTALL_PROMPT, Duration::from_secs(60))?;
    console.command(
        "quit",
        b"Restart? { (y)es, (n)o, (sh)ell, (h)elp }: ",
        Duration::from_secs(900),
    )?;
    console.command("y", b"IRIS console login: ", Duration::from_secs(600))?;
    println!("RAD4 configured and installed IRIX reached serial login. Stop the machine before switching to user networking and SDL.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_progress_selects_distribution_menu_once() {
        let mut progress = InstallProgress::new();
        assert_eq!(progress.distribution_choice(b" 1 primary\r\n 7 done (distribution information read, return to inst prompt)\r\nInstall software from: [primary"), None);
        assert_eq!(progress.distribution_choice(b"] "), Some(7));
        assert_eq!(
            progress.distribution_choice(b"Install software from: [primary] "),
            None
        );
    }

    #[test]
    fn install_progress_ignores_intermediate_inst_prompt() {
        let mut progress = InstallProgress::new();
        assert!(!progress.observe(b"Inst> Reading product descriptions .. 100% Done.\r\n"));
        assert!(!progress.observe(b"Inst> "));
        assert_eq!(progress.phase, InstallPhase::ReadingSources);
        assert!(!progress.observe(b"Installing/removing fi"));
        assert!(!progress.observe(b"les .. 22%\r\n"));
        assert_eq!(progress.phase, InstallPhase::Transferring);
        assert!(!progress.observe(b"Checking dependencies .. 97% 100% Done.\r\n"));
        assert_eq!(progress.phase, InstallPhase::Checking);
        assert!(!progress.observe(b"Calculating sizes .. 100% Done.\r\nInst>"));
        assert!(progress.observe(b" "));
    }

    #[test]
    fn rad4_guest_transfer_must_match_packaged_helper() {
        let size = RAD4_FINISH_SCRIPT.len();
        let crc = posix_cksum(RAD4_FINISH_SCRIPT.as_bytes());
        assert_eq!(crc, 2973792095);
        assert_eq!(posix_cksum(b"123456789"), 930766865);
        assert!(received_expected(
            "Received 2550 bytes in 0.0 seconds\r\n",
            size
        ));
        assert!(checksum_matches(
            "2973792095 2550 /tmp/finish-rad4.sh\r\n",
            crc,
            size,
        ));
        assert!(!received_expected("Error: transfer failed\r\n", size));
        assert!(!checksum_matches(
            "2973792095 2549 /tmp/finish-rad4.sh\r\n",
            crc,
            size,
        ));
    }

    #[test]
    fn addon_script_selects_mipspro_and_local_package() {
        use crate::{Firmware, Machine, Network};
        use std::time::{SystemTime, UNIX_EPOCH};

        let dir = std::env::temp_dir().join(format!(
            "sgi-addon-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let source = dir.join("tablet.tardist");
        fs::write(&source, b"synthetic source").unwrap();
        let media = InstallMedia {
            format: 1,
            media: LAYERS
                .iter()
                .map(|layer| (layer.name.into(), source.display().to_string()))
                .collect(),
            addons: vec![InstallAddon {
                name: "tablet".into(),
                source: source.display().to_string(),
                base: Some("tablet-disc".into()),
                dist: Some("dist".into()),
                install: vec!["tablet.sw.helper".into()],
            }],
        };
        let file = MachineFile {
            format: 1,
            machine: Machine {
                model: "origin200".into(),
                nodes: 1,
                cpus_per_node: 1,
                memory_per_node: "256MiB".into(),
                graphics: "rad4".into(),
            },
            firmware: Firmware {
                image: "prom.bin".into(),
            },
            identity: None,
            network: Network {
                mode: "private".into(),
                endpoint: Some("install/network.sock".into()),
                mac: Some("08:00:69:12:34:56".into()),
                forward: vec![],
            },
            drive: vec![],
        };
        let document = config(&dir, &file, &media).unwrap();
        let development = document["install_sets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|set| set["name"] == "development")
            .unwrap();
        assert_eq!(
            development["replacements"]["devfoundation"],
            "mipspro744update"
        );
        let mipspro = &document["install_scripts"][0];
        assert_eq!(
            mipspro["keep"],
            json!([
                "java2_plugin.sw32.mozilla_freeware",
                "java_dev.sw32.binaries"
            ])
        );
        let addon_set = &document["install_sets"][SETS.len()];
        assert_eq!(addon_set["name"], "addon-tablet");
        assert_eq!(
            addon_set["layers"][0]["source"],
            source.display().to_string()
        );
        assert_eq!(addon_set["layers"][0]["base"], "tablet-disc");
        let script = &document["install_scripts"][1];
        assert_eq!(script["name"], "addon-tablet");
        assert_eq!(script["keep"], mipspro["keep"]);
        assert!(script["install"]
            .as_array()
            .unwrap()
            .contains(&json!("c_fe.sw.c")));
        assert!(script["install"]
            .as_array()
            .unwrap()
            .contains(&json!("tablet.sw.helper")));
        let mut invalid = media.clone();
        invalid.addons[0].install = vec!["tablet.sw.helper\nquit".into()];
        assert!(config(&dir, &file, &invalid).is_err());
        invalid.addons[0].install = vec!["tablet.sw.helper".into()];
        invalid.addons.push(invalid.addons[0].clone());
        assert!(config(&dir, &file, &invalid).is_err());

        let mut rad4 = media.clone();
        rad4.addons[0].name = "rad4".into();
        rad4.addons[0].install = vec!["rad4x".into()];
        prepare_guest_scripts(&dir, &rad4).unwrap();
        let script_path = dir.join("install/generated/rad4/dist/finish-rad4.sh");
        assert_eq!(fs::read_to_string(script_path).unwrap(), RAD4_FINISH_SCRIPT);
        let document = config(&dir, &file, &rad4).unwrap();
        let layers = document["install_sets"][SETS.len()]["layers"]
            .as_array()
            .unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[1]["name"], "sgi-rad4-finish");
        assert_eq!(layers[1]["dist"], "dist");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn media_root_names_the_directory_containing_release_folders() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "sgi-media-root-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let media_root = root.join("media");
        fs::create_dir_all(&media_root).unwrap();
        let prom = root.join("prom.bin");
        fs::write(&prom, vec![0; 1048576]).unwrap();
        let machine = root.join("machine");
        let catalog = crate::catalogue().unwrap();
        crate::create(
            &machine,
            crate::preset(&catalog, "origin200-1").unwrap(),
            &prom,
            None,
            None,
        )
        .unwrap();
        let mut file = crate::read_machine(&machine).unwrap();
        init(&machine, &media_root, "08:00:69:12:34:56", &mut file).unwrap();
        let manifest = read_media(&machine).unwrap();
        assert!(file
            .network
            .endpoint
            .as_deref()
            .unwrap()
            .starts_with("tcp:127.0.0.1:"));
        assert_eq!(
            manifest.media["overlays1"],
            media_root
                .join("6.5.30/overlays1.image")
                .display()
                .to_string()
        );
        assert_eq!(
            manifest.media["mipspro_c"],
            media_root
                .join("mipspro/7.4.4/mipspro_c.tar.gz")
                .display()
                .to_string()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn standard_profile_contains_mipspro_media() {
        assert_eq!(
            LAYERS
                .iter()
                .filter(|layer| layer.set == "development" && layer.name.starts_with("mipspro"))
                .count(),
            2
        );
        assert!(LAYERS
            .iter()
            .any(|layer| layer.name == "overlays1" && layer.boot));
        assert_eq!(SETS.len(), 5);
    }
}
