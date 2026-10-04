use crate::{
    control, origin300, qemu_path_option, resolve, tcp_endpoint, Drive, MachineFile, Offering,
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
/// `state`. Each catalogue init input is the tool option of the same name.
pub fn init_arguments(
    offering: &Offering,
    boot_prom: &Path,
    io_prom: Option<&Path>,
    state: &Path,
) -> Result<Vec<OsString>> {
    let mut args = vec!["--machine".into(), offering.machine_options.clone().into()];
    for input in &offering.init_inputs {
        let image = match input.name.as_str() {
            "boot-prom" => Some(boot_prom),
            "io-prom" => io_prom,
            other => {
                return Err(format!("{} needs unsupported input {other}", offering.topology).into())
            }
        };
        match image {
            Some(image) => args.extend([format!("--{}", input.name).into(), image.into()]),
            None if input.required => {
                return Err(format!(
                    "{} needs its {} image; supply --{} FILE",
                    offering.topology, input.kind, input.name
                )
                .into())
            }
            None => (),
        }
    }
    if io_prom.is_some() && !offering.init_inputs.iter().any(|i| i.name == "io-prom") {
        return Err(format!("{} has no IO PROM", offering.topology).into());
    }
    args.push(state.into());
    Ok(args)
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

fn user_network(forward: &[PortForward]) -> String {
    let mut option =
        "user,id=net0,net=192.0.2.0/24,host=192.0.2.2,dhcpstart=192.0.2.15".to_string();
    for rule in forward {
        option.push_str(&format!(
            ",hostfwd={}:127.0.0.1:{}-:{}",
            rule.protocol, rule.host_port, rule.guest_port
        ));
    }
    option
}

pub fn arguments(
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
) -> Result<Vec<String>> {
    let mut machine = offering.machine_options.clone();
    if offering.product == "origin300" && file.identity.is_some() {
        machine.push_str(&origin300::machine_options(dir, file)?);
    }
    if !matches!(offering.product.as_str(), "octane" | "octane2") {
        let population = offering
            .cpus_per_node
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(":");
        machine.push_str(&format!(",population={population}"));
    } else {
        machine.push_str(&format!(",graphics-board={}", file.machine.graphics));
    }
    crate::profiles::validate_inputs(offering, &file.machine.inputs)?;
    let mut cpu = offering.cpu.clone();
    for (key, value) in &file.machine.inputs {
        if offering.product == "octane2" {
            cpu.push_str(&format!(",{key}={value}"));
        } else {
            machine.push_str(&format!(",{key}={value}"));
        }
    }
    if offering.needs_debug_leds_off {
        machine.push_str(",debug-leds=off");
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
    crate::profiles::add_graphics(&mut args, offering, &file.machine.graphics)?;
    for (index, drive) in file.drive.iter().enumerate() {
        add_drive(&mut args, dir, drive, index);
    }
    // Fuel reserves line 0 for L1; its guest IOC3 A is line 1.
    // sn-machine.c binds chardev-a with sgi_sn1_serial_line(node, 1).
    if offering.product == "fuel" {
        args.extend(["-serial".into(), "null".into()]);
    }
    args.extend(["-serial".into(), "stdio".into()]);
    if offering.product == "origin200" {
        args.extend([
            "-serial".into(),
            "none".into(),
            "-serial".into(),
            "null".into(),
        ]);
    }
    match file.network.mode.as_str() {
        "none" => args.extend(["-nic".into(), "none".into()]),
        "private" => {
            let endpoint = file
                .network
                .endpoint
                .as_deref()
                .ok_or("private network needs endpoint")?;
            let address = if let Some(address) = tcp_endpoint(endpoint)? {
                format!(
                    "stream,id=net0,server=off,addr.type=inet,addr.host={},addr.port={},reconnect-ms=1000",
                    address.ip(), address.port()
                )
            } else {
                format!(
                    "stream,id=net0,server=off,addr.type=unix,addr.path={}",
                    qemu_path_option(&resolve(dir, endpoint))
                )
            };
            args.extend(["-netdev".into(), address]);
            args.extend([
                "-net".into(),
                format!(
                    "nic,model=sgi-ioc3-eth,netdev=net0,macaddr={}",
                    file.network
                        .mac
                        .as_deref()
                        .ok_or("private network needs mac")?,
                ),
            ]);
        }
        "user" if offering.product == "origin300" && file.identity.is_some() => {
            let mac = &file.identity.as_ref().unwrap().mac;
            args.extend([
                "-netdev".into(),
                user_network(&file.network.forward),
                "-net".into(),
                format!("nic,model=sgi-ioc3-eth,netdev=net0,macaddr={mac}"),
            ]);
        }
        "user" if matches!(offering.product.as_str(), "octane" | "octane2") => args.extend([
            "-netdev".into(),
            user_network(&file.network.forward),
            "-net".into(),
            "nic,model=sgi-ioc3-eth,netdev=net0".into(),
        ]),
        "user" => args.extend(["-nic".into(), user_network(&file.network.forward)]),
        _ => return Err("network mode must be user, none, or private".into()),
    }
    Ok(args)
}

fn add_drive(args: &mut Vec<String>, dir: &Path, drive: &Drive, index: usize) {
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
    args.extend([
        "-device".into(),
        format!(
            "{device},bus=scsi.{},scsi-id={},lun=0,drive={id}",
            drive.bus, drive.target
        ),
    ]);
}

pub fn start_background(dir: &Path, display: Display) -> Result<()> {
    if control::is_locked(dir)? {
        return Err("machine is already running".into());
    }
    fs::create_dir_all(dir.join("logs"))?;
    let log_path = dir.join("logs/runner.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
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
                "machine failed to start ({status}); see {}",
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
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
) -> Result<ExitStatus> {
    run_inner(dir, file, offering, display, false)
}

pub fn serve(
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
) -> Result<ExitStatus> {
    run_inner(dir, file, offering, display, true)
}

fn run_inner(
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
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
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(dir.join("state/machine.lock"))?;
    lock.try_lock()
        .map_err(|error| format!("cannot lock machine: {error}"))?;
    let mut args = arguments(dir, file, offering, display)?;
    if file.network.mode == "private" {
        let endpoint = file
            .network
            .endpoint
            .as_deref()
            .ok_or("private network needs endpoint")?;
        if tcp_endpoint(endpoint)?.is_none() {
            let path = resolve(dir, endpoint);
            let metadata = fs::metadata(&path)
                .map_err(|error| format!("private network socket {}: {error}", path.display()))?;
            #[cfg(unix)]
            if !metadata.file_type().is_socket() {
                return Err(format!(
                    "private network endpoint is not a socket: {}",
                    path.display()
                )
                .into());
            }
            #[cfg(not(unix))]
            let _ = metadata;
        }
    }
    if offering.product == "origin300" && file.identity.is_some() {
        origin300::prepare_state(dir, file)?;
    }
    if !background {
        fs::create_dir_all(dir.join("logs"))?;
        log_primary_serial(
            &mut args,
            "stdio,id=serial0,logfile=logs/serial.log,logappend=on".into(),
        )?;
        let mut child = Command::new(qemu).args(args).current_dir(dir).spawn()?;
        return Ok(child.wait()?);
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
        format!("socket,id=serial0,host=127.0.0.1,port={console_port},server=on,wait=off,logfile=logs/serial.log,logappend=on"),
    )?;
    args.extend([
        "-qmp".into(),
        format!("tcp:127.0.0.1:{qmp_port},server=on,wait=off"),
        "-name".into(),
        name.clone(),
    ]);
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
            return Err(format!("QEMU exited before QMP was ready ({status})").into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.kill()?;
    child.wait()?;
    Err("QEMU did not open its QMP control endpoint".into())
}

fn log_primary_serial(args: &mut Vec<String>, chardev: String) -> Result<()> {
    let serial = args
        .windows(2)
        .position(|pair| pair == ["-serial", "stdio"])
        .ok_or("primary serial argument missing")?;
    args[serial + 1] = "chardev:serial0".into();
    args.extend(["-chardev".into(), chardev]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{preset, Machine, Network};

    fn machine(offering: &Offering, graphics: &str) -> MachineFile {
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
                graphics: graphics.into(),
            },
            identity: None,
            network: Network::default(),
            drive: vec![],
        }
    }

    #[test]
    fn local_graphics_uses_sdl_and_rad4() {
        let catalog = crate::test_catalogue();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let args = arguments(
            Path::new("/machine"),
            &machine(offer, "rad4"),
            offer,
            Display::Local,
        )
        .unwrap();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-display", "sdl,window-close=off"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-device", "psitech-rad4,addr=5"]));
        assert!(args.windows(2).any(|pair| pair == ["-serial", "stdio"]));
    }

    #[test]
    fn fuel_guest_console_uses_ioc3_a_in_foreground_and_background() {
        let catalog = crate::test_catalogue();
        let offer = preset(&catalog, "fuel-1").unwrap();
        let mut file = machine(offer, "none");
        file.machine.inputs = [
            ("fuel-board-id-word", "0x4000"),
            ("fuel-bedrock-revision", "0"),
            ("fuel-ioc3-subsystem-id", "0"),
            ("fuel-l1-type-code", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        let args = arguments(Path::new("/machine"), &file, offer, Display::None).unwrap();
        let serial: Vec<_> = args
            .windows(2)
            .filter(|p| p[0] == "-serial")
            .map(|p| p[1].as_str())
            .collect();
        assert_eq!(serial, ["null", "stdio"]);
        for chardev in ["stdio,id=serial0,logfile=logs/serial.log,logappend=on",
            "socket,id=serial0,host=127.0.0.1,port=12345,server=on,wait=off,logfile=logs/serial.log,logappend=on"] {
            let mut routed = args.clone();
            log_primary_serial(&mut routed, chardev.into()).unwrap();
            let serial: Vec<_> = routed.windows(2).filter(|p| p[0] == "-serial").map(|p| p[1].as_str()).collect();
            assert_eq!(serial, ["null", "chardev:serial0"]);
            assert!(routed.windows(2).any(|p| p == ["-chardev", chardev]));
        }
    }

    #[test]
    fn foreground_serial_keeps_console_and_logs_output() {
        let catalog = crate::test_catalogue();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let mut args = arguments(
            Path::new("/machine"),
            &machine(offer, "rad4"),
            offer,
            Display::None,
        )
        .unwrap();
        log_primary_serial(
            &mut args,
            "stdio,id=serial0,logfile=logs/serial.log,logappend=on".into(),
        )
        .unwrap();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-serial", "chardev:serial0"]));
        assert!(args.windows(2).any(|pair| pair
            == [
                "-chardev",
                "stdio,id=serial0,logfile=logs/serial.log,logappend=on"
            ]));
    }

    #[test]
    fn vnc_port_maps_to_qemu_display_offset() {
        let catalog = crate::test_catalogue();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let display = Display::Vnc { port: 5991 };
        let args = arguments(
            Path::new("/machine"),
            &machine(offer, "rad4"),
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
        let catalog = crate::test_catalogue();
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
        let expected = "user,id=net0,net=192.0.2.0/24,host=192.0.2.2,dhcpstart=192.0.2.15,hostfwd=tcp:127.0.0.1:2222-:22,hostfwd=udp:127.0.0.1:5353-:53";
        let origin200 = preset(&catalog, "origin200-1").unwrap();
        let mut file = machine(origin200, "rad4");
        file.network.forward = rules.clone();
        let args = arguments(Path::new("/machine"), &file, origin200, Display::None).unwrap();
        assert!(args.windows(2).any(|pair| pair == ["-nic", expected]));

        let origin300 = preset(&catalog, "origin300-2").unwrap();
        let mut file = machine(origin300, "none");
        file.network.forward = rules;
        file.identity = Some(crate::Origin300Identity {
            mac: "08:00:69:12:34:56".into(),
            spd_dimm2: "firmware/spd-dimm2.bin".into(),
            spd_dimm3: "firmware/spd-dimm3.bin".into(),
        });
        let args = arguments(Path::new("/machine"), &file, origin300, Display::None).unwrap();
        assert!(args.windows(2).any(|pair| pair == ["-netdev", expected]));
    }

    #[test]
    fn storage_attaches_as_named_block_nodes() {
        let catalog = crate::test_catalogue();
        let offer = preset(&catalog, "origin2000-8").unwrap();
        let args = arguments(
            Path::new("/machine,one"),
            &machine(offer, "none"),
            offer,
            Display::None,
        )
        .unwrap();
        let blockdevs: Vec<_> = args
            .windows(2)
            .filter(|pair| pair[0] == "-blockdev")
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(blockdevs.len(), offer.storage.len());
        assert_eq!(
            blockdevs[0],
            "driver=raw,node-name=node0-flash,file.driver=file,\
             file.filename=/machine,,one/state/node0-flash.raw"
        );
        assert!(blockdevs.contains(
            &"driver=raw,node-name=io0-flash,file.driver=file,\
              file.filename=/machine,,one/state/io0-flash.raw"
        ));
        assert!(!args.iter().any(|arg| arg == "-bios" || arg == "-drive"));
        assert!(args.windows(2).any(|pair| pair
            == [
                "-M",
                "origin2000,topology=origin2000-rack,nodes=4,population=2:2:2:2,\
                 node0-flash=node0-flash,node1-flash=node1-flash,node2-flash=node2-flash,\
                 node3-flash=node3-flash,io0-flash=io0-flash,nvram0=nvram0,\
                 nvram0-clock=nvram0-clock"
            ]));
    }

    #[test]
    fn init_arguments_follow_the_catalogue_inputs() {
        let catalog = crate::test_catalogue();
        let origin200 = preset(&catalog, "origin200-1").unwrap();
        let args = init_arguments(
            origin200,
            Path::new("boot,prom.img"),
            None,
            Path::new("/machine/state"),
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "--machine",
                "origin200,topology=origin200,nodes=1",
                "--boot-prom",
                "boot,prom.img",
                "/machine/state"
            ]
        );
        let error = init_arguments(
            origin200,
            Path::new("boot.img"),
            Some(Path::new("io.img")),
            Path::new("state"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("no IO PROM"), "{error}");
        let onyx2 = preset(&catalog, "onyx2-infinite-reality").unwrap();
        let error =
            init_arguments(onyx2, Path::new("boot.img"), None, Path::new("state")).unwrap_err();
        assert!(error.to_string().contains("--io-prom FILE"), "{error}");
        let args = init_arguments(
            onyx2,
            Path::new("boot.img"),
            Some(Path::new("io.img")),
            Path::new("state"),
        )
        .unwrap();
        assert_eq!(&args[4..], ["--io-prom", "io.img", "state"]);
    }

    #[test]
    fn origin300_identity_uses_record_and_spd_inputs() {
        let catalog = crate::test_catalogue();
        let offer = preset(&catalog, "origin300-2").unwrap();
        let dir = Path::new("/machine");
        let mut file = machine(offer, "none");
        file.identity = Some(crate::Origin300Identity {
            mac: "08:00:69:12:34:56".into(),
            spd_dimm2: "firmware/spd-dimm2.bin".into(),
            spd_dimm3: "firmware/spd-dimm3.bin".into(),
        });
        let args = arguments(dir, &file, offer, Display::None).unwrap();
        assert!(args.windows(2).any(|pair| pair[0] == "-M"
            && pair[1].starts_with("origin300,topology=origin300,nodes=1,chassis-eeprom.0=,")));
        for (slot, name) in [(3, "spd-dimm2.bin"), (5, "spd-dimm3.bin")] {
            let expected = format!(
                "spd-eeprom.{slot}={}",
                resolve(dir, &format!("firmware/{name}")).display()
            );
            assert!(args.iter().any(|arg| arg.contains(&expected)));
        }
        assert!(args
            .iter()
            .any(|arg| arg == "nic,model=sgi-ioc3-eth,netdev=net0,macaddr=08:00:69:12:34:56"));
        file.identity.as_mut().unwrap().spd_dimm2 = "firmware/dimm,2.bin".into();
        let escaped = arguments(Path::new("/machine,one"), &file, offer, Display::None).unwrap();
        assert!(escaped
            .iter()
            .any(|arg| arg.contains("chassis-eeprom.1=") && arg.contains("machine,,one")));
        assert!(escaped
            .iter()
            .any(|arg| arg.contains("spd-eeprom.3=") && arg.contains("dimm,,2.bin")));
        assert!(escaped
            .iter()
            .any(|arg| arg.contains("node-name=node0-flash,") && arg.contains("machine,,one")));
    }

    #[cfg(unix)]
    #[test]
    fn private_network_uses_qemu_stream_client() {
        let catalog = crate::test_catalogue();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let mut file = machine(offer, "rad4");
        file.network = Network {
            mode: "private".into(),
            endpoint: Some("install,one.sock".into()),
            mac: Some("08:00:69:12:34:56".into()),
            forward: vec![],
        };
        let args = arguments(Path::new("/machine"), &file, offer, Display::None).unwrap();
        assert!(args.windows(2).any(|pair| pair
            == [
                "-netdev",
                "stream,id=net0,server=off,addr.type=unix,addr.path=/machine/install,,one.sock"
            ]));
        assert!(args.windows(2).any(|pair| pair
            == [
                "-net",
                "nic,model=sgi-ioc3-eth,netdev=net0,macaddr=08:00:69:12:34:56"
            ]));
    }

    #[test]
    fn private_tcp_network_uses_loopback_stream_client() {
        let catalog = crate::test_catalogue();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let mut file = machine(offer, "rad4");
        file.network = Network {
            mode: "private".into(),
            endpoint: Some("tcp:127.0.0.1:49173".into()),
            mac: Some("08:00:69:12:34:56".into()),
            forward: vec![],
        };
        let args = arguments(Path::new("/machine"), &file, offer, Display::None).unwrap();
        assert!(args.windows(2).any(|pair| pair
            == [
                "-netdev",
                "stream,id=net0,server=off,addr.type=inet,addr.host=127.0.0.1,addr.port=49173,reconnect-ms=1000"
            ]));
    }
}
