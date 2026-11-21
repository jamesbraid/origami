use crate::catalogue::{offering_data, OfferingData};
use crate::{control, qemu_path_option};
use origami_machine::addressing::{dhcp_hostname, Addresses};
use origami_machine::stores::state_path;
use origami_machine::{
    control as run_record, resolve, tcp_endpoint, Drive, MachineFile, Machines, Offering, Result,
};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Local,
    Vnc {
        port: u16,
    },
    /// No window and no VNC: the machine publishes its graphic heads through
    /// shared memory and a local socket named by [`shm_name`], for a GUI that
    /// draws them.
    Shm {
        /// The Vulkan `deviceUUID` of the viewer's adapter, so QEMU exports
        /// scanout memory a viewer on it imports without a copy. None asks
        /// for plain shared memory.
        gpu: Option<GpuUuid>,
    },
    /// [`Display::Shm`] on a QEMU with the version 4 `display-shm` device;
    /// `fd` is the runner's listening socket, else QEMU binds [`display_socket`].
    ShmV4 {
        fd: Option<i32>,
    },
    None,
}

/// Which display protocol a QEMU's shared-memory display speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShmProtocol {
    /// The machine's `display-shm` property.
    V3,
    /// The `display-shm` device.
    V4,
}

/// How long a QEMU gets to answer the probe.
const PROBE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(500)
} else {
    Duration::from_secs(30)
};

/// Ask `qemu` whether it has the `display-shm` device type. Version 3 for
/// one that predates it or cannot be asked (not remembered).
pub fn shm_protocol(qemu: &Path) -> ShmProtocol {
    static KNOWN: std::sync::Mutex<Vec<(PathBuf, ShmProtocol)>> = std::sync::Mutex::new(Vec::new());
    let known = |qemu: &Path| {
        let known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
        known.iter().find(|(path, _)| path == qemu).map(|k| k.1)
    };
    if let Some(found) = known(qemu) {
        return found;
    }
    let Some(has) = has_display_device(qemu) else {
        return ShmProtocol::V3;
    };
    let found = if has {
        ShmProtocol::V4
    } else {
        ShmProtocol::V3
    };
    KNOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((qemu.to_path_buf(), found));
    found
}

#[derive(serde::Serialize)]
struct QomListTypes {
    implements: &'static str,
    #[serde(rename = "abstract")]
    abstract_: bool,
}

impl qapi::Command for QomListTypes {
    type Ok = serde_json::Value;
    const NAME: &'static str = "qom-list-types";
    const ALLOW_OOB: bool = false;
}

/// Whether `qemu` lists the `display-shm` device, or None if it did not
/// answer in time.
fn has_display_device(qemu: &Path) -> Option<bool> {
    let types = crate::catalogue::ask_machineless(qemu, Some(PROBE_TIMEOUT), |qmp| {
        qmp.execute(&QomListTypes {
            implements: "device",
            abstract_: false,
        })
        .map_err(|e| e.to_string())
    })
    .ok()?;
    Some(
        types
            .as_array()?
            .iter()
            .any(|ty| ty.get("name").and_then(|n| n.as_str()) == Some("display-shm")),
    )
}

/// The socket the runner listens on for a version 4 display, removed when
/// the machine ends.
#[cfg(unix)]
struct DisplayListener {
    listener: std::os::unix::net::UnixListener,
    path: PathBuf,
}

#[cfg(unix)]
impl DisplayListener {
    fn bind(path: PathBuf) -> std::io::Result<DisplayListener> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        // Bound in a directory only we can enter, then moved into place, so
        // the socket never exists for others at the umask's mode. The
        // temporary path is shorter than `display.sock`, so it fits too.
        let parent = path.parent().unwrap_or(Path::new("."));
        let private = parent.join(format!(".d{}", std::process::id()));
        let _ = fs::remove_dir_all(&private);
        fs::DirBuilder::new().mode(0o700).create(&private)?;
        let bound = (|| -> std::io::Result<_> {
            let temporary = private.join("s");
            let listener = std::os::unix::net::UnixListener::bind(&temporary)?;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
            // The machine lock keeps a second runner out; a file here is left from a killed one.
            let _ = fs::remove_file(&path);
            fs::rename(&temporary, &path)?;
            Ok(listener)
        })();
        let _ = fs::remove_dir_all(&private);
        Ok(DisplayListener {
            listener: bound?,
            path,
        })
    }
}

#[cfg(unix)]
impl Drop for DisplayListener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// A Vulkan `deviceUUID`: 16 bytes, written as 32 hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuUuid(pub [u8; 16]);

impl GpuUuid {
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl std::str::FromStr for GpuUuid {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{s:?} is not 32 hex digits"));
        }
        let mut uuid = [0u8; 16];
        for (i, byte) in uuid.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|e| e.to_string())?;
        }
        Ok(GpuUuid(uuid))
    }
}

/// `dir` without Windows' verbatim prefix (`\\?\` or `\\?\UNC\`), so the core and
/// the runner derive display names and socket paths from one spelling.
pub fn plain_path(dir: &Path) -> PathBuf {
    let Some(text) = dir.to_str() else {
        return dir.to_path_buf();
    };
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else {
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text))
    }
}

