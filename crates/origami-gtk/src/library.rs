//! The library: the machine directories under one folder, read from their
//! `machine.toml`. This is the app's own scan until the core facade owns the
//! library; it reads the same files and the same format the CLI writes.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Entry {
    pub dir: PathBuf,
    pub name: String,
    pub model: String,
    /// The machine's still, when the product art has one.
    pub art: Option<&'static str>,
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

/// The grey a machine with no still is tinted with.
const NO_ART: u32 = 0x7b7b84;

impl Entry {
    /// The machine's measured body colour, for tints and washes.
    pub fn colour(&self) -> u32 {
        self.art.map_or(NO_ART, crate::art::colour)
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
                art: crate::art::of(&m.model, m.topology.as_deref()),
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
            art: None,
            error: Some(e.to_string()),
            summary: Summary::default(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_without_a_still_gets_the_neutral_tint() {
        let mut e = read(Path::new("/nonexistent"));
        assert_eq!(e.art, None);
        assert_eq!(e.colour(), NO_ART);
        e.art = Some("octane2");
        assert_eq!(e.colour(), 0x2e354e);
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
