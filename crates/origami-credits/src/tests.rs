use super::*;
use std::collections::BTreeSet;

fn all() -> Credits {
    credits().unwrap()
}

fn names(c: &Credits) -> BTreeSet<String> {
    c.licences
        .iter()
        .flat_map(|l| l.entries.iter().flat_map(|e| e.components.clone()))
        .collect()
}

#[test]
fn every_scene_has_a_credit_and_a_link() {
    let scenes: serde_json::Value = serde_json::from_str(SCENES).unwrap();
    let listed = scenes["scenes"].as_array().unwrap();
    let c = all();
    assert_eq!(c.photographs.len(), listed.len());
    for p in &c.photographs {
        assert!(!p.title.is_empty() && !p.author.is_empty(), "{p:?}");
        assert!(p.year.len() == 4 && !p.licence.is_empty(), "{p:?}");
        assert!(
            p.licence_url.starts_with("https://creativecommons.org/"),
            "{p:?}"
        );
        assert!(
            p.source
                .starts_with("https://commons.wikimedia.org/wiki/File:"),
            "{p:?}"
        );
    }
}

#[test]
fn a_credit_line_without_a_year_is_refused() {
    let scene = serde_json::json!({
        "title": "T", "source": "https://x", "credit": "Someone, CC BY 2.0, via Wikimedia Commons"
    });
    assert!(photograph(&scene).is_err());
}

#[test]
fn every_bundled_component_appears_under_a_licence() {
    let c = all();
    let have = names(&c);
    let mut skipped = false;
    let mut wanted = 0;
    for line in NOTICES.lines() {
        if line == "Libraries in origami-gui" {
            skipped = true;
        } else if line.starts_with("Libraries ")
            || line.starts_with("Language ")
            || line.starts_with("Rust ")
            || line.starts_with("Go ")
        {
            skipped = false;
        }
        let Some(header) = line.strip_prefix("--- ") else {
            continue;
        };
        if skipped || header.starts_with(NO_FILE) || header.trim_end().ends_with("--") {
            continue;
        }
        let expected: Vec<String> = if header.contains('(') {
            vec![header.to_string()]
        } else {
            header.split(", ").map(str::to_string).collect()
        };
        for component in expected {
            wanted += 1;
            assert!(
                have.contains(&component),
                "{component} is in no licence group"
            );
        }
    }
    assert!(wanted > 100, "only {wanted} components found");
    for group in &c.licences {
        assert!(!group.name.is_empty());
        for e in &group.entries {
            assert!(
                !e.components.is_empty() && !e.text.trim().is_empty(),
                "{}",
                group.name
            );
        }
    }
}

const NO_FILE: &str = "Crates with no licence file";

#[test]
fn what_we_ship_and_depend_on_is_listed_with_its_text() {
    let c = all();
    let have = names(&c);
    for name in [
        "Origami",
        "Instigator",
        "iris",
        "QEMU",
        "libghostty-vt (the apps' terminal engine)",
    ] {
        assert!(
            have.contains(name),
            "{name} is missing: {:?}",
            have.iter().take(5).collect::<Vec<_>>()
        );
    }
    let gpl = c
        .licences
        .iter()
        .find(|l| l.name == "GNU General Public License")
        .unwrap();
    let qemu = gpl
        .entries
        .iter()
        .find(|e| e.components == ["QEMU"])
        .unwrap();
    assert!(qemu.text.contains("Version 2, June 1991"));
    assert_eq!(c.licences[0].name, "GNU General Public License");
}

#[test]
fn qemu_source_is_the_tag_in_the_lock() {
    let tag = qemu_tag().unwrap();
    let c = all();
    assert_eq!(c.qemu.tag, tag);
    assert_eq!(
        c.qemu.url,
        format!("https://github.com/jamesbraid/qemu-sgi-origami/tree/{tag}")
    );
    assert!(c.qemu.note.contains(&tag));
    let file = qemu_source_file().unwrap();
    assert!(file.contains(&c.qemu.url) && file.contains(&c.qemu.archive_url));
    assert!(qemu_tag_of("[qemu]\n").is_err());
}

#[test]
fn the_people_are_credited() {
    let c = all();
    let built: Vec<_> = c.built_on.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(built, ["QEMU", "iris", "libghostty-vt", "Instigator"]);
    assert_eq!(c.built_on[1].by, "Dominik Behr and contributors");
    assert_eq!(c.thanks.len(), 4);
    assert!(c.copyright.starts_with("© ") && c.copyright.ends_with("James Braid"));
}

#[test]
fn texts_name_every_licence_they_carry() {
    let both = "Permission is hereby granted, free of charge, to any person\n\
                Apache License\nVersion 2.0, January 2004";
    assert_eq!(
        notices::kind(both).as_deref(),
        Some("Apache License 2.0 and MIT")
    );
    assert_eq!(notices::kind("nothing to see"), None);
    let mpl = "Mozilla Public License, version 2.0\n\n1.12. \"Secondary License\"\n  means either the GNU General Public License, Version 2.0,\n  the GNU Lesser General Public License, Version 2.1";
    assert_eq!(
        notices::kind(mpl).as_deref(),
        Some("Mozilla Public License 2.0")
    );
}

#[test]
fn crates_without_a_licence_file_are_grouped_by_their_declared_licence() {
    let bar = "=".repeat(78);
    let text = format!(
        "{bar}\nRust crates\n{bar}\n\n--- Crates with no licence file in their package; the licence named applies\n\
         foo 1.0 (MIT OR Apache-2.0)\nbar 2.0 (MIT)\nbaz 3.0 (ISC)\n"
    );
    let found = parse_notices(&text);
    let mit: Vec<_> = found
        .iter()
        .filter(|e| e.name == "MIT")
        .flat_map(|e| e.components.clone())
        .collect();
    assert_eq!(mit, ["bar 2.0", "foo 1.0"]);
    assert!(found.iter().any(|e| e.name == "ISC"));
}

#[test]
fn a_header_with_a_parenthesis_is_one_component() {
    let bar = "=".repeat(78);
    let text = format!(
        "{bar}\nLibraries\n{bar}\n\n--- GCC runtime support, including libstdc++ (statically linked)\nMIT License\n\n--- anstream, clap 4.5\nMIT License\n"
    );
    let found = parse_notices(&text);
    assert_eq!(
        found[0].components,
        ["GCC runtime support, including libstdc++ (statically linked)"]
    );
    assert_eq!(found[1].components, ["anstream", "clap 4.5"]);
}

#[test]
fn a_rule_line_inside_a_text_does_not_start_a_group() {
    let bar = "=".repeat(78);
    let text = format!("{bar}\nRust crates\n{bar}\n\n--- foo 1.0\nApache License\n\n--- LLVM Exceptions ----\nstill foo\n");
    let found = parse_notices(&text);
    assert_eq!(found.len(), 1);
    assert!(found[0].text.contains("still foo"));
}