/// The name a machine run with [`Display::Shm`] gives its display
/// (`display-shm=NAME`). It comes from the machine directory, so two
/// directories never share a name. Windows' verbatim spelling (`\\?\C:\...`)
/// hashes like the plain one, because the core and the runner spell a path
/// differently and must agree.
pub fn shm_name(dir: &Path) -> String {
    let plain = plain_path(dir);
    let digest = Sha256::digest(plain.as_os_str().as_encoded_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("origami-{hex}")
}

/// The longest socket path the hosts take: a `sockaddr_un` holds 104 bytes on
/// macOS and 108 on Linux and Windows, one of them the terminator.
const SOCKET_PATH_MAX: usize = 100;

/// Where a machine run with [`Display::Shm`] listens for its viewer, passed to
/// QEMU as `display-shm-socket`, so a viewer finds it from the machine
/// directory alone and not from anyone's environment. It is
/// `state/display.sock` in the machine directory when that path is short
/// enough for a socket, else a short name in the system's temporary directory.
pub fn display_socket(dir: &Path) -> PathBuf {
    let dir = &plain_path(dir);
    let own = dir.join("state").join("display.sock");
    if own.as_os_str().len() <= SOCKET_PATH_MAX {
        return own;
    }
    let base = if cfg!(windows) {
        std::env::temp_dir()
    } else {
        PathBuf::from("/tmp")
    };
    base.join(format!("{}.sock", shm_name(dir)))
}

/// How long QEMU has to answer on QMP after it starts. QEMU opens the control
/// port at once but greets only when its main loop runs. The first start of a
/// CHD overlay, which has no diff file yet, took 33 seconds to greet for a
/// 1.6 GB image on a busy host; a start with the diff in place took 1 second.
const QMP_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// `QMP_READY_TIMEOUT`, or the seconds in ORIGAMI_QMP_TIMEOUT, which the tests
/// of a QEMU that never answers set so they do not wait two minutes.
pub(crate) fn qmp_ready_timeout() -> Duration {
    std::env::var("ORIGAMI_QMP_TIMEOUT")
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .map_or(QMP_READY_TIMEOUT, Duration::from_secs)
}

/// The display device id and head number of each graphic head, in head order.
pub fn head_devices(
    machines: &Machines,
    offering: &Offering,
    file: &MachineFile,
) -> Vec<(String, u32)> {
    origami_machine::graphics::heads(machines, &offering.topology, &file.pipe)
        .into_iter()
        .map(|h| (h.slot, h.head))
        .collect()
}

/// Where each head of the machine is served for `--display vnc` from
/// `port`: the head and its TCP port, in port order. A machine with one
/// head has the single server a VNC display always had.
pub fn vnc_ports(
    machines: &Machines,
    offering: &Offering,
    file: &MachineFile,
    port: u16,
) -> Result<Vec<(origami_machine::graphics::Head, u16)>> {
    let heads = origami_machine::graphics::heads(machines, &offering.topology, &file.pipe);
    let last = u32::from(port) + heads.len().saturating_sub(1) as u32;
    if last > u32::from(u16::MAX) {
        return Err(format!(
            "{} heads need VNC ports {port} to {last}, past the last port",
            heads.len()
        )
        .into());
    }
    Ok(heads.into_iter().zip(port..).collect())
}

/// Where the bundled programs are. Resolved once, from `ORIGAMI_RUNTIME_DIR`
/// when it is set (a directory holding QEMU and its tools, as the archive's
/// `libexec/origami` does) and otherwise relative to this executable.
#[derive(Clone, Debug)]
pub struct Runtime {
    dir: PathBuf,
    data: PathBuf,
    instigator: PathBuf,
}

impl Runtime {
    pub fn locate() -> Result<Self> {
        Self::resolve(
            std::env::var_os("ORIGAMI_RUNTIME_DIR").map(PathBuf::from),
            &std::env::current_exe()?,
        )
    }

    /// The runtime for the executable `exe` and the runtime directory
    /// named by the environment, if any.
    fn resolve(selected: Option<PathBuf>, exe: &Path) -> Result<Self> {
        let beside = exe
            .parent()
            .ok_or("cannot locate origami executable directory")?;
        let (dir, instigator) = match selected {
            Some(dir) => {
                // A runtime directory may carry its own Instigator; without
                // one, the product's own sits beside this executable.
                let own = dir.join(binary_name("instigator"));
                let instigator = if own.is_file() {
                    own
                } else {
                    product_program(beside, "instigator")
                };
                (dir, instigator)
            }
            None => (
                beside.join("..").join("libexec").join("origami"),
                product_program(beside, "instigator"),
            ),
        };
        let data = dir
            .join("..")
            .join("..")
            .join("share")
            .join("origami")
            .join("qemu");
        Ok(Self {
            dir,
            data,
            instigator,
        })
    }

    pub fn qemu(&self) -> PathBuf {
        self.dir.join(binary_name("qemu-system-mips64"))
    }

    pub fn qemu_img(&self) -> PathBuf {
        self.dir.join(binary_name("qemu-img"))
    }

    pub fn machine_init(&self) -> PathBuf {
        self.dir.join(binary_name("qemu-sgi-machine-init"))
    }

    /// QEMU's data directory, which holds the keymaps.
    pub fn data(&self) -> &Path {
        &self.data
    }

    /// The options that point QEMU at its data directory.
    pub fn data_arguments(&self) -> [String; 2] {
        ["-L".into(), self.data.display().to_string()]
    }

    /// The bundled Instigator, which must exist.
    pub fn instigator(&self) -> Result<&Path> {
        if !self.instigator.is_file() {
            return Err(
                format!("packaged Instigator missing: {}", self.instigator.display()).into(),
            );
        }
        Ok(&self.instigator)
    }
}

fn binary_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.into()
    }
}

/// A product program in the sibling `bin` or beside the executable: an app
/// named `Origami` cannot share a directory with `origami` on macOS or Windows.
pub(crate) fn product_program(exe_dir: &Path, stem: &str) -> PathBuf {
    let name = binary_name(stem);
    if exe_dir.file_name().is_some_and(|dir| dir == "bin") {
        return exe_dir.join(name);
    }
    let sibling = exe_dir.join("..").join("bin").join(&name);
    if sibling.is_file() {
        sibling
    } else {
        exe_dir.join(name)
    }
}

fn user_network(dir: &Path, file: &MachineFile) -> Result<String> {
    let mut option = crate::user_netdev(&Addresses::of(file)?);
    // DHCP offers the directory name as the guest's hostname when it is a
    // valid one; IRIX keeps its own name otherwise.
    if let Some(name) = dhcp_hostname(dir) {
        option.push_str(&format!(",hostname={name}"));
    }
    for rule in &file.network.forward {
        option.push_str(&format!(
            ",hostfwd={}:127.0.0.1:{}-:{}",
            rule.protocol, rule.host_port, rule.guest_port
        ));
    }
    Ok(option)
}

