use crate::{
    control, origin300, qemu_path_option, resolve, tcp_endpoint, Drive, MachineFile, Offering,
    PortForward, Result,
};
use fs2::FileExt;
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

impl Display {
    pub fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "local" => Self::Local,
            "vnc" => Self::Vnc { port: 5900 },
            "none" => Self::None,
            _ => return Err(format!("unknown display: {value}").into()),
        })
    }

    pub fn with_vnc_port(self, value: Option<&str>) -> Result<Self> {
        let Some(value) = value else {
            return Ok(self);
        };
        if !matches!(self, Self::Vnc { .. }) {
            return Err("--vnc-port requires --display vnc".into());
        }
        let port: u16 = value.parse().map_err(|_| "VNC port must be 5900..65535")?;
        if port < 5900 {
            return Err("VNC port must be 5900..65535".into());
        }
        Ok(Self::Vnc { port })
    }
}

pub fn qemu_path() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("SGI_RUNTIME_DIR") {
        return Ok(PathBuf::from(root).join(binary_name("qemu-system-mips64")));
    }
    let exe = std::env::current_exe()?;
    Ok(exe
        .parent()
        .ok_or("cannot locate origami executable directory")?
        .join("../libexec/sgi")
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
        .join("../../share/sgi/qemu"))
}

fn cpu_flash_paths(dir: &Path, _file: &MachineFile, offering: &Offering) -> Vec<PathBuf> {
    crate::firmware::cpu_paths(dir, offering)
}

fn initialization_path(dir: &Path) -> PathBuf {
    dir.join("state/firmware-initialization")
}

fn state_paths(dir: &Path, file: &MachineFile, offering: &Offering) -> Result<Vec<(PathBuf, u64)>> {
    let cpu = crate::firmware::Layout::find(&offering.topology, "cpu")?;
    let mut paths: Vec<_> = cpu_flash_paths(dir, file, offering)
        .into_iter()
        .map(|path| (path, cpu.size() as u64))
        .collect();
    let io_paths = crate::firmware::io_paths(dir, file, offering);
    if !io_paths.is_empty() {
        let io = crate::firmware::Layout::find(&offering.topology, "io")?;
        paths.extend(io_paths.into_iter().map(|path| (path, io.size() as u64)));
    }
    let nvram_count: u32 = offering
        .resources
        .iter()
        .filter(|r| r.kind == "nvram")
        .map(|r| r.count)
        .sum();
    let nvram_size = offering
        .resources
        .iter()
        .find(|r| r.kind == "nvram")
        .map_or(0, |r| r.size);
    let clock_size = match offering.resources.iter().find(|r| r.kind == "rtc-clock") {
        Some(resource) => resource.size,
        None if nvram_count == 0 => 0,
        None => {
            return Err(format!(
                "catalogue lacks clock backend geometry for {}",
                offering.topology
            )
            .into())
        }
    };
    if nvram_count > 0 && (nvram_size == 0 || clock_size == 0) {
        return Err(format!("invalid NVRAM or clock geometry for {}", offering.topology).into());
    }
    for node in 0..nvram_count {
        paths.push((dir.join(format!("state/nvram{node}.raw")), nvram_size));
        paths.push((dir.join(format!("state/nvram{node}.raw.clock")), clock_size));
    }
    Ok(paths)
}

