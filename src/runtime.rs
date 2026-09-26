use crate::{resolve, tcp_endpoint, Drive, MachineFile, Offering, Result};
use fs2::FileExt;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Local,
    Vnc,
    None,
}

impl Display {
    pub fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "local" => Self::Local,
            "vnc" => Self::Vnc,
            "none" => Self::None,
            _ => return Err(format!("unknown display: {value}").into()),
        })
    }
}

pub fn qemu_path() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("SGI_RUNTIME_DIR") {
        return Ok(PathBuf::from(root).join(binary_name("qemu-system-mips64")));
    }
    let exe = std::env::current_exe()?;
    Ok(exe
        .parent()
        .ok_or("cannot locate sgi executable directory")?
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

pub fn prepare_state(dir: &Path, offering: &Offering, prom: &Path) -> Result<()> {
    fs::create_dir_all(dir.join("state"))?;
    let nvram_count: u32 = offering
        .resources
        .iter()
        .filter(|resource| resource.kind == "nvram")
        .map(|resource| resource.count)
        .sum();
    for node in 0..nvram_count {
        ensure_size(&dir.join(format!("state/nvram{node}.raw")), 32768)?;
        ensure_size(&dir.join(format!("state/nvram{node}.raw.clock")), 16)?;
    }
    if offering.product == "origin2000" {
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

pub fn arguments(
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
) -> Result<Vec<String>> {
    if offering.product == "origin300" {
        return Err("Origin 300 launch needs its managed flash and identity inputs; this build cannot run it".into());
    }
    let mut machine = offering.product.clone();
    if offering.topology != offering.product && offering.topology != "origin2000-module" {
        machine.push_str(&format!(",topology={}", offering.topology));
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
        offering.cpu.clone(),
        "-smp".into(),
        offering.smp.to_string(),
        "-m".into(),
        (memory * offering.nodes).to_string(),
    ];
    if offering.product == "origin2000" {
        for node in 0..offering.nodes {
            args.extend([
                "-drive".into(),
                format!(
                    "if=pflash,index={node},file={},format=raw",
                    dir.join(format!("state/node-proms/node{}.bin", node + 1))
                        .display(),
                ),
            ]);
        }
    } else {
        args.extend([
            "-bios".into(),
            resolve(dir, &file.firmware.image).display().to_string(),
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
                path.display()
            ),
        ]);
        args.extend([
            "-drive".into(),
            format!(
                "if=none,id=sgi-nvram{node}-clock,file={},format=raw",
                dir.join(format!("state/nvram{node}.raw.clock")).display()
            ),
        ]);
    }
    match display {
        Display::Local => args.extend(["-display".into(), "sdl,window-close=off".into()]),
        Display::Vnc => args.extend(["-display".into(), "vnc=127.0.0.1:0".into()]),
        Display::None => args.extend(["-display".into(), "none".into()]),
    }
    args.extend(["-audio".into(), "none".into()]);
    if file.machine.graphics == "rad4" {
        args.extend(["-device".into(), "psitech-rad4,addr=5".into()]);
    }
    for (index, drive) in file.drive.iter().enumerate() {
        add_drive(&mut args, dir, drive, index);
    }
    // The first serial line is IOC3 A. Origin 200 has a system controller on line 2.
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
                    resolve(dir, endpoint).display()
                )
            };
            args.extend(["-netdev".into(), address]);
            args.extend([
                "-net".into(),
                format!(
                    "nic,netdev=net0,macaddr={}",
                    file.network
                        .mac
                        .as_deref()
                        .ok_or("private network needs mac")?,
                ),
            ]);
        }
        "user" => args.extend([
            "-nic".into(),
            "user,id=net0,net=192.0.2.0/24,host=192.0.2.2,dhcpstart=192.0.2.15".into(),
        ]),
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
            path.display(),
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

pub fn run(
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    display: Display,
) -> Result<ExitStatus> {
    let qemu = qemu_path()?;
    if !qemu.is_file() {
        return Err(format!("packaged QEMU missing: {}", qemu.display()).into());
    }
    fs::create_dir_all(dir.join("state"))?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(dir.join("state/machine.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|error| format!("cannot lock machine: {error}"))?;
    let args = arguments(dir, file, offering, display)?;
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
    prepare_state(dir, offering, &resolve(dir, &file.firmware.image))?;
    let mut child = Command::new(qemu).args(args).current_dir(dir).spawn()?;
    Ok(child.wait()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalogue, preset, Firmware, Machine, Network};

    fn machine(offering: &Offering, graphics: &str) -> MachineFile {
        MachineFile {
            format: 1,
            machine: Machine {
                model: offering.product.clone(),
                nodes: offering.nodes,
                cpus_per_node: offering.cpus_per_node[0],
                memory_per_node: format!("{}MiB", offering.memory.default),
                graphics: graphics.into(),
            },
            firmware: Firmware {
                image: "firmware/prom.bin".into(),
            },
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
        assert!(args.windows(2).any(|pair| pair == ["-M", "origin2000"]));
    }

    #[test]
    fn origin300_cannot_claim_a_runnable_command() {
        let catalog = catalogue().unwrap();
        let offer = preset(&catalog, "origin300-2").unwrap();
        assert!(arguments(
            Path::new("/machine"),
            &machine(offer, "none"),
            offer,
            Display::None
        )
        .is_err());
    }

    #[test]
    fn private_network_uses_qemu_stream_client() {
        let catalog = catalogue().unwrap();
        let offer = preset(&catalog, "origin200-1").unwrap();
        let mut file = machine(offer, "rad4");
        file.network = Network {
            mode: "private".into(),
            endpoint: Some("install.sock".into()),
            mac: Some("08:00:69:12:34:56".into()),
        };
        let args = arguments(Path::new("/machine"), &file, offer, Display::None).unwrap();
        assert!(args.windows(2).any(|pair| pair
            == [
                "-netdev",
                "stream,id=net0,server=off,addr.type=unix,addr.path=/machine/install.sock"
            ]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-net", "nic,netdev=net0,macaddr=08:00:69:12:34:56"]));
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
        };
        let args = arguments(Path::new("/machine"), &file, offer, Display::None).unwrap();
        assert!(args.windows(2).any(|pair| pair
            == [
                "-netdev",
                "stream,id=net0,server=off,addr.type=inet,addr.host=127.0.0.1,addr.port=49173,reconnect-ms=1000"
            ]));
    }
}
