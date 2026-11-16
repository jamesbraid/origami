//! The library: the machine directories under one folder, read from their
//! `machine.toml`. This is the app's own scan until the core facade owns the
//! library; it reads the same files and the same format the CLI writes.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Entry {
    pub dir: PathBuf,
    pub name: String,
    pub model: String,
    pub chassis: Chassis,
    /// Why the machine file could not be read, when it could not.
    pub error: Option<String>,
    pub summary: Summary,
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub processor: String,
    pub cpus: u32,
    pub memory: String,
    pub graphics: Option<String>,
    pub drives: usize,
    pub network: String,
    pub forwards: usize,
    pub shares: usize,
}

/// A product's colour. It is identity, not state: it never carries meaning
/// on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chassis {
    Octane,
    Octane2,
    O2,
    Fuel,
    Tezro,
    Origin,
    Onyx,
    Other,
}

impl Chassis {
    pub fn of(model: &str) -> Chassis {
        let m = model.to_ascii_lowercase();
        // Octane2 before Octane, Onyx before Origin's shared prefix.
        if m.contains("octane2") {
            Chassis::Octane2
        } else if m.contains("octane") || m.contains("ip30") {
            Chassis::Octane
        } else if m.contains("o2") || m.contains("ip32") {
            Chassis::O2
        } else if m.contains("fuel") {
            Chassis::Fuel
        } else if m.contains("tezro") {
            Chassis::Tezro
        } else if m.contains("onyx") {
            Chassis::Onyx
        } else if m.contains("origin") {
            Chassis::Origin
        } else {
            Chassis::Other
        }
    }

    pub fn css(self) -> &'static str {
        match self {
            Chassis::Octane => "chassis-octane",
            Chassis::Octane2 => "chassis-octane2",
            Chassis::O2 => "chassis-o2",
            Chassis::Fuel => "chassis-fuel",
            Chassis::Tezro => "chassis-tezro",
            Chassis::Origin => "chassis-origin",
            Chassis::Onyx => "chassis-onyx",
            Chassis::Other => "chassis-other",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Chassis::Origin | Chassis::Onyx => "network-server-symbolic",
            _ => "computer-symbolic",
        }
    }
}

/// The library as the core lists it, each machine with what its hero card shows.
pub fn entries(core: &origami_core::facade::Core) -> Vec<Entry> {
    let listed = core.list_machines().unwrap_or_else(|e| {
        eprintln!("origami-gtk: cannot read the library: {e}");
        Vec::new()
    });
    let mut out: Vec<Entry> = listed
        .into_iter()
        .map(|m| {
            let mut e = read(Path::new(&m.key));
            e.name = m.name;
            if let Some(p) = m.problem {
                e.error = Some(p);
            }
            e
        })
        .collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

pub fn read(dir: &Path) -> Entry {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    match origami_machine::read_machine(dir) {
        Ok(file) => {
            let m = &file.machine;
            Entry {
                dir: dir.to_path_buf(),
                name,
                model: m.model.clone(),
                chassis: Chassis::of(&m.model),
                error: None,
                summary: Summary {
                    processor: m.processor.clone().unwrap_or_default(),
                    cpus: m.nodes * m.cpus_per_node,
                    memory: m.memory_per_node.clone(),
                    graphics: file.pipe.first().map(|p| p.product.clone()),
                    drives: file.drive.len(),
                    network: file.network.mode.clone(),
                    forwards: file.network.forward.len(),
                    shares: file.share.len(),
                },
            }
        }
        Err(e) => Entry {
            dir: dir.to_path_buf(),
            name,
            model: String::new(),
            chassis: Chassis::Other,
            error: Some(e.to_string()),
            summary: Summary::default(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chassis_follows_the_model_name() {
        assert_eq!(Chassis::of("octane2"), Chassis::Octane2);
        assert_eq!(Chassis::of("Octane"), Chassis::Octane);
        assert_eq!(Chassis::of("o2"), Chassis::O2);
        assert_eq!(Chassis::of("origin350"), Chassis::Origin);
        assert_eq!(Chassis::of("onyx2"), Chassis::Onyx);
        assert_eq!(Chassis::of("mystery"), Chassis::Other);
    }

    #[test]
    fn a_broken_machine_file_is_read_with_its_error() {
        let dir = std::env::temp_dir().join(format!("origami-gtk-lib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("machine.toml"), "format = 99\n").unwrap();
        assert!(read(&dir).error.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
