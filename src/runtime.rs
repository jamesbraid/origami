use crate::{
    control, origin300, qemu_path_option, resolve, tcp_endpoint, Drive, MachineFile, Offering,
    PortForward, Result,
};
use std::fs::{self, OpenOptions};
use std::io::Write;
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

pub fn prepare_state(
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    prom: &Path,
) -> Result<()> {
    fs::create_dir_all(dir.join("state"))?;
    let nvram_count: u32 = offering
        .resources
        .iter()
        .filter(|resource| resource.kind == "nvram")
        .map(|resource| resource.count)
        .sum();
    let nvram_size = offering
        .resources
        .iter()
        .find(|r| r.kind == "nvram")
        .map_or(32768, |r| r.size);
    let clock_size = offering
        .resources
        .iter()
        .find(|r| r.kind == "rtc-clock")
        .map_or(16, |r| r.size);
    for node in 0..nvram_count {
        ensure_size(&dir.join(format!("state/nvram{node}.raw")), nvram_size)?;
        ensure_size(
            &dir.join(format!("state/nvram{node}.raw.clock")),
            clock_size,
        )?;
    }
    if offering.product == "origin300" && file.identity.is_some() {
        origin300::prepare_state(dir, file, prom)?;
    }
    if matches!(offering.product.as_str(), "origin2000" | "onyx2") {
        let firmware = fs::read(prom)?;
        if firmware.is_empty() || firmware.len() > 1048576 {
            return Err("Origin 2000 PROM must fit one MiB".into());
        }
        let flash_dir = dir.join("state/node-proms");
        fs::create_dir_all(&flash_dir)?;
        for node in 1..=offering.nodes {
            let store = flash_dir.join(format!("node{node}.bin"));
            if !store.exists() {
                let mut image = vec![0xff; 1048576];
                image[..firmware.len()].copy_from_slice(&firmware);
                // A newly erased pair needs a formatted PROM log before stock firmware can assign module IDs.
                let log = 14 * 65536 + 0x10;
                if image[14 * 65536..].iter().all(|byte| *byte == 0xff) {
                    image[log..log + 12]
                        .copy_from_slice(&[0x50, 0x4c, 0x4f, 0x47, 0, 0, 0, 1, 0, 0, 0, 1]);
                }
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&store)?
                    .write_all(&image)?;
            } else if fs::metadata(&store)?.len() != 1048576 {
                return Err(format!("invalid node PROM size: {}", store.display()).into());
            }
        }
    }
    Ok(())
}

fn ensure_size(path: &Path, bytes: u64) -> Result<()> {
    if path.exists() {
        if fs::metadata(path)?.len() != bytes {
            return Err(format!("invalid state file size: {}", path.display()).into());
        }
    } else {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?
            .set_len(bytes)?;
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
    let mut machine = if offering.product == "origin300" && file.identity.is_some() {
        origin300::machine_options(dir, file)?
    } else {
        offering.product.clone()
    };
    if offering.topology != offering.product {
        machine.push_str(&format!(",topology={}", offering.topology));
    }
    if !matches!(offering.product.as_str(), "octane" | "octane2") {
        let population = offering
            .cpus_per_node
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(":");
        machine.push_str(&format!(
            ",nodes={},population={population}",
            offering.nodes
        ));
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
    if matches!(offering.product.as_str(), "origin2000" | "onyx2") {
        for node in 0..offering.nodes {
            args.extend([
                "-drive".into(),
                format!(
                    "if=pflash,index={node},file={},format=raw",
                    qemu_path_option(&dir.join(format!("state/node-proms/node{}.bin", node + 1))),
                ),
            ]);
        }
    } else {
        args.extend([
            "-bios".into(),
            resolve(dir, &file.firmware.image).display().to_string(),
        ]);
        if offering.product == "origin300" && file.identity.is_some() {
            args.extend([
                "-drive".into(),
                format!(
                    "if=pflash,index=0,file={},format=raw",
                    qemu_path_option(&origin300::flash_path(dir))
                ),
            ]);
        }
    }
    let nvram_count: u32 = offering
        .resources
        .iter()
        .filter(|resource| resource.kind == "nvram")
        .map(|resource| resource.count)
        .sum();
    for node in 0..nvram_count {
        let path = dir.join(format!("state/nvram{node}.raw"));
        args.extend([
            "-drive".into(),
            format!(
                "if=none,id=sgi-nvram{node},file={},format=raw",
                qemu_path_option(&path)
            ),
        ]);
        args.extend([
            "-drive".into(),
            format!(
                "if=none,id=sgi-nvram{node}-clock,file={},format=raw",
                qemu_path_option(&dir.join(format!("state/nvram{node}.raw.clock")))
            ),
        ]);
    }
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
    let _lock = control::lock_for_edit(dir, "starting it again")?;
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
    prepare_state(dir, file, offering, &resolve(dir, &file.firmware.image))?;
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
    use crate::{catalogue, preset, Firmware, Machine, Network};

    fn machine(offering: &Offering, graphics: &str) -> MachineFile {
        MachineFile {
            format: 1,
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
            firmware: Firmware {
                image: "firmware/prom.bin".into(),
            },
            identity: None,
            network: Network::default(),
            drive: vec![],
        }
    }

    #[test]
    fn local_graphics_uses_sdl_and_rad4() {
        let catalog = catalogue().unwrap();
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
        let catalog = catalogue().unwrap();
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
        let catalog = catalogue().unwrap();
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
        let catalog = catalogue().unwrap();
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
        let catalog = catalogue().unwrap();
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
    fn origin2000_uses_independent_node_flash_images() {
        let catalog = catalogue().unwrap();
        let offer = preset(&catalog, "origin2000-8").unwrap();
        let args = arguments(
            Path::new("/machine"),
            &machine(offer, "none"),
            offer,
            Display::None,
        )
        .unwrap();
        let flashes = args
            .windows(2)
            .filter(|pair| pair[0] == "-drive" && pair[1].starts_with("if=pflash,"))
            .count();
        assert_eq!(flashes, 4);
        assert!(!args.iter().any(|arg| arg == "-bios"));
        assert!(args.windows(2).any(|pair| pair
            == [
                "-M",
                "origin2000,topology=origin2000-rack,nodes=4,population=2:2:2:2"
            ]));
    }

    #[test]
    fn origin300_uses_persistent_flash_and_spd_inputs() {
        let catalog = catalogue().unwrap();
        let offer = preset(&catalog, "origin300-2").unwrap();
        let dir = Path::new("/machine");
        let mut file = machine(offer, "none");
        file.identity = Some(crate::Origin300Identity {
            mac: "08:00:69:12:34:56".into(),
            spd_dimm2: "firmware/spd-dimm2.bin".into(),
            spd_dimm3: "firmware/spd-dimm3.bin".into(),
        });
        let args = arguments(dir, &file, offer, Display::None).unwrap();
        let flash = format!(
            "if=pflash,index=0,file={},format=raw",
            dir.join("state/ip35-boot-flash.raw").display()
        );
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-drive", flash.as_str()]));
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
            .any(|arg| arg.contains("if=pflash,index=0,file=") && arg.contains("machine,,one")));
    }

    #[cfg(unix)]
    #[test]
    fn private_network_uses_qemu_stream_client() {
        let catalog = catalogue().unwrap();
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
        let catalog = catalogue().unwrap();
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
