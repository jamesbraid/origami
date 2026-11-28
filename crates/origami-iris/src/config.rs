//! A machine as an iris configuration.

use crate::catalogue::{self, Data};
use origami_machine::dir::MachineDir;
use origami_machine::{MachineFile, Offering, Result};
use std::fmt::Write;
use std::path::Path;

/// The loopback ports iris serves on. Two iris processes at once need
/// different ones, and iris's defaults are shared.
#[derive(Clone, Copy, Debug)]
pub struct Ports {
    /// The console line: serial channel B, the PROM's `tty1`.
    pub console: u16,
    /// The other serial channel.
    pub second: u16,
    pub monitor: u16,
}

impl Ports {
    pub fn free() -> Result<Self> {
        let mut ports = [0u16; 3];
        for i in 0..ports.len() {
            loop {
                let port = origami_machine::control::free_port()?;
                if !ports[..i].contains(&port) {
                    ports[i] = port;
                    break;
                }
            }
        }
        Ok(Self {
            console: ports[0],
            second: ports[1],
            monitor: ports[2],
        })
    }
}

fn quote(path: &Path) -> String {
    quote_str(&path.display().to_string())
}

fn quote_str(text: &str) -> String {
    toml::Value::String(text.into()).to_string()
}

/// Refuse what iris cannot run, in the model's words.
pub fn check(file: &MachineFile) -> Result<()> {
    if !matches!(file.network.mode.as_str(), "user" | "private") {
        return Err(format!(
            "iris machines take user or private networking only, not {}",
            file.network.mode
        )
        .into());
    }
    if file.network.mode == "private" && file.network.endpoint.is_none() {
        return Err("private network needs endpoint".into());
    }
    if !file.network.forward.is_empty() {
        return Err("iris machines do not take port forwards yet".into());
    }
    if !file.share.is_empty() {
        return Err("iris machines do not take host shares yet".into());
    }
    for drive in &file.drive {
        if drive.kind == "tape" {
            return Err(format!("iris machines have no tape drive ({})", drive.name).into());
        }
        if drive.bus != 0 {
            return Err(format!("iris machines have one SCSI bus ({})", drive.name).into());
        }
    }
    if file.machine.console.as_deref().is_some_and(|c| c != "tty1") {
        return Err("the PROM console of an iris machine is tty1".into());
    }
    Ok(())
}

/// The PROM console a new machine is seeded with: the graphics head, since every
/// iris offering has one, unless its first-boot settings name the serial line.
pub fn prom_console(file: &MachineFile) -> u8 {
    match file.firstboot.as_ref().and_then(|f| f.nvram.get("console")) {
        Some(c) if c == "d" => b'd',
        _ => b'g',
    }
}

