use crate::{
    control, qemu_path_option, resolve, tcp_endpoint, Drive, MachineFile, Machines, Offering,
    PortForward, Result, StorageItem,
};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Local,
    Vnc { port: u16 },
    None,
}

pub fn qemu_path() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("ORIGAMI_RUNTIME_DIR") {
        return Ok(PathBuf::from(root).join(binary_name("qemu-system-mips64")));
    }
    let exe = std::env::current_exe()?;
    Ok(exe
        .parent()
        .ok_or("cannot locate origami executable directory")?
        .join("../libexec/origami")
        .join(binary_name("qemu-system-mips64")))
}

fn binary_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.into()
    }
}

pub fn qemu_img_path() -> Result<PathBuf> {
    Ok(qemu_path()?.with_file_name(binary_name("qemu-img")))
}

fn qemu_data_path() -> Result<PathBuf> {
    Ok(qemu_path()?
        .parent()
        .ok_or("cannot locate packaged QEMU directory")?
        .join("../../share/origami/qemu"))
}

pub fn machine_init_path() -> Result<PathBuf> {
    Ok(qemu_path()?.with_file_name(binary_name("qemu-sgi-machine-init")))
}

/// The init tool's command line for a new machine whose storage goes in
/// `state`. The tool refuses an image the machine does not read and a
/// missing one it needs, before it creates anything.
pub fn init_arguments(
    offering: &Offering,
    boot_prom: &Path,
    io_prom: Option<&Path>,
    state: &Path,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--machine".into(),
        offering.machine_options.clone().into(),
        "--boot-prom".into(),
        boot_prom.into(),
    ];
    if let Some(io_prom) = io_prom {
        args.extend(["--io-prom".into(), io_prom.into()]);
    }
    args.push(state.into());
    args
}

