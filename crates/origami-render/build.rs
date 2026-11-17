//! Embeds the cube's scene photographs. `resources/identity/scenes/scenes.json`
//! lists them, in the order Alt-click cycles, and names the scene About shows
//! in each appearance; adding a scene is a new image and a new entry.

use std::fmt::Write;
use std::path::Path;

fn main() {
    let dir = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../resources/identity/scenes");
    let manifest = dir.join("scenes.json");
    println!("cargo:rerun-if-changed={}", dir.display());
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).expect("scenes.json"))
            .expect("scenes.json");
    let text = |v: &serde_json::Value, key: &str| -> String {
        v[key]
            .as_str()
            .unwrap_or_else(|| panic!("scenes.json: {v} has no {key}"))
            .to_string()
    };
    let scenes = json["scenes"].as_array().expect("scenes.json: scenes");
    assert!(!scenes.is_empty(), "scenes.json lists no scene");
    let mut out = String::from(
        "pub struct SceneData {\n    pub id: &'static str,\n    pub title: &'static str,\n    \
         pub credit: &'static str,\n    pub source: &'static str,\n    pub image: &'static [u8],\n}\n\n\
         pub static SCENES: &[SceneData] = &[\n",
    );
    for s in scenes {
        let file = dir.join(text(s, "file"));
        println!("cargo:rerun-if-changed={}", file.display());
        writeln!(
            out,
            "    SceneData {{ id: {:?}, title: {:?}, credit: {:?}, source: {:?}, image: include_bytes!({:?}) }},",
            text(s, "id"),
            text(s, "title"),
            text(s, "credit"),
            text(s, "source"),
            file.display().to_string(),
        )
        .unwrap();
    }
    out.push_str("];\n\n");
    for (name, mode) in [("ABOUT_LIGHT", "light"), ("ABOUT_DARK", "dark")] {
        let id = text(&json["about"], mode);
        let at = scenes
            .iter()
            .position(|s| s["id"] == id.as_str())
            .unwrap_or_else(|| {
                panic!("scenes.json: about.{mode} names {id}, which is not a scene")
            });
        writeln!(out, "pub const {name}: usize = {at};").unwrap();
    }
    std::fs::write(
        Path::new(&std::env::var("OUT_DIR").unwrap()).join("scenes.rs"),
        out,
    )
    .unwrap();
}