/// The iris configuration of `file`, which `offering` describes, in `dir`.
pub fn iris_toml(
    offering: &Offering,
    file: &MachineFile,
    dir: &MachineDir,
    ports: Ports,
) -> Result<String> {
    check(file)?;
    let Data {
        profile,
        cpu,
        bank_sizes,
        banks,
        console,
        board,
    } = catalogue::data(offering)?;
    let memory = file
        .machine
        .memory_per_node
        .strip_suffix("MiB")
        .ok_or("memory_per_node must use MiB")?
        .parse::<u32>()?;
    let mut filled = catalogue::banks_for(memory, &bank_sizes, banks)
        .ok_or_else(|| format!("{memory} MiB cannot be made from {bank_sizes:?} MiB banks"))?;
    filled.resize(4, 0);
    let mut t = String::new();
    // iris's own PROM runs unless a file is named; the product names none.
    writeln!(t, "prom = \"(embedded)\"")?;
    // iris saves an NVRAM file even where the machine has none (the
    // Indigo2), by default into the working directory.
    if !offering.storage.iter().any(|item| item.name != "nveeprom") {
        let spare = dir.path().join("state/iris-nvram.raw");
        writeln!(t, "nvram = {}", quote(&spare))?;
    }
    for item in &offering.storage {
        // The Indigo2 keeps its settings in an EEPROM; the others in NVRAM.
        let key = if item.name == "nveeprom" {
            "nveeprom"
        } else {
            "nvram"
        };
        let path = origami_machine::stores::state_path(dir.path(), item);
        writeln!(t, "{key} = {}", quote(&path))?;
    }
    writeln!(t, "banks = {filled:?}")?;
    writeln!(t, "no_audio = true")?;
    writeln!(t, "monitor_port = {}", ports.monitor)?;
    let (a, b) = if console == "a" {
        (ports.console, ports.second)
    } else {
        (ports.second, ports.console)
    };
    writeln!(t, "serial_port_a = {a}")?;
    writeln!(t, "serial_port_b = {b}")?;
    writeln!(t, "\n[machine]\nprofile = \"{profile}\"\ncpu = \"{cpu}\"")?;
    if let Some(board) = board {
        // The VPro's raster runs on the CPU threads: no host GPU needed.
        writeln!(t, "\n[graphics]\nboard = \"{board}\"\nraster = \"cpu\"")?;
    }
    // A private network is the install server's, which iris joins as a client.
    match (file.network.mode.as_str(), file.network.endpoint.as_deref()) {
        ("private", Some(endpoint)) => {
            let peer = if endpoint.starts_with("tcp:") {
                endpoint.to_owned()
            } else {
                format!("unix:{}", dir.resolve(endpoint).display())
            };
            writeln!(
                t,
                "\n[network]\nmode = \"stream\"\nstream = {}",
                quote_str(&peer)
            )?;
        }
        _ => writeln!(t, "\n[network]\nmode = \"nat\"")?,
    }
    let mac = file
        .identity
        .as_ref()
        .map_or(crate::nvram::DEFAULT_MAC, |i| &i.mac);
    writeln!(t, "mac = \"{mac}\"")?;
    // A `scsi` table that is present replaces iris's default drives.
    writeln!(t, "\n[scsi]")?;
    for drive in &file.drive {
        let image = dir.resolve(&drive.image);
        writeln!(t, "\n[scsi.{}]\npath = {}", drive.target, quote(&image))?;
        if drive.kind == "cdrom" {
            writeln!(t, "cdrom = true")?;
        }
        if drive.overlay {
            writeln!(t, "overlay = true")?;
        }
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use origami_machine::model::Machine;

    fn indy() -> (Offering, MachineFile) {
        let catalogue = catalogue::catalogue();
        let offering = catalogue.offerings[0].clone();
        let mut machine = Machine::from_offering(&offering, None, None, vec![], Default::default());
        machine.engine.id = crate::ENGINE.into();
        (offering, machine.to_file())
    }

    #[test]
    fn an_indy_becomes_a_profile_with_banks_and_ports() {
        let (offering, file) = indy();
        let ports = Ports {
            console: 1001,
            second: 1002,
            monitor: 1003,
        };
        let text = iris_toml(&offering, &file, &MachineDir::new("/m"), ports).unwrap();
        assert!(text.contains("banks = [128, 0, 0, 0]"), "{text}");
        assert!(text.contains("serial_port_b = 1001"));
        assert!(text.contains("profile = \"indy_ip24\""));
        assert!(
            text.contains("\n[scsi]\n"),
            "no drives must not mean iris's default ones"
        );
        let nvram = quote(&Path::new("/m").join("state").join("nvram.raw"));
        assert!(text.contains(&format!("nvram = {nvram}")));
        text.parse::<toml::Table>().unwrap();
    }

    #[test]
    fn the_console_is_graphics_unless_first_boot_says_serial() {
        let (_, mut file) = indy();
        assert_eq!(prom_console(&file), b'g');
        let mut firstboot = origami_machine::model::FirstBoot::default();
        firstboot.nvram.insert("console".into(), "d".into());
        file.firstboot = Some(firstboot);
        assert_eq!(prom_console(&file), b'd');
    }

    #[test]
    fn what_iris_cannot_run_is_refused() {
        let (offering, mut file) = indy();
        file.network.mode = "tap".into();
        let ports = Ports {
            console: 1,
            second: 2,
            monitor: 3,
        };
        let error = iris_toml(&offering, &file, &MachineDir::new("/m"), ports).unwrap_err();
        assert!(
            error.to_string().contains("user or private networking"),
            "{error}"
        );
    }

    #[test]
    fn a_private_network_is_a_stream_to_the_install_server() {
        let (offering, mut file) = indy();
        let ports = Ports {
            console: 1,
            second: 2,
            monitor: 3,
        };
        let toml = |file: &MachineFile| iris_toml(&offering, file, &MachineDir::new("/m"), ports);
        file.network.mode = "private".into();
        assert!(toml(&file)
            .unwrap_err()
            .to_string()
            .contains("needs endpoint"));
        file.network.endpoint = Some("tcp:127.0.0.1:4000".into());
        let text = toml(&file).unwrap();
        assert!(
            text.contains("mode = \"stream\"\nstream = \"tcp:127.0.0.1:4000\""),
            "{text}"
        );
        file.network.endpoint = Some("install.sock".into());
        assert!(toml(&file)
            .unwrap()
            .contains("stream = \"unix:/m/install.sock\""));
    }
}
