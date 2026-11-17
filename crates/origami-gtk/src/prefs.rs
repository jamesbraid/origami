//! The app's own settings, kept as JSON beside the user's other Origami
//! configuration. A missing or unreadable file means defaults: a preference
//! must never keep the app from starting.

use crate::view_scale::ViewScale;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PointerMode {
    /// The guest cursor goes where the host pointer is.
    #[default]
    Follow,
    /// A click takes the pointer; relative motion drives the guest until the
    /// release chord.
    Capture,
}

impl PointerMode {
    pub fn label(self) -> &'static str {
        match self {
            PointerMode::Follow => "Follows mouse",
            PointerMode::Capture => "Capture on click",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default)]
    pub pointer: PointerMode,
    /// Per-machine override, keyed by machine directory.
    #[serde(default)]
    pub machine_pointer: BTreeMap<String, PointerMode>,
    /// Per-machine view scale, keyed by machine directory.
    #[serde(default)]
    pub machine_view: BTreeMap<String, ViewScale>,
    /// Machines the user asked to keep running when the window was closed.
    #[serde(default)]
    pub remember_close_choice: Option<String>,
    #[serde(skip)]
    path: PathBuf,
}

impl Prefs {
    pub fn load(dir: &Path) -> Prefs {
        let path = dir.join("gtk.json");
        let mut p: Prefs = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        p.path = path;
        p
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = self.path.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    pub fn pointer_for(&self, dir: &Path) -> PointerMode {
        self.machine_pointer
            .get(&dir.display().to_string())
            .copied()
            .unwrap_or(self.pointer)
    }

    pub fn set_pointer_for(&mut self, dir: &Path, mode: Option<PointerMode>) {
        let key = dir.display().to_string();
        match mode {
            Some(m) => self.machine_pointer.insert(key, m),
            None => self.machine_pointer.remove(&key),
        };
        self.save();
    }

    pub fn view_for(&self, dir: &Path) -> ViewScale {
        self.machine_view
            .get(&dir.display().to_string())
            .copied()
            .unwrap_or_default()
    }

    pub fn set_view_for(&mut self, dir: &Path, view: ViewScale) {
        self.machine_view.insert(dir.display().to_string(), view);
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_override_beats_the_default_and_clearing_it_restores_the_default() {
        let dir = std::env::temp_dir().join(format!("origami-gtk-prefs-{}", std::process::id()));
        let mut p = Prefs::load(&dir);
        let m = Path::new("/lib/o2");
        assert_eq!(p.pointer_for(m), PointerMode::Follow);
        p.set_pointer_for(m, Some(PointerMode::Capture));
        assert_eq!(Prefs::load(&dir).pointer_for(m), PointerMode::Capture);
        p.set_pointer_for(m, None);
        assert_eq!(Prefs::load(&dir).pointer_for(m), PointerMode::Follow);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_view_scale_is_kept_per_machine_directory() {
        let dir =
            std::env::temp_dir().join(format!("origami-gtk-prefs-view-{}", std::process::id()));
        let mut p = Prefs::load(&dir);
        p.set_view_for(Path::new("/lib/a/o2"), ViewScale::X2);
        let p = Prefs::load(&dir);
        assert_eq!(p.view_for(Path::new("/lib/a/o2")), ViewScale::X2);
        assert_eq!(p.view_for(Path::new("/lib/b/o2")), ViewScale::Fit);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_broken_file_gives_defaults() {
        let dir =
            std::env::temp_dir().join(format!("origami-gtk-prefs-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gtk.json"), b"{not json").unwrap();
        assert_eq!(Prefs::load(&dir).pointer, PointerMode::Follow);
        let _ = std::fs::remove_dir_all(dir);
    }
}