pub fn validate_state(dir: &Path, file: &MachineFile, offering: &Offering) -> Result<()> {
    let marker = initialization_path(dir);
    match fs::read(&marker) {
        Ok(bytes) if bytes == [1] => (),
        Ok(bytes) if bytes == [0] => return Err(format!(
            "unfinished firmware initialization at {}; create a new machine from original firmware or import complete prepared flash backends into a new directory",
            marker.display()).into()),
        Ok(_) => return Err(format!("invalid firmware initialization record {}; restore a complete machine backup or create a new machine", marker.display()).into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    for (path, size) in state_paths(dir, file, offering)? {
        validate_store(&path, size)?;
    }
    Ok(())
}

fn validate_store(path: &Path, bytes: u64) -> Result<()> {
    let info = fs::symlink_metadata(path).map_err(|error| format!(
        "cannot open persistent state {}: {error}; restore this file from a complete machine backup or create/import a new machine", path.display()))?;
    if !info.file_type().is_file() || info.len() != bytes {
        return Err(format!("invalid persistent state {}: expected a regular file of {bytes} bytes; restore this file from a complete machine backup", path.display()).into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if info.nlink() != 1 {
            return Err(format!(
                "persistent state {} must not share a hard link",
                path.display()
            )
            .into());
        }
    }
    #[cfg(windows)]
    if file_link_count(path)? != 1 {
        return Err(format!(
            "persistent state {} must not share a hard link",
            path.display()
        )
        .into());
    }
    Ok(())
}

pub fn prepare_state(dir: &Path, file: &MachineFile, offering: &Offering) -> Result<()> {
    validate_state(dir, file, offering)?;
    // A completed legacy marker is obsolete only after every backend was checked.
    match fs::remove_file(initialization_path(dir)) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub fn create_state(
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    cpu_images: &[Vec<u8>],
    io_images: &[Vec<u8>],
) -> Result<()> {
    let cpu_paths = cpu_flash_paths(dir, file, offering);
    let io_paths = crate::firmware::io_paths(dir, file, offering);
    if cpu_paths.len() != cpu_images.len() || io_paths.len() != io_images.len() {
        return Err("prepared firmware count does not match machine topology".into());
    }
    for (path, image) in cpu_paths
        .iter()
        .zip(cpu_images)
        .chain(io_paths.iter().zip(io_images))
    {
        fs::create_dir_all(path.parent().ok_or("invalid flash path")?)?;
        let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
        output.write_all(image)?;
        output.sync_all()?;
    }
    for (path, size) in state_paths(dir, file, offering)? {
        if !cpu_paths.contains(&path) && !io_paths.contains(&path) {
            let output = OpenOptions::new().write(true).create_new(true).open(path)?;
            output.set_len(size)?;
            output.sync_all()?;
        }
    }
    validate_state(dir, file, offering)
}

#[cfg(windows)]
fn file_link_count(path: &Path) -> Result<u32> {
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    struct FileInformation {
        attributes: u32,
        created: FileTime,
        accessed: FileTime,
        modified: FileTime,
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            info: *mut FileInformation,
        ) -> i32;
    }
    let file = fs::File::open(path)?;
    let mut info = std::mem::MaybeUninit::<FileInformation>::uninit();
    // The Windows API writes the complete structure only on success.
    let success = unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) };
    if success == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { info.assume_init().links })
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
    crate::profiles::validate_console(offering, file.machine.console.as_deref())?;
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
    for (index, path) in cpu_flash_paths(dir, file, offering).iter().enumerate() {
        let readonly = if offering.firmware.kind == "ip30-prom" {
            ",readonly=on"
        } else {
            ""
        };
        args.extend([
            "-drive".into(),
            format!(
                "if=pflash,index={index},file={},format=raw{readonly}",
                qemu_path_option(path)
            ),
        ]);
    }
    let io_paths = crate::firmware::io_paths(dir, file, offering);
    let io_backend_ids = crate::firmware::io_backend_ids(offering)?;
    for (path, id) in io_paths.iter().zip(io_backend_ids) {
        args.extend([
            "-drive".into(),
            format!("if=none,id={id},file={},format=raw", qemu_path_option(path)),
        ]);
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
    // Fresh Fuel firmware uses L1 on line 0. console=d uses IOC3-A on line 1.
    if offering.product == "fuel" && file.machine.console.as_deref() == Some("ioc3-a") {
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
            let mac = &file
                .identity
                .as_ref()
                .ok_or("Origin 300 needs identity")?
                .mac;
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
    if control::is_running(dir)? || control::is_locked(dir)? {
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
    validate_state(dir, file, offering)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(dir.join("state/machine.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|error| format!("cannot lock machine: {error}"))?;
    prepare_state(dir, file, offering)?;
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
    if !background {
        fs::create_dir_all(dir.join("logs"))?;
        log_primary_serial(
            &mut args,
            "stdio,id=origami-console,logfile=logs/serial.log,logappend=on".into(),
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
        format!("socket,id=origami-console,host=127.0.0.1,port={console_port},server=on,wait=off,logfile=logs/serial.log,logappend=on"),
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
    args[serial + 1] = "chardev:origami-console".into();
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
                console: None,
            },
            firmware: Firmware {
                image: "firmware/prom.bin".into(),
                io_image: None,
            },
            identity: None,
            network: Network::default(),
            drive: vec![],
        }
    }

    fn state_fixture(name: &str, preset_name: &str, io: bool) -> (PathBuf, MachineFile, Offering) {
        let dir = std::env::temp_dir().join(format!(
            "origami-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join("state")).unwrap();
        let catalog = catalogue().unwrap();
        let offering = preset(&catalog, preset_name).unwrap().clone();
        let mut file = machine(&offering, "none");
        if io {
            file.firmware.io_image = Some("firmware/io.img".into());
        }
        let cpu_size = crate::firmware::Layout::find(&offering.topology, "cpu")
            .unwrap()
            .size();
        let cpus = vec![vec![0xa5; cpu_size]; offering.nodes as usize];
        let ios = if io {
            vec![vec![
                0x5a;
                crate::firmware::Layout::find(&offering.topology, "io")
                    .unwrap()
                    .size()
            ]]
        } else {
            vec![]
        };
        create_state(&dir, &file, &offering, &cpus, &ios).unwrap();
        (dir, file, offering)
    }

    #[test]
    fn completed_and_markerless_machines_preserve_all_state() {
        for legacy_marker in [false, true] {
            let (dir, file, offering) = state_fixture("reopen", "origin2000-8", true);
            let paths = state_paths(&dir, &file, &offering).unwrap();
            for (index, (path, _)) in paths.iter().enumerate() {
                let mut bytes = fs::read(path).unwrap();
                bytes[0] = index as u8 + 17;
                fs::write(path, bytes).unwrap();
            }
            let before: Vec<_> = paths
                .iter()
                .map(|(path, _)| fs::read(path).unwrap())
                .collect();
            if legacy_marker {
                fs::write(initialization_path(&dir), [1]).unwrap();
            }
            prepare_state(&dir, &file, &offering).unwrap();
            assert!(!initialization_path(&dir).exists());
            for ((path, _), bytes) in paths.iter().zip(before) {
                assert_eq!(fs::read(path).unwrap(), bytes);
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn missing_store_never_reconstructs_cpu_io_nvram_or_clock() {
        let (dir, file, offering) = state_fixture("missing", "origin2000-8", true);
        for (path, _) in state_paths(&dir, &file, &offering).unwrap() {
            let original = fs::read(&path).unwrap();
            fs::remove_file(&path).unwrap();
            let error = prepare_state(&dir, &file, &offering)
                .unwrap_err()
                .to_string();
            assert!(error.contains(&path.display().to_string()), "{error}");
            assert!(!path.exists());
            fs::write(&path, original).unwrap();
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_initialization_requires_explicit_recovery_and_preserves_bytes() {
        let (dir, file, offering) = state_fixture("pending", "origin2000-8", false);
        let flash = cpu_flash_paths(&dir, &file, &offering)[0].clone();
        let before = fs::read(&flash).unwrap();
        fs::write(initialization_path(&dir), [0]).unwrap();
        let error = prepare_state(&dir, &file, &offering)
            .unwrap_err()
            .to_string();
        assert!(error.contains("create a new machine") && error.contains("import"));
        assert_eq!(fs::read(&flash).unwrap(), before);
        assert_eq!(fs::read(initialization_path(&dir)).unwrap(), [0]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn node_flash_stores_are_independent_and_hardlinks_are_rejected() {
        let (dir, file, offering) = state_fixture("independent", "origin2000-8", false);
        let paths = cpu_flash_paths(&dir, &file, &offering);
        let mut bytes = fs::read(&paths[0]).unwrap();
        bytes[0] = 42;
        fs::write(&paths[0], bytes).unwrap();
        assert_eq!(fs::read(&paths[1]).unwrap()[0], 0xa5);
        fs::remove_file(&paths[1]).unwrap();
        fs::hard_link(&paths[0], &paths[1]).unwrap();
        assert!(prepare_state(&dir, &file, &offering)
            .unwrap_err()
            .to_string()
            .contains("hard link"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn io_prom_uses_a_persistent_backend_with_path_escaping() {
        let catalog = catalogue().unwrap();
        for preset_name in ["origin200-impact", "origin2000-8", "onyx2-infinite-reality"] {
            let offering = preset(&catalog, preset_name).unwrap();
            let mut file = machine(offering, crate::profiles::default_graphics(offering));
            file.firmware.io_image = Some("firmware/io,prom.img".into());
            let args =
                arguments(Path::new("/machine,one"), &file, offering, Display::None).unwrap();
            assert!(
                args.iter().any(|arg| arg.contains("if=none,id=sgi-")
                    && arg.contains("-flash0,file=/machine,,one/state/io-proms/io0.bin")),
                "{args:?}"
            );
            assert!(!args.iter().any(|arg| arg.contains("io-prom=")));
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
    fn fuel_guest_console_defaults_to_l1_in_foreground_and_background() {
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
        for (selection, expected) in [
            (None, vec!["stdio"]),
            (Some("l1"), vec!["stdio"]),
            (Some("ioc3-a"), vec!["null", "stdio"]),
        ] {
            file.machine.console = selection.map(str::to_owned);
            let args = arguments(Path::new("/machine"), &file, offer, Display::None).unwrap();
            let serial: Vec<_> = args
                .windows(2)
                .filter(|p| p[0] == "-serial")
                .map(|p| p[1].as_str())
                .collect();
            assert_eq!(serial, expected);
            for chardev in ["stdio,id=origami-console,logfile=logs/serial.log,logappend=on",
                "socket,id=origami-console,host=127.0.0.1,port=12345,server=on,wait=off,logfile=logs/serial.log,logappend=on"] {
                let mut routed = args.clone();
                log_primary_serial(&mut routed, chardev.into()).unwrap();
                let serial: Vec<_> = routed.windows(2).filter(|p| p[0] == "-serial").map(|p| p[1].as_str()).collect();
                let managed: Vec<_> = expected.iter().map(|port| if *port == "stdio" { "chardev:origami-console" } else { port }).collect();
                assert_eq!(serial, managed);
                assert!(routed.windows(2).any(|p| p == ["-chardev", chardev]));
                assert!(!routed.windows(2).any(|p| p[0] == "-chardev" && p[1].split(',').any(|field| field == "id=serial0")));
            }
        }
        file.machine.console = Some("unknown".into());
        assert!(arguments(Path::new("/machine"), &file, offer, Display::None).is_err());
        let other = preset(&catalog, "origin300-2").unwrap();
        file.machine.console = Some("l1".into());
        assert!(arguments(Path::new("/machine"), &file, other, Display::None).is_err());
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
        let catalog = catalogue().unwrap();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let display = Display::parse("vnc")
            .unwrap()
            .with_vnc_port(Some("5991"))
            .unwrap();
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
        assert_eq!(Display::parse("vnc").unwrap(), Display::Vnc { port: 5900 });
        assert!(Display::parse("local")
            .unwrap()
            .with_vnc_port(Some("5991"))
            .is_err());
        assert!(Display::parse("vnc")
            .unwrap()
            .with_vnc_port(Some("5899"))
            .is_err());
        assert!(Display::parse("vnc")
            .unwrap()
            .with_vnc_port(Some("abc"))
            .is_err());
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
            chassis_eeprom: None,
            board_eeprom: None,
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
        assert!(!args
            .iter()
            .any(|arg| arg == "-bios" || arg == "-S" || arg.contains("firmware-initialize")));
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
            chassis_eeprom: None,
            board_eeprom: None,
        });
        let args = arguments(dir, &file, offer, Display::None).unwrap();
        let flash = format!(
            "if=pflash,index=0,file={},format=raw",
            dir.join("state/ip35-boot-flash.raw").display()
        );
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-drive", flash.as_str()]));
        for (slot, name) in [(2, "spd-dimm2.bin"), (3, "spd-dimm3.bin")] {
            let expected = format!(
                "spd-dimm{slot}={}",
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
            .any(|arg| arg.contains("io8-mac=08:00:69:12:34:56")));
        assert!(escaped
            .iter()
            .any(|arg| arg.contains("spd-dimm2=") && arg.contains("dimm,,2.bin")));
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
