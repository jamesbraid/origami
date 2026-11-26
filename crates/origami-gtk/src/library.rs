//! The library: the machine directories under one folder, read from their
//! `machine.toml`. This is the app's own scan until the core facade owns the
//! library; it reads the same files and the same format the CLI writes.

use origami_core::facade::{MachineSummary, ShownState};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Entry {
    pub dir: PathBuf,
    pub name: String,
    /// The product's SGI name, such as "Origin 200".
    pub product_label: String,
    /// The machine's still, when the product art has one.
    pub art: Option<&'static str>,
    /// Why the machine file could not be read, when it could not.
    pub error: Option<String>,
    /// Made by an older Origami: it opens as the upgrade dialog until upgraded.
    pub needs_upgrade: bool,
    /// What the system disk holds, such as `IRIX 6.5.30`, once read.
    pub system_disk: Option<String>,
    /// Install IRIX applies only to machines that run IRIX.
    pub runs_irix: bool,
    /// The tiles under the name, as the core words them.
    pub cards: Vec<origami_core::facade::SummaryCard>,
    /// What the core listed, for the state words and the row's description.
    pub summary: Option<MachineSummary>,
}

/// The grey a machine with no still is tinted with.
const NO_ART: u32 = 0x7b7b84;

impl Entry {
    /// The model the library row shows under the name of a stopped machine.
    pub fn model(&self) -> &str {
        match &self.summary {
            Some(m) => m.topology_name.as_deref().unwrap_or(&m.product_label),
            None => &self.product_label,
        }
    }

    /// The state the core shows for a machine that is not open.
    pub fn shown(&self) -> ShownState {
        self.summary
            .as_ref()
            .map_or(ShownState::Stopped, |m| m.shown(false))
    }

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
            e.name = m.name.clone();
            e.needs_upgrade = m.needs_upgrade;
            e.system_disk = m.system_disk.clone();
            e.runs_irix = m.runs_irix;
            e.cards = m.cards.clone();
            e.error = m.problem.clone();
            e.summary = Some(m);
            e
        })
        .collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

/// One machine as the core lists it now, for a refresh after an edit.
pub fn fresh(core: &origami_core::facade::Core, dir: &Path) -> Entry {
    entries(core)
        .into_iter()
        .find(|e| e.dir == dir)
        .unwrap_or_else(|| read(dir))
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
                product_label: origami_machine::product_label(&m.model, m.topology.as_deref()),
                art: crate::art::of(&m.model, m.topology.as_deref()),
                error: None,
                needs_upgrade: false,
                system_disk: None,
                runs_irix: true,
                cards: Vec::new(),
                summary: None,
            }
        }
        Err(e) => Entry {
            dir: dir.to_path_buf(),
            name,
            product_label: String::new(),
            art: None,
            error: Some(e.to_string()),
            needs_upgrade: false,
            system_disk: None,
            runs_irix: true,
            cards: Vec::new(),
            summary: None,
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
            (ea.product_label.as_str(), eb.product_label.as_str()),
            ("Octane2", "Octane2")
        );
        assert_ne!((&ea.dir, &ea.name), (&eb.dir, &eb.name));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_row_with_no_summary_is_stopped_and_names_its_product() {
        let mut e = read(Path::new("/nonexistent"));
        assert_eq!(e.shown(), ShownState::Stopped);
        e.product_label = "Fuel".into();
        assert_eq!(e.model(), "Fuel");
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
