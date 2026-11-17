//! Embeds the photographs of `resources/identity/scenes/scenes.json`, each scene's
//! local `hours` ([from, to), wrapping past midnight) and the `about.order` to step.

use std::fmt::Write;
use std::path::Path;

fn main() {
    let dir = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../resources/identity/scenes");
    println!("cargo:rerun-if-changed={}", dir.display());
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("scenes.json")).expect("scenes.json"))
            .expect("scenes.json");
    let text = |v: &serde_json::Value, key: &str| -> String {
        v[key]
            .as_str()
            .unwrap_or_else(|| panic!("scenes.json: {v} has no {key}"))
            .to_string()
    };
    let scenes = json["scenes"].as_array().expect("scenes.json: scenes");
    assert!(!scenes.is_empty(), "scenes.json lists no scene");
    let times = json["about"]["times"].as_object().expect("about.times");
    let mut out = String::from(
        "pub struct SceneData {\n    pub id: &'static str,\n    pub title: &'static str,\n    \
         pub credit: &'static str,\n    pub source: &'static str,\n    pub image: &'static [u8],\n    \
         pub time: &'static str,\n    pub from: f32,\n    pub to: f32,\n}\n\n\
         pub static SCENES: &[SceneData] = &[\n",
    );
    for s in scenes {
        let file = dir.join(text(s, "file"));
        println!("cargo:rerun-if-changed={}", file.display());
        let time = text(s, "time");
        assert!(
            times.contains_key(&time),
            "scenes.json: {s} has an unknown time"
        );
        let h = &s["hours"];
        assert!(
            times[&time] == *h,
            "scenes.json: {s} has hours that differ from about.times.{time}"
        );
        let (from, to) = match (h[0].as_f64(), h[1].as_f64()) {
            (Some(a), Some(b)) if (0.0..24.0).contains(&a) && (0.0..=24.0).contains(&b) => (a, b),
            _ => panic!("scenes.json: {s} needs hours [from, to] within 0 to 24"),
        };
        writeln!(
            out,
            "    SceneData {{ id: {:?}, title: {:?}, credit: {:?}, source: {:?}, image: include_bytes!({:?}), time: {:?}, from: {:?}, to: {:?} }},",
            text(s, "id"),
            text(s, "title"),
            text(s, "credit"),
            text(s, "source"),
            file.display().to_string(),
            time,
            from,
            to,
        )
        .unwrap();
    }
    out.push_str("];\n\n");
    let order: Vec<usize> = json["about"]["order"]
        .as_array()
        .expect("about.order")
        .iter()
        .map(|id| {
            scenes
                .iter()
                .position(|s| s["id"] == *id)
                .unwrap_or_else(|| panic!("scenes.json: about.order names {id}, not a scene"))
        })
        .collect();
    let mut seen = order.clone();
    seen.sort_unstable();
    assert!(
        seen == (0..scenes.len()).collect::<Vec<_>>(),
        "scenes.json: about.order must list every scene once"
    );
    writeln!(out, "pub static ORDER: &[usize] = &{order:?};").unwrap();
    std::fs::write(
        Path::new(&std::env::var("OUT_DIR").unwrap()).join("scenes.rs"),
        out,
    )
    .unwrap();
}
