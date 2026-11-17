//! The library: the machine directories under one folder, read from their
//! `machine.toml`. This is the app's own scan until the core facade owns the
//! library; it reads the same files and the same format the CLI writes.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Entry {
    pub dir: PathBuf,
    pub name: String,
    pub model: String,
    /// The product's SGI name, such as "Origin 200".
    pub product_label: String,
    /// The machine's still, when the product art has one.
    pub art: Option<&'static str>,
    /// Why the machine file could not be read, when it could not.
    pub error: Option<String>,
    pub summary: Summary,
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub nodes: u32,
    pub topology: Option<String>,
    pub memory: String,
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
                product_label: origami_machine::product_label(&m.model, m.topology.as_deref()),
                art: crate::art::of(&m.model, m.topology.as_deref()),
                error: None,
                summary: Summary {
                    nodes: m.nodes,
                    topology: m.topology.clone(),
                    memory: m.memory_per_node.clone(),
                },
            }
        }
        Err(e) => Entry {
            dir: dir.to_path_buf(),
            name,
            model: String::new(),
            product_label: String::new(),
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
    fn machines_of_one_product_are_told_apart_by_directory() {
        let root = std::env::temp_dir().join(format!("origami-gtk-lib-two-{}", std::process::id()));
        let file = "format = 5\n[machine]\nmodel = \"octane2\"\nnodes = 1\ncpus_per_node = 1\nmemory_per_node = \"256MiB\"\n[engine]\nid = \"qemu\"\n";
        let (a, b) = (root.join("a"), root.join("b"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("machine.toml"), file).unwrap();
        }
        let (ea, eb) = (read(&a), read(&b));
        assert_eq!(
            (ea.model.as_str(), eb.model.as_str()),
            ("octane2", "octane2")
        );
        assert_ne!((&ea.dir, &ea.name), (&eb.dir, &eb.name));
        let _ = std::fs::remove_dir_all(root);
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