/// One fixed suggestion per host-backed mode, shown beside QEMU's own error.
pub fn network_hint(file: &MachineFile) -> Option<String> {
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
    let extra_args = file.engine.extra_args();
    check_extra_args(&extra_args)?;
    check_extra_args(once)?;
    args.extend(extra_args);
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

pub use origami_machine::shell_quote;

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
    shares: Option<&origami_machine::shares::Ports>,
) -> Result<Vec<String>> {
    let data = offering_data(offering)?;
    let mut machine = data.machine_options.clone();
    // The Octane and O2 machines have one CPU board and no population property.
    if !matches!(offering.product.as_str(), "octane" | "octane2" | "o2") {
        let population = offering
            .cpus_per_node
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(":");
        machine.push_str(&format!(",population={population}"));
    }
    origami_machine::settings::validate(machines, offering, &file.machine.inputs)?;
    let inputs = crate::settings::render(machines, offering, &file.machine.inputs)?;
    machine.push_str(&inputs.machine);
    let cpu = format!("{}{}", data.cpu, inputs.cpu);
    if let Some(identity) = &file.identity {
        machine.push_str(&format!(
            ",{}={}",
            data.mac_property(offering),
            identity.mac
        ));
        if let Some(emu_id) = identity.emu_id {
            machine.push_str(&format!(",emu-id={emu_id}"));
        }
    }
    if data.needs_debug_leds_off {
        machine.push_str(",debug-leds=off");
    }
    if let Display::Shm { gpu } = display {
        machine.push_str(&format!(
            ",display-shm={},display-shm-socket={}",
            shm_name(dir),
            qemu_path_option(&display_socket(dir))
        ));
        if let Some(gpu) = gpu {
            machine.push_str(&format!(",display-shm-gpu={}", gpu.hex()));
        }
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
    // The O2 starts its PROM from the flash store but still wants the image
    // named, with its geometry. The store is the whole array and is a regular
    // file of the right size, so it stands for the image.
    if data.launch_recipe.as_deref() == Some("ip32") {
        let flash = offering
            .storage
            .iter()
            .find(|item| item.initial == "boot-prom")
            .zip(offering.firmware.as_ref())
            .ok_or("the O2 launch recipe needs a boot PROM store and firmware size")?;
        machine.push_str(&format!(
            ",rom-image={},rom-size={},rom-offset=0,rom-pad-byte=255",
            qemu_path_option(&state_path(dir, flash.0)),
            flash.1.size
        ));
    }
    let memory: u32 = file
        .machine
        .memory_per_node
        .trim_end_matches("MiB")
        .parse()?;
    // A machine that starts with instruction counting runs on virtual time:
    // counting needs one TCG thread, and the guest's clock follows the count.
    let counted_clock = offering.clock.as_ref().and_then(|clock| {
        clock
            .default_icount_shift
            .map(|shift| (shift, clock.default_rtc.as_deref().unwrap_or("vm")))
    });
    let mut args = vec![
        // Default devices create a second, empty SDL window beside RAD4.
        "-nodefaults".into(),
        "-accel".into(),
        if counted_clock.is_some() {
            "tcg,thread=single"
        } else if offering.cpus > 1 {
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
        offering.cpus.to_string(),
        "-m".into(),
        (memory * offering.nodes).to_string(),
    ];
    if let Some((shift, rtc)) = counted_clock {
        args.extend([
            "-icount".into(),
            format!("shift={shift},align=off,sleep=off"),
            "-rtc".into(),
            format!("clock={rtc}"),
        ]);
    }
    args.extend(storage);
    match display {
        Display::Local => args.extend(["-display".into(), "sdl,window-close=off".into()]),
        Display::Vnc { port } => {
            let ports = vnc_ports(machines, offering, file, port)?;
            if ports.len() > 1 {
                // One server per head, each showing its own console.
                for (head, port) in ports {
                    args.extend([
                        "-display".into(),
                        format!(
                            "vnc=127.0.0.1:{},display={},head={}",
                            port - 5900,
                            head.slot,
                            head.head
                        ),
                    ]);
                }
            } else {
                args.extend(["-display".into(), format!("vnc=127.0.0.1:{}", port - 5900)])
            }
        }
        Display::Shm { .. } | Display::None => args.extend(["-display".into(), "none".into()]),
        Display::ShmV4 { fd } => {
            args.extend(["-display".into(), "none".into(), "-device".into()]);
            args.push(match fd {
                Some(fd) => format!("display-shm,fd={fd}"),
                None => format!(
                    "display-shm,path={}",
                    qemu_path_option(&display_socket(dir))
                ),
            });
        }
    }
    for global in &inputs.globals {
        args.extend(["-global".into(), global.clone()]);
    }
    args.extend(crate::graphics::arguments(
        machines,
        &offering.topology,
        &file.pipe,
    )?);
    // IRIX numbers a keyboard and mouse into /etc/ioconfig.conf at boot, and
    // one plugged in later keeps ID -1, which X never opens. So they are
    // present from power-on on every run. A relative mouse: IRIX reads a
    // tablet's absolute coordinates as signed relative motion.
    if origami_machine::profiles::usb_input_only(offering) {
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
    for _ in 0..origami_machine::serial::console_line(offering, file.machine.console.as_deref())? {
        args.extend(["-serial".into(), "none".into()]);
    }
    args.extend(["-serial".into(), "stdio".into()]);
    let backend = match file.network.mode.as_str() {
        "none" => None,
        "user" => Some(user_network(dir, file)?),
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
            // A private network without an endpoint is one `run` hosts, on a
            // port it picks when it starts.
            let endpoint = match file.network.endpoint.as_deref() {
                Some(endpoint) => endpoint,
                None if mode == "private" => "tcp:127.0.0.1:<hosted-port>",
                None => return Err(format!("{mode} network needs endpoint").into()),
            };
            if endpoint.contains('<') {
                format!(
                    "stream,id=net0,server=off,addr.type=inet,addr.host=127.0.0.1,addr.port={},reconnect-ms=1000",
                    "<hosted-port>"
                )
            } else if let Some(address) = tcp_endpoint(endpoint)? {
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
            args.extend([
                "-net".into(),
                nic_option(offering, &data, file, "share-guest"),
            ]);
        }
        (Some(backend), None) => args.extend([
            "-netdev".into(),
            backend,
            "-net".into(),
            nic_option(offering, &data, file, "net0"),
        ]),
        (None, _) => args.extend(["-nic".into(), "none".into()]),
    }
    Ok(args)
}

/// The `-net nic` binding net0 to the machine's default onboard Ethernet.
/// Its address is the machine's mac= unless the machine list says the
/// adapter takes its own. A model-less NIC binds the first onboard one, for
/// a QEMU whose machine list does not name the model.
fn nic_option(
    offering: &Offering,
    data: &OfferingData,
    file: &MachineFile,
    netdev: &str,
) -> String {
    let adapter = offering.default_network();
    let nic = data.default_nic(offering);
    let mut option = "nic".to_string();
    if let Some(model) = nic.and_then(|nic| nic.model.as_deref()) {
        option.push_str(&format!(",model={model}"));
    }
    option.push_str(&format!(",netdev={netdev}"));
    if let (Some(adapter), Some(nic), Some(identity)) = (adapter, nic, &file.identity) {
        if let (Some(mac), Some(name)) = (&adapter.mac, &nic.mac_option) {
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
    } else if origami_machine::is_chd(&path) {
        "chd"
    } else {
        "raw"
    };
    // A CHD disk takes guest writes into <image>.diff.chd and folds them
    // back at a clean exit; overlay=on keeps the diff instead.
    let overlay = if drive.overlay { ",overlay=on" } else { "" };
    let id = format!("drive{index}");
    args.extend([
        "-drive".into(),
        format!(
            "if=none,id={id},file={},format={format},readonly={}{overlay}",
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
    let bus = offering.scsi_bus_name(drive.bus);
    let loop_names = if offering
        .scsi_bus(drive.bus)
        .is_some_and(|adapter| adapter.kind == "fc-loop")
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

/// Who started a runner, and how.
#[derive(Clone, Debug, Default)]
pub struct Start {
    /// Start QEMU paused, until a client sends `cont`.
    pub hold: bool,
    /// Give the guest no sound.
    pub no_audio: bool,
    /// `cli` or `app`, as `run.json` records it.
    pub started_by: &'static str,
}

/// Start the machine in a background runner and return the runner's pid.
pub fn start_background(dir: &Path, display: Display, once: &[String]) -> Result<u32> {
    let start = Start {
        started_by: "cli",
        ..Start::default()
    };
    start_background_with(&std::env::current_exe()?, dir, display, once, &start)
}

/// [`start_background`] with the runner `exe`, for a caller that is not the
/// `origami` executable itself, such as a GUI that starts machines in the
/// `origami` beside it.
pub fn start_background_as(
    exe: &Path,
    dir: &Path,
    display: Display,
    once: &[String],
) -> Result<u32> {
    let start = Start {
        started_by: "app",
        ..Start::default()
    };
    start_background_with(exe, dir, display, once, &start)
}

/// Whether no runner holds the machine lock, waiting up to `patience` for one
/// that is exiting: a lock with no run record is an exit in progress.
fn runner_released(dir: &Path, patience: Duration) -> Result<bool> {
    let deadline = Instant::now() + patience;
    while run_record::is_locked(dir)? {
        if run_record::read(dir).is_ok() || Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(true)
}

/// [`start_background_as`] with the start's other choices.
pub fn start_background_with(
    exe: &Path,
    dir: &Path,
    display: Display,
    once: &[String],
    start: &Start,
) -> Result<u32> {
    if !runner_released(dir, Duration::from_secs(5))? {
        return Err("machine is already running".into());
    }
    fs::create_dir_all(dir.join("logs"))?;
    let log_path = dir.join("logs/runner.log");
    crate::quiet::rotate(&log_path);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_start = log.metadata()?.len();
    let display_name = match display {
        Display::Local => "local",
        Display::Vnc { .. } => "vnc",
        Display::Shm { .. } | Display::ShmV4 { .. } => "shm",
        Display::None => "none",
    };
    let mut child = Command::new(exe);
    child
        .arg("_serve")
        .arg(dir)
        .arg("--display")
        .arg(display_name);
    if let Display::Vnc { port } = display {
        child.arg("--vnc-port").arg(port.to_string());
    }
    if let Display::Shm { gpu: Some(gpu) } = display {
        child.arg("--display-gpu").arg(gpu.hex());
    }
    if start.hold {
        child.arg("--hold");
    }
    if start.no_audio {
        child.arg("--no-audio");
    }
    if !start.started_by.is_empty() {
        child.arg("--started-by").arg(start.started_by);
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
    // The runner process waits qmp_ready_timeout() for QEMU, so wait a little longer.
    let deadline = Instant::now() + qmp_ready_timeout() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(record) = run_record::read(dir) {
            // An engine with no QMP is up once its runner has written the record.
            if record.pid == child.id()
                && (record.qmp_port == 0 || control::verified_qmp(&record).is_ok())
            {
                return Ok(record.pid);
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

/// What the runner asks of a launch beyond the machine itself.
#[derive(Clone, Debug, Default)]
pub struct LaunchOptions {
    /// Start QEMU paused (`-S`), until the runner is told to continue.
    pub hold: bool,
    /// Give the guest no sound. Audio is on unless this is set.
    pub no_audio: bool,
}

/// A QEMU the runner has started and found answering QMP. Dropping it
/// releases the machine lock and the share switch, so the owner keeps it
/// for as long as the guest lives.
pub struct Launched {
    child: Option<Child>,
    pid: u32,
    pub qmp_port: u16,
    /// QEMU's console chardev, which only the runner connects to.
    pub engine_console: u16,
    pub name: String,
    /// Removable drives: guest name and QEMU block device id.
    pub removable: Vec<(String, String)>,
    /// Each graphic head's display device id and its head number there.
    pub heads: Vec<(String, u32)>,
    pub display: Option<PathBuf>,
    /// The display protocol the machine speaks, 0 for none: 3 or 4.
    pub display_protocol: u32,
    #[cfg(unix)]
    display_listener: Option<DisplayListener>,
    _lock: run_record::EditLock,
    _share_net: Option<origami_machine::shares::Running>,
}

impl Launched {
    /// QEMU's process id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The listening socket of a version 4 display, to drop when QEMU exits.
    pub(crate) fn take_display_guard(&mut self) -> Option<Box<dyn std::any::Any + Send>> {
        #[cfg(unix)]
        {
            self.display_listener
                .take()
                .map(|l| Box::new(l) as Box<dyn std::any::Any + Send>)
        }
        #[cfg(not(unix))]
        None
    }

    /// Hand the process to whoever watches it; once.
    pub(crate) fn take_child(&mut self) -> Child {
        self.child.take().expect("the QEMU process is taken once")
    }
}

/// Start QEMU for the machine in `dir`, with its console and QMP on private
/// loopback ports, and return once QMP answers. The caller serves the
/// machine's console and control sockets and writes the run record.
#[allow(clippy::too_many_arguments)]
pub fn launch(
    rt: &Runtime,
    machines: &Machines,
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
    once: &[String],
    options: &LaunchOptions,
) -> Result<Launched> {
    let qemu = rt.qemu();
    if !qemu.is_file() {
        return Err(format!("packaged QEMU missing: {}", qemu.display()).into());
    }
    let keymap = rt.data().join("keymaps/en-us");
    if !keymap.is_file() {
        return Err(format!("packaged QEMU keymap missing: {}", keymap.display()).into());
    }
    // Asked before the machine lock and the share switch are taken: a QEMU
    // that hangs must not hold them while the probe times out.
    let protocol = match display {
        Display::Shm { .. } => shm_protocol(&qemu),
        _ => ShmProtocol::V3,
    };
    // A machine without its storage is not started, and gains nothing.
    origami_machine::stores::check_state(dir, offering)?;
    let lock = run_record::lock_for_edit(dir, "starting it again")?;
    for warning in origami_machine::emu_id::running_twin_warnings(dir, file) {
        eprintln!("warning: {warning}");
    }
    // The switch runs in the runner, which also waits on QEMU, so it
    // lives exactly as long as the guest.
    let share_net = origami_machine::shares::start(dir, file)?;
    // A hosted private network is the NIC's own peer; any other switch sits
    // beside the machine's network.
    let hosted = share_net.as_ref().and_then(|net| net.hosted_endpoint());
    let ports = share_net
        .as_ref()
        .filter(|_| hosted.is_none())
        .map(|net| net.ports());
    let mut hosted_file;
    let file = match &hosted {
        Some(endpoint) => {
            hosted_file = file.clone();
            hosted_file.network.endpoint = Some(endpoint.clone());
            eprintln!(
                "hosting the private network at {}",
                Addresses::of(file)?.host()
            );
            &hosted_file
        }
        None => file,
    };
    #[cfg(unix)]
    let (display, display_listener) = match display {
        Display::Shm { .. } if protocol == ShmProtocol::V4 => {
            let listener = DisplayListener::bind(display_socket(dir))?;
            use std::os::fd::AsRawFd;
            (
                Display::ShmV4 {
                    fd: Some(listener.listener.as_raw_fd()),
                },
                Some(listener),
            )
        }
        other => (other, None),
    };
    // Without an inherited socket QEMU binds the display socket itself.
    #[cfg(not(unix))]
    let display = match display {
        Display::Shm { .. } if protocol == ShmProtocol::V4 => Display::ShmV4 { fd: None },
        other => other,
    };
    let mut args = arguments_with(machines, dir, file, offering, display, ports.as_ref())?;
    args.splice(0..0, rt.data_arguments());
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
    let (audio, note) = crate::audio::arguments(&qemu, !options.no_audio);
    if let Some(note) = note {
        eprintln!("origami: {note}");
    }
    args.extend(audio);
    let (mut engine_console, mut qmp_port) = fresh_ports()?;
    let name = format!(
        "sgi-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    background_arguments(
        &mut args,
        file,
        once,
        engine_console,
        qmp_port,
        &name,
        options.hold,
    )?;
    let removable = file
        .drive
        .iter()
        .enumerate()
        .filter(|(_, drive)| drive.kind == "cdrom")
        .map(|(index, drive)| (drive.name.clone(), format!("drive{index}")))
        .collect();
    let heads = head_devices(machines, offering, file);
    let mut attempts = 1;
    'launch: loop {
        let mut command = Command::new(&qemu);
        // QEMU's error output goes through `quiet::relay`, which collapses repeats.
        command
            .args(&args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stderr(Stdio::piped());
        // The listening socket is made close-on-exec; QEMU is told its number.
        #[cfg(unix)]
        if let Display::ShmV4 { fd: Some(fd) } = display {
            use std::os::unix::process::CommandExt;
            // SAFETY: only async-signal-safe fcntl runs between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = command.spawn()?;
        let relay = child
            .stderr
            .take()
            .map(|stderr| std::thread::spawn(move || crate::quiet::relay(stderr)));
        let probe = run_record::Record {
            pid: std::process::id(),
            qmp_port,
            console_port: engine_console,
            name: name.clone(),
            qemu_pid: child.id(),
            ..Default::default()
        };
        let deadline = Instant::now() + qmp_ready_timeout();
        while Instant::now() < deadline {
            if control::verified_qmp(&probe).is_ok() {
                return Ok(Launched {
                    pid: child.id(),
                    child: Some(child),
                    qmp_port,
                    engine_console,
                    name,
                    removable,
                    heads,
                    display: matches!(display, Display::Shm { .. } | Display::ShmV4 { .. })
                        .then(|| display_socket(dir)),
                    display_protocol: match display {
                        Display::Shm { .. } => 3,
                        Display::ShmV4 { .. } => 4,
                        _ => 0,
                    },
                    #[cfg(unix)]
                    display_listener,
                    _lock: lock,
                    _share_net: share_net,
                });
            }
            if let Some(status) = child.try_wait()? {
                // Another process can take a port between free_port and QEMU's bind.
                let last = relay
                    .and_then(|r| r.join().ok().flatten())
                    .unwrap_or_default();
                if port_was_taken(&last) && attempts < PORT_ATTEMPTS {
                    attempts += 1;
                    let new = fresh_ports()?;
                    retarget_ports(&mut args, (engine_console, qmp_port), new);
                    (engine_console, qmp_port) = new;
                    continue 'launch;
                }
                let hint = network_hint(file).map_or(String::new(), |hint| format!("\n{hint}"));
                return Err(format!("QEMU exited before QMP was ready ({status}){hint}").into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        child.kill()?;
        child.wait()?;
        return Err("QEMU did not open its QMP control endpoint".into());
    }
}

/// The options a background run adds to [`arguments_with`]: the console and
/// QMP endpoints on loopback ports, the process name, `-S` for a start
/// paused, and the unsupported options last.
pub(crate) fn background_arguments(
    args: &mut Vec<String>,
    file: &MachineFile,
    once: &[String],
    console_port: u16,
    qmp_port: u16,
    name: &str,
    paused: bool,
) -> Result<()> {
    log_primary_serial(args, console_chardev(console_port))?;
    args.extend([
        "-qmp".into(),
        qmp_endpoint(qmp_port),
        "-name".into(),
        // Names the vCPU threads ("CPU 0/TCG"); macOS reports no usable ids.
        format!("{name},debug-threads=on"),
    ]);
    if paused {
        args.push("-S".into());
    }
    append_extra_args(args, file, once)
}

pub(crate) const PORT_ATTEMPTS: u32 = 4;

fn console_chardev(port: u16) -> String {
    format!("socket,id=origami-console,host=127.0.0.1,port={port},server=on,wait=off,logfile=logs/serial.log,logappend=on")
}

fn qmp_endpoint(port: u16) -> String {
    format!("tcp:127.0.0.1:{port},server=on,wait=off")
}

/// Fresh loopback ports for a launch that lost the race for its first pair.
pub(crate) fn fresh_ports() -> Result<(u16, u16)> {
    let console = run_record::free_port()?;
    let mut qmp = run_record::free_port()?;
    while qmp == console {
        qmp = run_record::free_port()?;
    }
    Ok((console, qmp))
}

/// Whether QEMU's last error says a port it was told to listen on was taken.
pub(crate) fn port_was_taken(last_error: &str) -> bool {
    last_error.contains("Address already in use")
}

/// Swap the ports [`background_arguments`] wrote for new ones.
pub(crate) fn retarget_ports(args: &mut [String], old: (u16, u16), new: (u16, u16)) {
    for arg in args {
        if *arg == console_chardev(old.0) {
            *arg = console_chardev(new.0);
        } else if *arg == qmp_endpoint(old.1) {
            *arg = qmp_endpoint(new.1);
        }
    }
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
    use crate::test_machines;
    use origami_machine::{preset, Machine, Network, PortForward};

    #[test]
    fn a_runner_that_is_exiting_is_waited_for_and_a_running_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let lock = run_record::lock_for_edit(dir.path(), "t").unwrap();
        let held = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(lock);
        });
        assert!(runner_released(dir.path(), Duration::from_secs(5)).unwrap());
        held.join().unwrap();
        let _lock = run_record::lock_for_edit(dir.path(), "t").unwrap();
        assert!(!runner_released(dir.path(), Duration::from_millis(100)).unwrap());
        let started = Instant::now();
        run_record::write(dir.path(), &run_record::Record::default()).unwrap();
        assert!(!runner_released(dir.path(), Duration::from_secs(5)).unwrap());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    fn machine(offering: &Offering) -> MachineFile {
        MachineFile {
            format: origami_machine::MACHINE_FORMAT,
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
            firmware: None,
            firstboot: None,
            engine: origami_machine::model::EngineChoice::qemu(&[]),
        }
    }

    #[test]
    fn the_runtime_is_found_beside_the_executable_unless_one_is_selected() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("bin").join(binary_name("origami"));
        let runtime = Runtime::resolve(None, &exe).unwrap();
        let libexec = root
            .path()
            .join("bin")
            .join("..")
            .join("libexec")
            .join("origami");
        assert_eq!(
            runtime.qemu(),
            libexec.join(binary_name("qemu-system-mips64"))
        );
        assert_eq!(runtime.qemu_img(), libexec.join(binary_name("qemu-img")));
        assert_eq!(
            runtime.machine_init(),
            libexec.join(binary_name("qemu-sgi-machine-init"))
        );
        assert_eq!(
            runtime.data_arguments(),
            [
                "-L".to_string(),
                libexec
                    .join("..")
                    .join("..")
                    .join("share")
                    .join("origami")
                    .join("qemu")
                    .display()
                    .to_string()
            ]
        );
        // Instigator sits with the executable, and is missing here.
        assert!(runtime
            .instigator()
            .unwrap_err()
            .to_string()
            .contains("missing"));

        let selected = root.path().join("selected");
        let runtime = Runtime::resolve(Some(selected.clone()), &exe).unwrap();
        assert_eq!(
            runtime.qemu(),
            selected.join(binary_name("qemu-system-mips64"))
        );
    }

    #[test]
    fn an_app_outside_bin_finds_the_programs_in_the_sibling_bin() {
        let root = tempfile::tempdir().unwrap();
        let (app, bin) = (root.path().join("app"), root.path().join("bin"));
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join(binary_name("origami")), "").unwrap();
        // The app's own executable would answer for `origami` on a
        // case-insensitive filesystem.
        fs::write(app.join(binary_name("Origami")), "").unwrap();
        assert_eq!(
            product_program(&app, "origami"),
            app.join("..").join("bin").join(binary_name("origami"))
        );
        assert_eq!(
            product_program(&app, "instigator"),
            app.join(binary_name("instigator"))
        );
    }

    #[test]
    fn a_selected_runtime_directory_may_carry_its_own_instigator() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let selected = root.path().join("selected");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&selected).unwrap();
        let exe = bin.join(binary_name("origami"));
        let beside = bin.join(binary_name("instigator"));
        fs::write(&beside, "").unwrap();
        let runtime = Runtime::resolve(Some(selected.clone()), &exe).unwrap();
        assert_eq!(runtime.instigator().unwrap(), beside);
        let own = selected.join(binary_name("instigator"));
        fs::write(&own, "").unwrap();
        let runtime = Runtime::resolve(Some(selected), &exe).unwrap();
        assert_eq!(runtime.instigator().unwrap(), own);
    }

    #[test]
    fn local_graphics_uses_sdl_and_rad4() {
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let mut file = machine(offer);
        file.pipe = origami_machine::graphics::select(
            &machines,
            &offer.topology,
            offer.nodes,
            &origami_machine::graphics::Request {
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
        assert!(args.windows(2).any(|pair| pair
            == [
                "-device",
                "psitech-rad4,bus=pci,addr=5,product=rad4,id=pci-5"
            ]));
        assert!(args.windows(2).any(|pair| pair == ["-serial", "stdio"]));
    }

    #[test]
    fn the_o2_launch_counts_instructions_and_names_its_flash_image() {
        let machines = test_machines();
        let launch = |name: &str| {
            let offer = preset(&machines, name).unwrap();
            let mut file = machine(offer);
            file.machine.inputs =
                origami_machine::profiles::preset_inputs(name, Default::default());
            file.drive.push(Drive {
                name: "disk1".into(),
                kind: "disk".into(),
                bus: 0,
                target: 1,
                image: "disk.qcow2".into(),
                read_only: false,
                overlay: false,
            });
            let args = arguments(
                &machines,
                Path::new("/machine"),
                &file,
                offer,
                Display::None,
            )
            .unwrap();
            let option = args[args.iter().position(|a| a == "-M").unwrap() + 1].clone();
            (args, option)
        };
        let (args, option) = launch("o2");
        let has = |pair: [&str; 2]| args.windows(2).any(|w| w == pair);
        assert!(has(["-accel", "tcg,thread=single"]));
        assert!(has(["-icount", "shift=3,align=off,sleep=off"]));
        assert!(has(["-rtc", "clock=vm"]));
        // The flash store stands for the image, with the 512 KiB geometry.
        let flash = Path::new("/machine").join("state").join("node0-flash.raw");
        assert!(
            option.contains(&format!(
                ",rom-image={},rom-size=524288,rom-offset=0,rom-pad-byte=255",
                flash.display()
            )),
            "{option}"
        );
        // The preset's board inputs reach QEMU; the O2 has no population.
        assert!(option.contains(",password-clear-input=1"), "{option}");
        assert!(!option.contains("population"), "{option}");
        // The disk goes on the first onboard controller, which QEMU calls aic0.0.
        assert!(args
            .iter()
            .any(|a| a.starts_with("scsi-hd,bus=aic0.0,scsi-id=1,")));
        // Other machines keep QEMU's own clock and need no image.
        let (args, option) = launch("origin200-1");
        assert!(!args.iter().any(|a| a == "-icount" || a == "-rtc"));
        assert!(!option.contains("rom-image"), "{option}");
    }

    #[test]
    fn a_pipe_adds_the_machine_properties_it_needs() {
        let machines = test_machines();
        let offer = preset(&machines, "origin2000-8").unwrap();
        let machine_option = |file: &MachineFile| {
            let args =
                arguments(&machines, Path::new("/machine"), file, offer, Display::None).unwrap();
            args[args.iter().position(|a| a == "-M").unwrap() + 1].clone()
        };
        let mut file = machine(offer);
        assert!(!machine_option(&file).contains("baseio-preset"));
        file.pipe = origami_machine::graphics::select(
            &machines,
            &offer.topology,
            offer.nodes,
            &origami_machine::graphics::Request {
                product: Some("si"),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(machine_option(&file).contains(",baseio-preset=io6g"));
    }

    #[test]
    fn only_usb_input_machines_get_a_usb_keyboard_and_mouse() {
        let machines = test_machines();
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
        let machines = test_machines();
        let offering = preset(&machines, "fuel-1").unwrap();
        let mut file = machine(offering);
        file.engine = origami_machine::model::EngineChoice::qemu(&[
            "-global".into(),
            "bcm570x.mac=08:00:69:12:34:56".into(),
        ]);
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
        let machines = test_machines();
        let device_for = |name: &str, target: u32| {
            let offering = preset(&machines, name).unwrap();
            let mut file = machine(offering);
            file.drive.push(origami_machine::Drive {
                name: "system".into(),
                kind: "disk".into(),
                bus: 0,
                target,
                image: "system.qcow2".into(),
                read_only: false,
                overlay: false,
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
    fn a_chd_disk_is_opened_with_the_chd_driver() {
        let machines = test_machines();
        let offering = preset(&machines, "fuel-1").unwrap();
        let drive_option = |overlay: bool| {
            let mut file = machine(offering);
            file.drive.push(origami_machine::Drive {
                name: "disk1".into(),
                kind: "disk".into(),
                bus: 0,
                target: 1,
                image: "/images/irix.chd".into(),
                read_only: false,
                overlay,
            });
            let args = arguments(
                &machines,
                Path::new("/machine"),
                &file,
                offering,
                Display::None,
            )
            .unwrap();
            args[args.iter().position(|a| a == "-drive").unwrap() + 1].clone()
        };
        assert_eq!(
            drive_option(false),
            "if=none,id=drive0,file=/images/irix.chd,format=chd,readonly=off"
        );
        assert_eq!(
            drive_option(true),
            "if=none,id=drive0,file=/images/irix.chd,format=chd,readonly=off,overlay=on"
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
        if cfg!(windows) {
            assert_eq!(shell_quote(""), "\"\"");
            assert_eq!(shell_quote("my dir\"x"), "\"my dir\\\"x\"");
        } else {
            assert_eq!(shell_quote(""), "''");
            assert_eq!(shell_quote("/my dir/it's"), "'/my dir/it'\\''s'");
        }
    }

    #[test]
    fn console_line_follows_the_machine_list_and_choice() {
        let machines = test_machines();
        let serial = |args: &[String]| -> Vec<String> {
            args.windows(2)
                .filter(|p| p[0] == "-serial")
                .map(|p| p[1].clone())
                .collect()
        };
        // Fuel's machine list entry gives l1 (its console), then IOC3 ports A and B.
        let fuel = preset(&machines, "fuel-1").unwrap();
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
        origin2000.consoles.push(origami_machine::Console {
            kind: "second-port".into(),
            node: 1,
            name: Some("node1_ioc3_a".into()),
        });
        let origin2000 = &origin2000;
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
        let machines = test_machines();
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
        let machines = test_machines();
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
    fn the_shm_name_ignores_the_windows_verbatim_prefix() {
        assert_eq!(
            shm_name(Path::new(r"\\?\C:\Users\a\machines\one")),
            shm_name(Path::new(r"C:\Users\a\machines\one"))
        );
        assert_eq!(
            shm_name(Path::new(r"\\?\UNC\host\share\one")),
            shm_name(Path::new(r"\\host\share\one"))
        );
        assert_ne!(
            shm_name(Path::new(r"C:\one")),
            shm_name(Path::new(r"C:\two"))
        );
    }

    #[test]
    fn the_display_socket_ignores_the_verbatim_prefix_at_the_length_limit() {
        // 4 bytes shorter plain than verbatim: straddles the short-path limit.
        let plain = format!(
            r"C:\{}",
            "d".repeat(100 - r"C:\".len() - "/state/display.sock".len())
        );
        let verbatim = format!(r"\\?\{plain}");
        assert_eq!(
            display_socket(Path::new(&plain)),
            display_socket(Path::new(&verbatim))
        );
    }

    #[test]
    fn the_shm_display_names_the_segment_after_the_machine_directory() {
        let machines = test_machines();
        let offer = preset(&machines, "octane2-vpro").unwrap();
        let dir = Path::new("/machines/one");
        let shm = Display::Shm { gpu: None };
        let args = arguments(&machines, dir, &machine(offer), offer, shm).unwrap();
        let option = args[args.iter().position(|a| a == "-M").unwrap() + 1].clone();
        let name = shm_name(dir);
        assert!(
            option.contains(&format!(",display-shm={name},")),
            "{option}"
        );
        let socket = display_socket(dir).display().to_string();
        assert!(
            option.contains(&format!(",display-shm-socket={socket}")),
            "{option}"
        );
        assert!(args.windows(2).any(|pair| pair == ["-display", "none"]));
        assert!(name.starts_with("origami-") && name.len() == "origami-".len() + 12);
        assert_ne!(name, shm_name(Path::new("/machines/two")));
        assert!(!option.contains("display-shm-gpu"), "{option}");
        // Any other display leaves the property off.
        let args = arguments(&machines, dir, &machine(offer), offer, Display::None).unwrap();
        assert!(!args.iter().any(|a| a.contains("display-shm")));
    }

    #[test]
    fn a_version_4_display_is_a_device_on_the_runners_socket() {
        let machines = test_machines();
        let offer = preset(&machines, "octane2-vpro").unwrap();
        let dir = Path::new("/machines/one");
        let build = |display| arguments(&machines, dir, &machine(offer), offer, display).unwrap();
        let args = build(Display::ShmV4 { fd: Some(7) });
        assert!(args
            .windows(2)
            .any(|p| p == ["-device", "display-shm,fd=7"]));
        assert!(!args.iter().any(|a| a.contains("display-shm=")));
        assert!(args.windows(2).any(|p| p == ["-display", "none"]));
        let args = build(Display::ShmV4 { fd: None });
        let device = format!("display-shm,path={}", display_socket(dir).display());
        assert!(args.windows(2).any(|p| p == ["-device", device.as_str()]));
    }

    #[cfg(unix)]
    fn fake_qemu(dir: &Path, types: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("qemu");
        // Greets, then answers the two commands the probe sends, in order.
        fs::write(
            &path,
            format!(
                "#!/bin/sh\n\
                 echo '{{\"QMP\": {{\"version\": {{\"qemu\": {{\"micro\": 0, \"minor\": 0, \"major\": 0}}, \"package\": \"\"}}, \"capabilities\": []}}}}'\n\
                 read line; echo '{{\"return\": {{}}}}'\n\
                 read line; echo '{{\"return\": {types}}}'\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn the_display_device_type_selects_protocol_4() {
        let dir = tempfile::tempdir().unwrap();
        let v4 = fake_qemu(
            dir.path(),
            r#"[{"name": "e1000", "parent": "pci-device"}, {"name": "display-shm", "parent": "device"}]"#,
        );
        assert_eq!(shm_protocol(&v4), ShmProtocol::V4);
        let other = tempfile::tempdir().unwrap();
        let v3 = fake_qemu(
            other.path(),
            r#"[{"name": "e1000", "parent": "pci-device"}]"#,
        );
        assert_eq!(shm_protocol(&v3), ShmProtocol::V3);
        // A QEMU that cannot be run, or does not answer, is version 3.
        assert_eq!(shm_protocol(&dir.path().join("missing")), ShmProtocol::V3);
    }

    #[cfg(unix)]
    #[test]
    fn a_qemu_that_does_not_answer_is_version_3_for_now_and_asked_again() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qemu");
        fs::write(&path, "#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        assert_eq!(shm_protocol(&path), ShmProtocol::V3);
        assert!(started.elapsed() < Duration::from_secs(10));
        let answering = fake_qemu(dir.path(), r#"[{"name": "display-shm"}]"#);
        assert_eq!(shm_protocol(&answering), ShmProtocol::V4);
    }

    #[cfg(unix)]
    #[test]
    fn the_runners_display_socket_replaces_a_stale_one_and_goes_with_the_machine() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("display.sock");
        fs::write(&path, b"left by a killed runner").unwrap();
        let listener = DisplayListener::bind(path.clone()).unwrap();
        assert!(fs::metadata(&path).unwrap().file_type().is_socket());
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        std::os::unix::net::UnixStream::connect(&path).unwrap();
        drop(listener);
        assert!(!path.exists());
    }

    #[test]
    fn the_shm_display_names_the_viewers_gpu_when_it_has_one() {
        let machines = test_machines();
        let offer = preset(&machines, "octane2-vpro").unwrap();
        let uuid: GpuUuid = "00112233445566778899aabbccddeeff".parse().unwrap();
        assert_eq!(uuid.0[0], 0x00);
        assert_eq!(uuid.0[15], 0xff);
        assert_eq!(uuid.hex(), "00112233445566778899aabbccddeeff");
        let shm = Display::Shm { gpu: Some(uuid) };
        let args = arguments(
            &machines,
            Path::new("/machines/one"),
            &machine(offer),
            offer,
            shm,
        )
        .unwrap();
        let option = args[args.iter().position(|a| a == "-M").unwrap() + 1].clone();
        assert!(
            option.ends_with(",display-shm-gpu=00112233445566778899aabbccddeeff")
                || option.contains(",display-shm-gpu=00112233445566778899aabbccddeeff,"),
            "{option}"
        );
        for bad in [
            "",
            "00112233",
            "zz112233445566778899aabbccddeeff0",
            "00112233445566778899aabbccddeefg",
        ] {
            assert!(bad.parse::<GpuUuid>().is_err(), "{bad}");
        }
    }

    #[test]
    fn the_display_socket_is_in_the_machine_directory_unless_that_is_too_long() {
        let short = Path::new("/machines/one");
        assert_eq!(
            display_socket(short),
            Path::new("/machines/one/state/display.sock")
        );
        let long = PathBuf::from(format!("/machines/{}", "x".repeat(120)));
        let fallback = display_socket(&long);
        assert!(
            fallback.as_os_str().len() <= SOCKET_PATH_MAX,
            "{fallback:?}"
        );
        assert!(fallback.to_string_lossy().contains(&shm_name(&long)));
        assert_ne!(
            fallback,
            display_socket(&PathBuf::from(format!("/machines/{}", "y".repeat(120))))
        );
        // A path of exactly the limit still stays in the directory.
        let edge = PathBuf::from(format!(
            "/{}",
            "e".repeat(SOCKET_PATH_MAX - "/state/display.sock".len() - 1)
        ));
        assert_eq!(display_socket(&edge).as_os_str().len(), SOCKET_PATH_MAX);
        assert!(display_socket(&edge).starts_with(&edge));
    }

    #[test]
    fn vnc_serves_each_head_of_each_pipe_on_its_own_port() {
        let machines = test_machines();
        let offer = preset(&machines, "onyx3400-infinite-reality").unwrap();
        let mut file = machine(offer);
        file.pipe = origami_machine::graphics::select(
            &machines,
            &offer.topology,
            offer.nodes,
            &origami_machine::graphics::Request {
                pipes: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        let args = arguments(
            &machines,
            Path::new("/machine"),
            &file,
            offer,
            Display::Vnc { port: 5900 },
        )
        .unwrap();
        let displays: Vec<_> = args
            .windows(2)
            .filter(|pair| pair[0] == "-display")
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(
            displays,
            [
                "vnc=127.0.0.1:0,display=gbrick0-pipe0-pipe,head=0",
                "vnc=127.0.0.1:1,display=gbrick0-pipe0-pipe,head=1",
                "vnc=127.0.0.1:2,display=gbrick0-pipe1-pipe,head=0",
                "vnc=127.0.0.1:3,display=gbrick0-pipe1-pipe,head=1",
            ]
        );
        // The ids those displays name are on the pipes.
        for slot in ["gbrick0-pipe0-pipe", "gbrick0-pipe1-pipe"] {
            assert!(
                args.iter()
                    .any(|a| a.starts_with("sgi-kona,") && a.ends_with(&format!(",id={slot}"))),
                "{args:?}"
            );
        }
        let ports = vnc_ports(&machines, offer, &file, 5900).unwrap();
        let ports: Vec<_> = ports
            .iter()
            .map(|(h, p)| (h.slot.as_str(), h.head, *p))
            .collect();
        assert_eq!(
            ports,
            [
                ("gbrick0-pipe0-pipe", 0, 5900),
                ("gbrick0-pipe0-pipe", 1, 5901),
                ("gbrick0-pipe1-pipe", 0, 5902),
                ("gbrick0-pipe1-pipe", 1, 5903),
            ]
        );
        let error = vnc_ports(&machines, offer, &file, 65534).unwrap_err();
        assert!(error.to_string().contains("past the last port"), "{error}");

        // One pipe of one head keeps the single server and its plain device.
        let octane = preset(&machines, "octane-impact").unwrap();
        let mut file = machine(octane);
        file.pipe = origami_machine::graphics::select(
            &machines,
            &octane.topology,
            octane.nodes,
            &Default::default(),
        )
        .unwrap();
        let args = arguments(
            &machines,
            Path::new("/machine"),
            &file,
            octane,
            Display::Vnc { port: 5900 },
        )
        .unwrap();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-display", "vnc=127.0.0.1:0"]));
        assert!(args
            .iter()
            .filter(|a| a.starts_with("sgi-mgras,"))
            .all(|a| a.contains(",id=")));
    }

    #[test]
    fn user_network_forwards_bind_only_to_loopback() {
        let machines = test_machines();
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
        let expected = "user,id=net0,net=10.98.0.0/24,host=10.98.0.2,dhcpstart=10.98.0.65,hostname=machine,hostfwd=tcp:127.0.0.1:2222-:22,hostfwd=udp:127.0.0.1:5353-:53";
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
        file.identity = Some(origami_machine::Identity {
            mac: "08:00:69:12:34:56".into(),
            emu_id: None,
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
        let machines = test_machines();
        let nic = |name: &str, mode: &str| {
            let mut offer = preset(&machines, name).unwrap().clone();
            // A machine list naming no model: bind the first onboard NIC.
            if name == "origin2000-8" {
                let mut data = offering_data(&offer).unwrap();
                data.nics.iter_mut().for_each(|n| n.model = None);
                offer
                    .engine
                    .insert(crate::ENGINE.into(), serde_json::to_value(data).unwrap());
            }
            let offer = &offer;
            let mut file = machine(offer);
            file.identity = Some(origami_machine::Identity {
                mac: "08:00:69:12:34:56".into(),
                emu_id: None,
            });
            file.network = Network {
                mode: mode.into(),
                endpoint: Some("tcp:127.0.0.1:49173".into()),
                interface: None,
                forward: vec![],
                share_address: None,
                subnet: None,
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
            assert_eq!(nic("onyx350", mode), "nic,model=bcm570x,netdev=net0");
            assert_eq!(
                nic("origin200-1", mode),
                "nic,model=sgi-ioc3-eth,netdev=net0"
            );
            assert_eq!(
                nic("octane-impact", mode),
                "nic,model=sgi-ioc3-eth,netdev=net0"
            );
            assert_eq!(nic("origin2000-8", mode), "nic,netdev=net0");
        }
    }

    #[test]
    fn identity_mac_is_a_machine_property() {
        let machines = test_machines();
        let offer = preset(&machines, "origin2000-8").unwrap();
        let mut file = machine(offer);
        file.identity = Some(origami_machine::Identity {
            mac: "02:00:5d:aa:bb:cc".into(),
            emu_id: None,
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
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let mut file = machine(offer);
        file.network = Network {
            mode: "private".into(),
            endpoint: Some("install,one.sock".into()),
            interface: None,
            forward: vec![],
            share_address: None,
            subnet: None,
        };
        file.identity = Some(origami_machine::Identity {
            mac: "08:00:69:12:34:56".into(),
            emu_id: None,
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
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let mut file = machine(offer);
        file.network = Network {
            mode: "private".into(),
            endpoint: Some("tcp:127.0.0.1:49173".into()),
            interface: None,
            forward: vec![],
            share_address: None,
            subnet: None,
        };
        file.identity = Some(origami_machine::Identity {
            mac: "08:00:69:12:34:56".into(),
            emu_id: None,
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
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let file = machine(offer);
        let dir = Path::new("/machine");
        let args = |ports: Option<&origami_machine::shares::Ports>| {
            arguments_with(&machines, dir, &file, offer, Display::None, ports).unwrap()
        };
        let plain = args(None);
        let ports = origami_machine::shares::Ports {
            guest: origami_machine::shares::Listen::Tcp("40001".into()),
            network: origami_machine::shares::Listen::Unix("/run/user/1/a,b/net.sock".into()),
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
                "stream,id=share-net,addr.type=unix,addr.path=/run/user/1/a,,b/net.sock,server=off,reconnect-ms=1000",
                "hubport,id=share-hub-link,hubid=1,netdev=share-net",
                "stream,id=share-guest,addr.type=inet,addr.host=127.0.0.1,addr.port=40001,server=off,reconnect-ms=1000",
            ]
        );
        let nic = wired.windows(2).find(|p| p[0] == "-net").unwrap()[1].clone();
        assert_eq!(nic, "nic,model=sgi-ioc3-eth,netdev=share-guest");
        // Without shares the arguments are what they always were.
        assert!(!plain
            .iter()
            .any(|a| a.contains("share-hub") || a.contains("share-guest")));
    }

    #[test]
    fn host_networks_attach_the_ioc3_to_a_tap_or_socket_vmnet() {
        let machines = test_machines();
        let offer = preset(&machines, "origin200-1").unwrap();
        let mut file = machine(offer);
        file.identity = Some(origami_machine::Identity {
            mac: "08:00:69:12:34:56".into(),
            emu_id: None,
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
            subnet: None,
        };
        let expected = if cfg!(windows) {
            "tap,id=net0,ifname=TAP,,lab"
        } else {
            "tap,id=net0,ifname=TAP,,lab,script=,downscript="
        };
        assert_eq!(netdev(&file), expected);
        assert_eq!(
            origami_machine::validate_network(&file).is_ok(),
            !cfg!(target_os = "macos")
        );
        assert!(network_hint(&file).unwrap().contains("TAP,lab"));

        file.network = Network {
            mode: "vmnet".into(),
            endpoint: Some("/opt/homebrew/var/run/socket_vmnet".into()),
            interface: None,
            forward: vec![],
            share_address: None,
            subnet: None,
        };
        assert_eq!(
            netdev(&file),
            "stream,id=net0,server=off,addr.type=unix,\
             addr.path=/opt/homebrew/var/run/socket_vmnet,reconnect-ms=1000"
        );
        assert_eq!(
            origami_machine::validate_network(&file).is_ok(),
            cfg!(target_os = "macos")
        );
        assert!(network_hint(&file).unwrap().contains("socket_vmnet"));

        file.identity = None;
        for mode in ["tap", "vmnet"] {
            file.network.mode = mode.into();
            let error = origami_machine::validate_network(&file)
                .unwrap_err()
                .to_string();
            assert!(error.contains(mode), "{error}");
        }
        file.network.interface = None;
        file.network.mode = "tap".into();
        if !cfg!(target_os = "macos") {
            let error = origami_machine::validate_network(&file)
                .unwrap_err()
                .to_string();
            assert!(error.contains("--interface"), "{error}");
        }
    }
}