/// Run QEMU's init tool, which writes every storage file or nothing.
pub fn create_state(tool: &Path, arguments: &[OsString]) -> Result<()> {
    let output = Command::new(tool)
        .args(arguments)
        .output()
        .map_err(|error| format!("cannot run {}: {error}", tool.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} failed ({}): {}",
            tool.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

/// The serial lines a console can use, as (-serial index, chardev name,
/// whether it is the primary console). The machine list gives the lines in
/// -serial order, node 0 first; lines of other nodes are not offered.
fn console_ports(offering: &Offering) -> impl Iterator<Item = (usize, &str, bool)> {
    offering
        .consoles
        .iter()
        .enumerate()
        .take_while(|(_, line)| line.node == 0)
        .filter(|(_, line)| matches!(line.kind.as_str(), "console" | "second-port"))
        .filter_map(|(index, line)| {
            let name = line.argument.strip_prefix("-serial chardev:")?;
            Some((index, name, line.kind == "console"))
        })
}

/// The machine's default console: the machine list's line of kind console.
pub fn default_console(offering: &Offering) -> Result<&str> {
    console_ports(offering)
        .find(|(_, _, primary)| *primary)
        .map(|(_, name, _)| name)
        .ok_or_else(|| format!("{} lists no node 0 console", offering.topology).into())
}

/// The -serial index of a console port, by default the machine list's.
pub fn console_line(offering: &Offering, port: Option<&str>) -> Result<usize> {
    let port = match port {
        Some(port) => port,
        None => default_console(offering)?,
    };
    console_ports(offering)
        .find(|(_, name, _)| *name == port)
        .map(|(index, _, _)| index)
        .ok_or_else(|| {
            let names: Vec<_> = console_ports(offering).map(|(_, name, _)| name).collect();
            format!(
                "{} has no {port} console; choose {}",
                offering.topology,
                names.join(" or ")
            )
            .into()
        })
}

fn state_path(dir: &Path, item: &StorageItem) -> PathBuf {
    dir.join("state").join(format!("{}.raw", item.name))
}

/// Origami never rebuilds missing storage: a flash file holds the guest's
/// own PROM updates and settings, which the original images cannot restore.
pub fn check_state(dir: &Path, offering: &Offering) -> Result<()> {
    for item in &offering.storage {
        let path = state_path(dir, item);
        if !path.is_file() {
            return Err(format!(
                "missing machine state {}. Restore it from a backup, or create a new machine \
                 with origami create and attach this machine's disks",
                path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn user_network(dir: &Path, forward: &[PortForward]) -> String {
    let mut option =
        "user,id=net0,net=192.0.2.0/24,host=192.0.2.2,dhcpstart=192.0.2.15".to_string();
    // DHCP offers the directory name as the guest's hostname when it is a
    // valid one; IRIX keeps its own name otherwise.
    if let Some(name) = dir.file_name().and_then(|name| name.to_str()) {
        if !name.is_empty()
            && name.len() <= 63
            && !name.starts_with('-')
            && !name.ends_with('-')
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            option.push_str(&format!(",hostname={name}"));
        }
    }
    for rule in forward {
        option.push_str(&format!(
            ",hostfwd={}:127.0.0.1:{}-:{}",
            rule.protocol, rule.host_port, rule.guest_port
        ));
    }
    option
}

/// One fixed suggestion per host-backed mode, shown beside QEMU's own error.
fn network_hint(file: &MachineFile) -> Option<String> {
    let endpoint = file.network.endpoint.as_deref().unwrap_or("");
    let interface = file.network.interface.as_deref().unwrap_or("");
    match file.network.mode.as_str() {
        "tap" if cfg!(windows) => Some(format!(
            "hint: tap mode needs a TAP-Windows6 adapter named {interface} in Network \
             Connections; without administrator rights, set its Non-Admin Access"
        )),
        "tap" => Some(format!(
            "hint: tap mode needs a tap device you own; once, as root:\n      \
             ip tuntap add dev {interface} mode tap user $USER && \
             ip link set {interface} master br0 up"
        )),
        "vmnet" => Some(format!(
            "hint: no socket_vmnet daemon at {endpoint}; \
             brew install socket_vmnet && sudo brew services start socket_vmnet"
        )),
        _ => None,
    }
}

/// Append the unsupported QEMU options last, so they override what the CLI
/// generated: first those in machine.toml, then this run's `--qemu-arg`s.
pub fn append_extra_args(
    args: &mut Vec<String>,
    file: &MachineFile,
    once: &[String],
) -> Result<()> {
    check_extra_args(&file.qemu.extra_args)?;
    check_extra_args(once)?;
    args.extend(file.qemu.extra_args.iter().cloned());
    args.extend(once.iter().cloned());
    Ok(())
}

/// Options Origami itself relies on to find and drive the guest: its QMP
/// endpoint, QEMU name and console. Overriding them cuts off `status`, `stop`
/// and `console`, so they are refused.
pub fn check_extra_args(extra: &[String]) -> Result<()> {
    for (index, arg) in extra.iter().enumerate() {
        let owned = matches!(
            arg.as_str(),
            "-name" | "--name" | "-qmp" | "--qmp" | "-serial" | "--serial"
        ) || (matches!(arg.as_str(), "-chardev" | "--chardev")
            && extra
                .get(index + 1)
                .is_some_and(|value| value.contains("id=origami-console")));
        if owned {
            return Err(format!(
                "extra QEMU argument {arg} is refused: Origami uses it to control the machine"
            )
            .into());
        }
    }
    Ok(())
}

/// `arg` quoted so a shell reads it back as one word.
pub fn shell_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if plain {
        arg.into()
    } else if cfg!(windows) {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

pub fn arguments(
    machines: &Machines,
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
) -> Result<Vec<String>> {
    arguments_with(machines, dir, file, offering, display, None)
}

/// [`arguments`], with the machine's network behind the share switch when
/// `shares` names the ports it listens on.
pub fn arguments_with(
    machines: &Machines,
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
    shares: Option<&crate::shares::Ports>,
) -> Result<Vec<String>> {
    let mut machine = offering.machine_options.clone();
    if !matches!(offering.product.as_str(), "octane" | "octane2") {
        let population = offering
            .cpus_per_node
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(":");
        machine.push_str(&format!(",population={population}"));
    }
    crate::profiles::validate_inputs(offering, &file.machine.inputs)?;
    let mut cpu = offering.cpu.clone();
    for (key, value) in &file.machine.inputs {
        let target = if crate::profiles::input_target(offering, key) == Some("cpu") {
            &mut cpu
        } else {
            &mut machine
        };
        target.push_str(&format!(",{key}={value}"));
    }
    if let Some(identity) = &file.identity {
        machine.push_str(&format!(",{}={}", offering.mac_property(), identity.mac));
    }
    if offering.needs_debug_leds_off {
        machine.push_str(",debug-leds=off");
    }
    for property in crate::graphics::machine_properties(machines, &offering.topology, &file.pipe) {
        machine.push_str(&format!(",{}={}", property.name, property.value));
    }
    // Each store is a block node bound through the machine property of the
    // same name.
    let mut storage = Vec::new();
    for item in &offering.storage {
        machine.push_str(&format!(",{0}={0}", item.name));
        storage.extend([
            "-blockdev".into(),
            format!(
                "driver=raw,node-name={},file.driver=file,file.filename={}{}",
                item.name,
                qemu_path_option(&state_path(dir, item)),
                if item.read_only { ",read-only=on" } else { "" }
            ),
        ]);
    }
    let memory: u32 = file
        .machine
        .memory_per_node
        .trim_end_matches("MiB")
        .parse()?;
    let mut args = vec![
        // Default devices create a second, empty SDL window beside RAD4.
        "-nodefaults".into(),
        "-L".into(),
        qemu_data_path()?.display().to_string(),
        "-accel".into(),
        if offering.smp > 1 {
            "tcg,thread=multi"
        } else {
            "tcg"
        }
        .into(),
        "-M".into(),
        machine,
        "-cpu".into(),
        cpu,
        "-smp".into(),
        offering.smp.to_string(),
        "-m".into(),
        (memory * offering.nodes).to_string(),
    ];
    args.extend(storage);
    match display {
        Display::Local => args.extend(["-display".into(), "sdl,window-close=off".into()]),
        Display::Vnc { port } => {
            args.extend(["-display".into(), format!("vnc=127.0.0.1:{}", port - 5900)])
        }
        Display::None => args.extend(["-display".into(), "none".into()]),
    }
    args.extend(["-audio".into(), "none".into()]);
    args.extend(crate::graphics::arguments(
        machines,
        &offering.topology,
        &file.pipe,
    )?);
    // IRIX numbers a keyboard and mouse into /etc/ioconfig.conf at boot, and
    // one plugged in later keeps ID -1, which X never opens. So they are
    // present from power-on on every run. A relative mouse: IRIX reads a
    // tablet's absolute coordinates as signed relative motion.
    if crate::profiles::usb_input_only(offering) {
        args.extend([
            "-device".into(),
            "usb-kbd,bus=usb-bus.0".into(),
            "-device".into(),
            "usb-mouse,bus=usb-bus.0".into(),
        ]);
    }
    for (index, drive) in file.drive.iter().enumerate() {
        add_drive(&mut args, dir, offering, drive, index);
    }
    // Lines before the console get no backend, so each device can tell it
    // has none. Later lines are left off.
    for _ in 0..console_line(offering, file.machine.console.as_deref())? {
        args.extend(["-serial".into(), "none".into()]);
    }
    args.extend(["-serial".into(), "stdio".into()]);
    let backend = match file.network.mode.as_str() {
        "none" => None,
        "user" => Some(user_network(dir, &file.network.forward)),
        mode @ ("private" | "vmnet" | "tap") => Some(if mode == "tap" {
            let interface = file
                .network
                .interface
                .as_deref()
                .ok_or("tap network needs interface")?;
            // Origami runs no host scripts; the user sets up the device.
            let scripts = if cfg!(windows) {
                ""
            } else {
                ",script=,downscript="
            };
            format!(
                "tap,id=net0,ifname={}{scripts}",
                interface.replace(',', ",,")
            )
        } else {
            let endpoint = file
                .network
                .endpoint
                .as_deref()
                .ok_or_else(|| format!("{mode} network needs endpoint"))?;
            if let Some(address) = tcp_endpoint(endpoint)? {
                format!(
                    "stream,id=net0,server=off,addr.type=inet,addr.host={},addr.port={},reconnect-ms=1000",
                    address.ip(), address.port()
                )
            } else {
                // The server on the other end, socket_vmnet or Instigator, can
                // start after the guest or restart under it.
                format!(
                    "stream,id=net0,server=off,addr.type=unix,addr.path={},reconnect-ms=1000",
                    qemu_path_option(&resolve(dir, endpoint))
                )
            }
        }),
        _ => return Err("network mode must be user, none, private, tap or vmnet".into()),
    };
    match (backend, shares) {
        (Some(backend), Some(ports)) => {
            // The machine's network joins hub 1 beside the switch's second
            // connection; the NIC talks to the switch alone.
            args.extend(["-netdev".into(), backend]);
            args.extend(crate::shares::netdev_arguments(ports));
            args.extend(["-net".into(), nic_option(offering, file, "share-guest")]);
        }
        (Some(backend), None) => args.extend([
            "-netdev".into(),
            backend,
            "-net".into(),
            nic_option(offering, file, "net0"),
        ]),
        (None, _) => args.extend(["-nic".into(), "none".into()]),
    }
    Ok(args)
}

/// The `-net nic` binding net0 to the machine's default onboard Ethernet.
/// Its address is the machine's mac= unless the machine list says the
/// adapter takes its own. A model-less NIC binds the first onboard one, for
/// a QEMU whose machine list does not name the model.
fn nic_option(offering: &Offering, file: &MachineFile, netdev: &str) -> String {
    let adapter = offering.default_network();
    let mut option = "nic".to_string();
    if let Some(model) = adapter.and_then(|adapter| adapter.nic_model.as_deref()) {
        option.push_str(&format!(",model={model}"));
    }
    option.push_str(&format!(",netdev={netdev}"));
    if let (Some(adapter), Some(identity)) = (adapter, &file.identity) {
        if let (Some(mac), Some(name)) = (&adapter.mac, &adapter.nic_mac_option) {
            if !mac.derived {
                option.push_str(&format!(",{name}={}", identity.mac));
            }
        }
    }
    option
}

fn add_drive(args: &mut Vec<String>, dir: &Path, offering: &Offering, drive: &Drive, index: usize) {
    let path = resolve(dir, &drive.image);
    let format = if path.extension().is_some_and(|ext| ext == "qcow2") {
        "qcow2"
    } else {
        "raw"
    };
    let id = format!("drive{index}");
    args.extend([
        "-drive".into(),
        format!(
            "if=none,id={id},file={},format={format},readonly={}",
            qemu_path_option(&path),
            if drive.read_only { "on" } else { "off" }
        ),
    ]);
    let device = match drive.kind.as_str() {
        "cdrom" => "scsi-cd",
        "tape" => "scsi-tape",
        _ => "scsi-hd",
    };
    // A port on a Fibre Channel private loop needs names of its own. These
    // are synthetic, NAA type 2 with no vendor part, and the loop ID keeps
    // every port's apart.
    let bus = format!("scsi.{}", drive.bus);
    let loop_names = if offering
        .scsi_adapters
        .iter()
        .any(|adapter| adapter.bus == bus && adapter.kind == "fc-loop")
    {
        format!(
            ",port_wwn=0x21000000000000{0:02x},wwn=0x20000000000000{0:02x}",
            drive.target
        )
    } else {
        String::new()
    };
    args.extend([
        "-device".into(),
        format!(
            "{device},bus={bus},scsi-id={},lun=0,drive={id}{loop_names}",
            drive.target
        ),
    ]);
}

pub fn start_background(dir: &Path, display: Display, once: &[String]) -> Result<()> {
    if control::is_locked(dir)? {
        return Err("machine is already running".into());
    }
    fs::create_dir_all(dir.join("logs"))?;
    let log_path = dir.join("logs/runner.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_start = log.metadata()?.len();
    let display_name = match display {
        Display::Local => "local",
        Display::Vnc { .. } => "vnc",
        Display::None => "none",
    };
    let mut child = Command::new(std::env::current_exe()?);
    child
        .arg("_serve")
        .arg(dir)
        .arg("--display")
        .arg(display_name);
    if let Display::Vnc { port } = display {
        child.arg("--vnc-port").arg(port.to_string());
    }
    for arg in once {
        child.arg(format!("--qemu-arg={arg}"));
    }
    // Windows passes every inheritable handle to a child, including this
    // process's own standard streams, so a caller reading them would wait
    // until QEMU exits. Keep them out of the background process.
    #[cfg(windows)]
    for handle in [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ] {
        // SAFETY: the standard handles stay open for the life of the process.
        unsafe {
            windows_sys::Win32::Foundation::SetHandleInformation(
                handle,
                windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT,
                0,
            )
        };
    }
    let mut child = child
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(record) = control::read(dir) {
            if record.pid == child.id() && control::verified_qmp(&record).is_ok() {
                println!("running: pid {}", record.pid);
                return Ok(());
            }
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "machine failed to start ({status}):\n{}\nfull log: {}",
                log_tail(&log_path, log_start),
                log_path.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "machine startup timed out; inspect {} and use origami status",
        log_path.display()
    )
    .into())
}

pub fn run(
    machines: &Machines,
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
    once: &[String],
) -> Result<ExitStatus> {
    run_inner(machines, dir, file, offering, display, once, false)
}

pub fn serve(
    machines: &Machines,
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
    once: &[String],
) -> Result<ExitStatus> {
    run_inner(machines, dir, file, offering, display, once, true)
}

fn run_inner(
    machines: &Machines,
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
    once: &[String],
    background: bool,
) -> Result<ExitStatus> {
    let qemu = qemu_path()?;
    if !qemu.is_file() {
        return Err(format!("packaged QEMU missing: {}", qemu.display()).into());
    }
    let keymap = qemu_data_path()?.join("keymaps/en-us");
    if !keymap.is_file() {
        return Err(format!("packaged QEMU keymap missing: {}", keymap.display()).into());
    }
    // A machine without its storage is not started, and gains nothing.
    check_state(dir, offering)?;
    let _lock = control::lock_for_edit(dir, "starting it again")?;
    // The switch runs in this process, which also waits on QEMU, so it
    // lives exactly as long as the guest.
    let share_net = crate::shares::start(dir, file)?;
    let ports = share_net.as_ref().map(|net| net.ports());
    let mut args = arguments_with(machines, dir, file, offering, display, ports.as_ref())?;
    let mode = file.network.mode.as_str();
    if matches!(mode, "private" | "vmnet") {
        let endpoint = file
            .network
            .endpoint
            .as_deref()
            .ok_or_else(|| format!("{mode} network needs endpoint"))?;
        if tcp_endpoint(endpoint)?.is_none() {
            let path = resolve(dir, endpoint);
            let hint = network_hint(file).map_or(String::new(), |hint| format!("\n{hint}"));
            // Instigator's endpoint may come up after the guest, which keeps
            // trying; socket_vmnet has to be running already.
            let metadata = match fs::metadata(&path) {
                Err(error) if mode == "private" && error.kind() == std::io::ErrorKind::NotFound => {
                    None
                }
                other => Some(other.map_err(|error| {
                    format!("{mode} network socket {}: {error}{hint}", path.display())
                })?),
            };
            #[cfg(unix)]
            if metadata.is_some_and(|m| !m.file_type().is_socket()) {
                return Err(format!(
                    "{mode} network endpoint is not a socket: {}",
                    path.display()
                )
                .into());
            }
            #[cfg(not(unix))]
            let _ = metadata;
            // A stream client that cannot connect leaves the guest running
            // with a dead link and says nothing.
            #[cfg(unix)]
            if mode == "vmnet" {
                std::os::unix::net::UnixStream::connect(&path)
                    .map_err(|error| format!("vmnet socket {}: {error}{hint}", path.display()))?;
            }
        }
    }
    if !background {
        fs::create_dir_all(dir.join("logs"))?;
        log_primary_serial(
            &mut args,
            "stdio,id=origami-console,logfile=logs/serial.log,logappend=on".into(),
        )?;
        append_extra_args(&mut args, file, once)?;
        let mut child = Command::new(qemu).args(args).current_dir(dir).spawn()?;
        let status = child.wait()?;
        if let (false, Some(hint)) = (status.success(), network_hint(file)) {
            eprintln!("{hint}");
        }
        return Ok(status);
    }
    let qmp_port = control::free_port()?;
    let mut console_port = control::free_port()?;
    while console_port == qmp_port {
        console_port = control::free_port()?;
    }
    let name = format!(
        "sgi-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    log_primary_serial(
        &mut args,
        format!("socket,id=origami-console,host=127.0.0.1,port={console_port},server=on,wait=off,logfile=logs/serial.log,logappend=on"),
    )?;
    args.extend([
        "-qmp".into(),
        format!("tcp:127.0.0.1:{qmp_port},server=on,wait=off"),
        "-name".into(),
        name.clone(),
    ]);
    append_extra_args(&mut args, file, once)?;
    let record = control::Record {
        pid: std::process::id(),
        qmp_port,
        console_port,
        name,
    };
    let mut child = Command::new(qemu)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if control::verified_qmp(&record).is_ok() {
            if let Err(error) = control::write(dir, &record) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            let status = child.wait()?;
            control::clear_if_current(dir, &record);
            return Ok(status);
        }
        if let Some(status) = child.try_wait()? {
            let hint = network_hint(file).map_or(String::new(), |hint| format!("\n{hint}"));
            return Err(format!("QEMU exited before QMP was ready ({status}){hint}").into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.kill()?;
    child.wait()?;
    Err("QEMU did not open its QMP control endpoint".into())
}

/// The last few lines written to the runner log since `start`, which hold
/// QEMU's own error and the launcher's reply to it.
fn log_tail(path: &Path, start: u64) -> String {
    let text = fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(text.get(start as usize..).unwrap_or_default());
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(6)..].join("\n")
}

fn log_primary_serial(args: &mut Vec<String>, chardev: String) -> Result<()> {
    let serial = args
        .windows(2)
        .position(|pair| pair == ["-serial", "stdio"])
        .ok_or("primary serial argument missing")?;
    // QEMU labels the chardev it makes for each other -serial line serialN,
    // so a console chardev with one of those ids would collide.
    args[serial + 1] = "chardev:origami-console".into();
    args.extend(["-chardev".into(), chardev]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{preset, Machine, Network};

    fn machine(offering: &Offering) -> MachineFile {
        MachineFile {
            format: crate::MACHINE_FORMAT,
            machine: Machine {
                topology: None,
                population: vec![],
                inputs: Default::default(),
                model: offering.product.clone(),
                nodes: offering.nodes,
                cpus_per_node: offering.cpus_per_node[0],
                memory_per_node: format!("{}MiB", offering.memory.default),
                console: None,
                processor: None,
            },
            identity: None,
            network: Network::default(),
            pipe: vec![],
            drive: vec![],
            share: vec![],
            qemu: Default::default(),
        }
    }

    #[test]
    fn local_graphics_uses_sdl_and_rad4() {
        let machines = crate::test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let mut file = machine(offer);
        file.pipe = crate::graphics::select(
            &machines,
            &offer.topology,
            offer.nodes,
            &crate::graphics::Request {
                product: Some("rad4"),
                ..Default::default()
            },
        )
        .unwrap();
        let args = arguments(
            &machines,
            Path::new("/machine"),
            &file,
            offer,
            Display::Local,
        )
        .unwrap();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-display", "sdl,window-close=off"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-device", "psitech-rad4,bus=pci,addr=5,product=rad4"]));
        assert!(args.windows(2).any(|pair| pair == ["-serial", "stdio"]));
    }

    #[test]
    fn a_pipe_adds_the_machine_properties_it_needs() {
        let machines = crate::test_machines();
        let offer = preset(&machines, "origin2000-8").unwrap();
        let machine_option = |file: &MachineFile| {
            let args =
                arguments(&machines, Path::new("/machine"), file, offer, Display::None).unwrap();
            args[args.iter().position(|a| a == "-M").unwrap() + 1].clone()
        };
        let mut file = machine(offer);
        assert!(!machine_option(&file).contains("baseio-preset"));
        file.pipe = crate::graphics::select(
            &machines,
            &offer.topology,
            offer.nodes,
            &crate::graphics::Request {
                product: Some("si"),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(machine_option(&file).contains(",baseio-preset=io6g"));
    }

    #[test]
    fn only_usb_input_machines_get_a_usb_keyboard_and_mouse() {
        let machines = crate::test_machines();
        let devices = |name: &str| {
            let offer = preset(&machines, name).unwrap();
            let args = arguments(
                &machines,
                Path::new("/machine"),
                &machine(offer),
                offer,
                Display::None,
            )
            .unwrap();
            args.windows(2)
                .filter(|pair| pair[0] == "-device" && pair[1].starts_with("usb-"))
                .map(|pair| pair[1].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            devices("origin3200"),
            ["usb-kbd,bus=usb-bus.0", "usb-mouse,bus=usb-bus.0"]
        );
        // Fuel, Octane and Origin 200 keep their PS/2 ports.
        for name in ["fuel-1", "octane-impact", "origin200-1"] {
            assert!(devices(name).is_empty(), "{name}");
        }
    }

    #[test]
    fn extra_args_come_last_machine_toml_before_command_line() {
        let machines = crate::test_machines();
        let offering = preset(&machines, "fuel-1").unwrap();
        let mut file = machine(offering);
        file.qemu.extra_args = vec!["-global".into(), "bcm570x.mac=08:00:69:12:34:56".into()];
        let mut args = arguments(
            &machines,
            Path::new("/machine"),
            &file,
            offering,
            Display::None,
        )
        .unwrap();
        let generated = args.len();
        append_extra_args(&mut args, &file, &["-d".into(), "guest_errors".into()]).unwrap();
        assert_eq!(
            args[generated..],
            [
                "-global",
                "bcm570x.mac=08:00:69:12:34:56",
                "-d",
                "guest_errors"
            ]
        );
    }

    #[test]
    fn a_fibre_channel_loop_disk_gets_port_names() {
        let machines = crate::test_machines();
        let device_for = |name: &str, target: u32| {
            let offering = preset(&machines, name).unwrap();
            let mut file = machine(offering);
            file.drive.push(crate::Drive {
                name: "system".into(),
                kind: "disk".into(),
                bus: 0,
                target,
                image: "system.qcow2".into(),
                read_only: false,
            });
            let args = arguments(
                &machines,
                Path::new("/machine"),
                &file,
                offering,
                Display::None,
            )
            .unwrap();
            args[args.iter().position(|a| a.starts_with("scsi-hd")).unwrap()].clone()
        };
        // Loop IDs make the names, so no two ports share one.
        assert_eq!(
            device_for("origin3200", 1),
            "scsi-hd,bus=scsi.0,scsi-id=1,lun=0,drive=drive0,\
             port_wwn=0x2100000000000001,wwn=0x2000000000000001"
        );
        assert!(device_for("origin3200", 125)
            .ends_with("port_wwn=0x210000000000007d,wwn=0x200000000000007d"));
        // A parallel SCSI bus has no such names.
        assert_eq!(
            device_for("fuel-1", 1),
            "scsi-hd,bus=scsi.0,scsi-id=1,lun=0,drive=drive0"
        );
    }

    #[test]
    fn extra_args_cannot_take_over_control_options() {
        let bad = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            check_extra_args(&args).is_err()
        };
        assert!(bad(&["-name", "x"]));
        assert!(bad(&["-qmp", "tcp:127.0.0.1:1"]));
        assert!(bad(&["-serial", "stdio"]));
        assert!(bad(&["-chardev", "socket,id=origami-console,host=h"]));
        assert!(!bad(&["-chardev", "null,id=mine", "-msg", "timestamp=on"]));
    }

    #[test]
    fn shell_quote_survives_spaces_and_quotes() {
        assert_eq!(shell_quote("-nodefaults"), "-nodefaults");
        assert_eq!(shell_quote("a=b,c/d"), "a=b,c/d");
        assert_eq!(shell_quote(""), "''");
        if !cfg!(windows) {
            assert_eq!(shell_quote("/my dir/it's"), "'/my dir/it'\\''s'");
        }
    }

    #[test]
    fn console_line_follows_the_machine_list_and_choice() {
        let machines = crate::test_machines();
        let serial = |args: &[String]| -> Vec<String> {
            args.windows(2)
                .filter(|p| p[0] == "-serial")
                .map(|p| p[1].clone())
                .collect()
        };
        // Fuel's machine list entry gives l1 (its console), then IOC3 ports A and B.
        let fuel = preset(&machines, "fuel-1").unwrap();
        assert_eq!(default_console(fuel).unwrap(), "l1");
        assert_eq!(
            console_ports(fuel).collect::<Vec<_>>(),
            [(0, "l1", true), (1, "ioc3_a", false), (2, "ioc3_b", false)]
        );
        let mut file = machine(fuel);
        let args = arguments(&machines, Path::new("/machine"), &file, fuel, Display::None).unwrap();
        assert_eq!(serial(&args), ["stdio"]);
        file.machine.console = Some("ioc3_a".into());
        let args = arguments(&machines, Path::new("/machine"), &file, fuel, Display::None).unwrap();
        assert_eq!(serial(&args), ["none", "stdio"]);
        for chardev in [
            "stdio,id=origami-console,logfile=logs/serial.log,logappend=on",
            "socket,id=origami-console,host=127.0.0.1,port=12345,server=on,wait=off,\
             logfile=logs/serial.log,logappend=on",
        ] {
            let mut routed = args.clone();
            log_primary_serial(&mut routed, chardev.into()).unwrap();
            assert_eq!(serial(&routed), ["none", "chardev:origami-console"]);
            assert!(routed.windows(2).any(|p| p == ["-chardev", chardev]));
        }

        // Only node 0 lines of console kinds are offered.
        let mut origin2000 = preset(&machines, "origin2000-8").unwrap().clone();
        origin2000.consoles.push(crate::Console {
            kind: "second-port".into(),
            node: 1,
            argument: "-serial chardev:node1_ioc3_a".into(),
        });
        let origin2000 = &origin2000;
        assert_eq!(
            console_ports(origin2000).collect::<Vec<_>>(),
            [(0, "ioc3_a", true), (1, "ioc3_b", false)]
        );
        let mut file = machine(origin2000);
        let args = arguments(
            &machines,
            Path::new("/machine"),
            &file,
            origin2000,
            Display::None,
        )
        .unwrap();
        assert_eq!(serial(&args), ["stdio"]);
        file.machine.console = Some("elsc".into());
        let error = arguments(
            &machines,
            Path::new("/machine"),
            &file,
            origin2000,
            Display::None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("ioc3_a or ioc3_b"), "{error}");
    }

    #[test]
    fn foreground_serial_keeps_console_and_logs_output() {
        let machines = crate::test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let mut args = arguments(
            &machines,
            Path::new("/machine"),
            &machine(offer),
            offer,
            Display::None,
        )
        .unwrap();
        log_primary_serial(
            &mut args,
            "stdio,id=origami-console,logfile=logs/serial.log,logappend=on".into(),
        )
        .unwrap();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-serial", "chardev:origami-console"]));
        assert!(args.windows(2).any(|pair| pair
            == [
                "-chardev",
                "stdio,id=origami-console,logfile=logs/serial.log,logappend=on"
            ]));
    }

    #[test]
    fn vnc_port_maps_to_qemu_display_offset() {
        let machines = crate::test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let display = Display::Vnc { port: 5991 };
        let args = arguments(
            &machines,
            Path::new("/machine"),
            &machine(offer),
            offer,
            display,
        )
        .unwrap();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-display", "vnc=127.0.0.1:91"]));
    }

    #[test]
    fn user_network_forwards_bind_only_to_loopback() {
        let machines = crate::test_machines();
        let rules = vec![
            PortForward {
                name: "ssh".into(),
                protocol: "tcp".into(),
                host_port: 2222,
                guest_port: 22,
            },
            PortForward {
                name: "dns".into(),
                protocol: "udp".into(),
                host_port: 5353,
                guest_port: 53,
            },
        ];
        let expected = "user,id=net0,net=192.0.2.0/24,host=192.0.2.2,dhcpstart=192.0.2.15,hostname=machine,hostfwd=tcp:127.0.0.1:2222-:22,hostfwd=udp:127.0.0.1:5353-:53";
        let origin200 = preset(&machines, "origin200-1").unwrap();
        let mut file = machine(origin200);
        file.network.forward = rules.clone();
        let args = arguments(
            &machines,
            Path::new("/machine"),
            &file,
            origin200,
            Display::None,
        )
        .unwrap();
        assert!(args.windows(2).any(|pair| pair == ["-netdev", expected]));

        let origin2000 = preset(&machines, "origin2000-8").unwrap();
        let mut file = machine(origin2000);
        file.network.forward = rules;
        file.identity = Some(crate::Identity {
            mac: "08:00:69:12:34:56".into(),
        });
        let args = arguments(
            &machines,
            Path::new("/machine"),
            &file,
            origin2000,
            Display::None,
        )
        .unwrap();
        assert!(args.windows(2).any(|pair| pair == ["-netdev", expected]));
    }

    #[test]
    fn every_network_mode_binds_the_machine_lists_default_nic() {
        let machines = crate::test_machines();
        let nic = |name: &str, mode: &str| {
            let offer = preset(&machines, name).unwrap();
            let mut file = machine(offer);
            file.identity = Some(crate::Identity {
                mac: "08:00:69:12:34:56".into(),
            });
            file.network = Network {
                mode: mode.into(),
                endpoint: Some("tcp:127.0.0.1:49173".into()),
                interface: None,
                forward: vec![],
                share_address: None,
            };
            let args = arguments(
                &machines,
                Path::new("/machine"),
                &file,
                offer,
                Display::None,
            )
            .unwrap();
            let netdev = args.windows(2).find(|pair| pair[0] == "-netdev").unwrap()[1].clone();
            assert!(netdev.contains(",id=net0"), "{netdev}");
            args.windows(2).find(|pair| pair[0] == "-net").unwrap()[1].clone()
        };
        for mode in ["user", "private"] {
            assert_eq!(nic("onyx350-vpro", mode), "nic,model=bcm570x,netdev=net0");
            assert_eq!(
                nic("origin200-1", mode),
                "nic,model=sgi-ioc3-eth,netdev=net0"
            );
            assert_eq!(
                nic("octane-impact", mode),
                "nic,model=sgi-ioc3-eth,netdev=net0"
            );
            // A machine list naming no model: bind the first onboard NIC.
            assert_eq!(nic("origin2000-8", mode), "nic,netdev=net0");
        }
    }

    #[test]
    fn identity_mac_is_a_machine_property() {
        let machines = crate::test_machines();
        let offer = preset(&machines, "origin2000-8").unwrap();
        let mut file = machine(offer);
        file.identity = Some(crate::Identity {
            mac: "02:00:5d:aa:bb:cc".into(),
        });
        let args = arguments(
            &machines,
            Path::new("/machine"),
            &file,
            offer,
            Display::None,
        )
        .unwrap();
        let machine = &args.windows(2).find(|pair| pair[0] == "-M").unwrap()[1];
        assert!(machine.contains(",mac=02:00:5d:aa:bb:cc"), "{machine}");
        assert!(!args.iter().any(|arg| arg.contains("eeprom")));
        assert!(!args.iter().any(|arg| arg.contains("macaddr")));
    }

    #[cfg(unix)]
    #[test]
    fn private_network_uses_qemu_stream_client() {
        let machines = crate::test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let mut file = machine(offer);
        file.network = Network {
            mode: "private".into(),
            endpoint: Some("install,one.sock".into()),
            interface: None,
            forward: vec![],
            share_address: None,
        };
        file.identity = Some(crate::Identity {
            mac: "08:00:69:12:34:56".into(),
        });
        let args = arguments(
            &machines,
            Path::new("/machine"),
            &file,
            offer,
            Display::None,
        )
        .unwrap();
        assert!(args.windows(2).any(|pair| pair
            == [
                "-netdev",
                "stream,id=net0,server=off,addr.type=unix,addr.path=/machine/install,,one.sock,reconnect-ms=1000"
            ]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-net", "nic,model=sgi-ioc3-eth,netdev=net0"]));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "-M" && pair[1].contains(",mac=08:00:69:12:34:56")));
    }

    #[test]
    fn private_tcp_network_uses_loopback_stream_client() {
        let machines = crate::test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let mut file = machine(offer);
        file.network = Network {
            mode: "private".into(),
            endpoint: Some("tcp:127.0.0.1:49173".into()),
            interface: None,
            forward: vec![],
            share_address: None,
        };
        file.identity = Some(crate::Identity {
            mac: "08:00:69:12:34:56".into(),
        });
        let args = arguments(
            &machines,
            Path::new("/machine"),
            &file,
            offer,
            Display::None,
        )
        .unwrap();
        assert!(args.windows(2).any(|pair| pair
            == [
                "-netdev",
                "stream,id=net0,server=off,addr.type=inet,addr.host=127.0.0.1,addr.port=49173,reconnect-ms=1000"
            ]));
    }

    #[test]
    fn shares_put_the_machines_network_and_the_nic_on_either_side_of_the_switch() {
        let machines = crate::test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let file = machine(offer);
        let dir = Path::new("/machine");
        let args = |ports: Option<&crate::shares::Ports>| {
            arguments_with(&machines, dir, &file, offer, Display::None, ports).unwrap()
        };
        let plain = args(None);
        let ports = crate::shares::Ports {
            guest: "40001".into(),
            network: "40002".into(),
        };
        let wired = args(Some(&ports));
        // The user network stays as it was, now behind a hub port.
        let user = plain.windows(2).find(|p| p[0] == "-netdev").unwrap()[1].clone();
        assert!(wired.windows(2).any(|p| p == ["-netdev", user.as_str()]));
        let netdevs: Vec<&str> = wired
            .windows(2)
            .filter(|p| p[0] == "-netdev")
            .map(|p| p[1].as_str())
            .collect();
        assert_eq!(
            netdevs[1..],
            [
                "hubport,id=share-hub-net,hubid=1,netdev=net0",
                "stream,id=share-net,addr.type=inet,addr.host=127.0.0.1,addr.port=40002,server=off,reconnect-ms=1000",
                "hubport,id=share-hub-link,hubid=1,netdev=share-net",
                "stream,id=share-guest,addr.type=inet,addr.host=127.0.0.1,addr.port=40001,server=off,reconnect-ms=1000",
            ]
        );
        let nic = wired.windows(2).find(|p| p[0] == "-net").unwrap()[1].clone();
        assert_eq!(nic, "nic,model=sgi-ioc3-eth,netdev=share-guest");
        // A private network's stream client sits behind the hub the same way.
        let mut private = file.clone();
        private.network = Network {
            mode: "private".into(),
            endpoint: Some("tcp:127.0.0.1:49173".into()),
            ..Network::default()
        };
        private.identity = Some(crate::Identity {
            mac: "08:00:69:12:34:56".into(),
        });
        let wired =
            arguments_with(&machines, dir, &private, offer, Display::None, Some(&ports)).unwrap();
        assert!(wired.windows(2).any(|p| p
            == [
                "-netdev",
                "stream,id=net0,server=off,addr.type=inet,addr.host=127.0.0.1,addr.port=49173,reconnect-ms=1000"
            ]));
        assert!(wired
            .windows(2)
            .any(|p| p == ["-netdev", "hubport,id=share-hub-net,hubid=1,netdev=net0"]));
        // Without shares the arguments are what they always were.
        assert!(!plain
            .iter()
            .any(|a| a.contains("share-hub") || a.contains("share-guest")));
    }

    #[test]
    fn host_networks_attach_the_ioc3_to_a_tap_or_socket_vmnet() {
        let machines = crate::test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let mut file = machine(offer);
        file.identity = Some(crate::Identity {
            mac: "08:00:69:12:34:56".into(),
        });
        let netdev = |file: &MachineFile| {
            let args =
                arguments(&machines, Path::new("/machine"), file, offer, Display::None).unwrap();
            assert!(args
                .windows(2)
                .any(|pair| pair == ["-net", "nic,model=sgi-ioc3-eth,netdev=net0"]));
            args.windows(2).find(|pair| pair[0] == "-netdev").unwrap()[1].clone()
        };
        file.network = Network {
            mode: "tap".into(),
            endpoint: None,
            interface: Some("TAP,lab".into()),
            forward: vec![],
            share_address: None,
        };
        let expected = if cfg!(windows) {
            "tap,id=net0,ifname=TAP,,lab"
        } else {
            "tap,id=net0,ifname=TAP,,lab,script=,downscript="
        };
        assert_eq!(netdev(&file), expected);
        assert_eq!(
            crate::validate_network(&file).is_ok(),
            !cfg!(target_os = "macos")
        );
        assert!(network_hint(&file).unwrap().contains("TAP,lab"));

        file.network = Network {
            mode: "vmnet".into(),
            endpoint: Some("/opt/homebrew/var/run/socket_vmnet".into()),
            interface: None,
            forward: vec![],
            share_address: None,
        };
        assert_eq!(
            netdev(&file),
            "stream,id=net0,server=off,addr.type=unix,\
             addr.path=/opt/homebrew/var/run/socket_vmnet,reconnect-ms=1000"
        );
        assert_eq!(
            crate::validate_network(&file).is_ok(),
            cfg!(target_os = "macos")
        );
        assert!(network_hint(&file).unwrap().contains("socket_vmnet"));

        file.identity = None;
        for mode in ["tap", "vmnet"] {
            file.network.mode = mode.into();
            let error = crate::validate_network(&file).unwrap_err().to_string();
            assert!(error.contains(mode), "{error}");
        }
        file.network.interface = None;
        file.network.mode = "tap".into();
        if !cfg!(target_os = "macos") {
            let error = crate::validate_network(&file).unwrap_err().to_string();
            assert!(error.contains("--interface"), "{error}");
        }
    }
}
