use crate::{Catalog, Machine, Offering, Result};
use std::collections::BTreeMap;

pub fn population(machine: &Machine) -> Vec<u32> {
    if machine.population.is_empty() {
        vec![machine.cpus_per_node; machine.nodes as usize]
    } else {
        machine.population.clone()
    }
}

pub struct Profile {
    pub id: &'static str,
    pub topology: &'static str,
    pub population: &'static [u32],
    pub boot_prom: &'static str,
    pub io_prom: Option<&'static str>,
}

pub const STARTERS: &[Profile] = &[
    Profile {
        id: "origin200-1",
        topology: "origin200",
        population: &[1],
        boot_prom: "ip27prom-6.156",
        io_prom: None,
    },
    Profile {
        id: "origin200-2",
        topology: "origin200",
        population: &[2],
        boot_prom: "ip27prom-6.156",
        io_prom: None,
    },
    Profile {
        id: "origin200-dual",
        topology: "origin200-dual",
        population: &[2, 2],
        boot_prom: "ip27prom-6.156",
        io_prom: None,
    },
    Profile {
        id: "origin2000-8",
        topology: "origin2000-rack",
        population: &[2, 2, 2, 2],
        boot_prom: "ip27prom-6.156",
        io_prom: Some("io6prom-6.156"),
    },
    Profile {
        id: "origin300-2",
        topology: "origin300",
        population: &[2],
        boot_prom: "ip35prom-6.210",
        io_prom: None,
    },
    Profile {
        id: "origin200-impact",
        topology: "origin200-gigachannel",
        population: &[1],
        boot_prom: "ip27prom-6.156",
        io_prom: Some("io6prom-6.156"),
    },
    Profile {
        id: "octane-impact",
        topology: "octane",
        population: &[1],
        boot_prom: "IP30prom-4.17",
        io_prom: None,
    },
    Profile {
        id: "octane2-impact",
        topology: "octane2",
        population: &[1],
        boot_prom: "IP30prom-4.17",
        io_prom: None,
    },
    Profile {
        id: "onyx2-infinite-reality",
        topology: "onyx2-deskside",
        population: &[2, 2],
        boot_prom: "ip27prom-6.156",
        io_prom: Some("io6prom-6.156"),
    },
    Profile {
        id: "fuel-1",
        topology: "fuel",
        population: &[1],
        boot_prom: "ip35prom-6.210",
        io_prom: None,
    },
    Profile {
        id: "origin300-v12-direct-2",
        topology: "origin300-v12-direct",
        population: &[2],
        boot_prom: "ip35prom-6.210",
        io_prom: None,
    },
    Profile {
        id: "origin300-vbrick-2",
        topology: "origin300-vbrick",
        population: &[2],
        boot_prom: "ip35prom-6.210",
        io_prom: None,
    },
];

pub fn profile(id: &str) -> Option<&'static Profile> {
    STARTERS.iter().find(|profile| profile.id == id)
}

pub fn presets(catalog: &Catalog) -> Vec<(String, &Offering)> {
    STARTERS
        .iter()
        .filter_map(|profile| {
            catalog
                .offerings
                .iter()
                .find(|o| o.topology == profile.topology && o.cpus_per_node == profile.population)
                .map(|o| (profile.id.to_string(), o))
        })
        .collect()
}

pub fn default_graphics(offering: &Offering) -> &'static str {
    match offering.topology.as_str() {
        "onyx2-deskside" | "onyx2-rack" => "infinite-reality",
        "origin300-v12-direct" | "origin300-vbrick" => "vpro",
        "origin200" if offering.smp == 1 => "rad4",
        _ => "none",
    }
}

pub fn graphics(offering: &Offering) -> Vec<&'static str> {
    match offering.topology.as_str() {
        "octane" | "octane2" => vec!["none", "si"],
        "origin200-gigachannel" | "origin2000-deskside" | "origin2000-rack" => {
            vec!["none", "si", "esi", "infinite-reality"]
        }
        "onyx2-deskside" | "onyx2-rack" => vec!["infinite-reality"],
        "origin300-v12-direct" | "origin300-vbrick" => vec!["vpro"],
        "origin200" | "origin200-dual" => vec!["none", "rad4"],
        _ => vec!["none"],
    }
}

pub fn validate_graphics(offering: &Offering, graphics: &str) -> Result<()> {
    if !self::graphics(offering).contains(&graphics) {
        return Err(format!(
            "{} does not implement {graphics} graphics; choose {}",
            offering.topology,
            self::graphics(offering).join(", ")
        )
        .into());
    }
    Ok(())
}

pub const OCTANE2_CPU_INPUTS: &[&str] = &[
    "r12000-prid",
    "r12000-fpu-id",
    "r12000-reset-mode",
    "r12000-scache-bytes",
    "r12000-scache-block-words",
];

fn number(value: &str) -> Result<u64> {
    Ok(if let Some(hex) = value.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)?
    } else {
        value.parse()?
    })
}

// QEMU's R12000 CPU model owns the meaning and validity of these values.
fn validate_octane2_cpu(inputs: &BTreeMap<String, String>) -> Result<()> {
    for key in OCTANE2_CPU_INPUTS {
        if !inputs.contains_key(*key) {
            return Err(
                format!("Octane2 requires --{key}; no measured CPU default is available").into(),
            );
        }
    }
    if inputs.len() != OCTANE2_CPU_INPUTS.len() {
        return Err("unknown Octane2 CPU input".into());
    }
    Ok(())
}

pub fn validate_inputs(offering: &Offering, inputs: &BTreeMap<String, String>) -> Result<()> {
    const REQUIRED: &[(&str, u64)] = &[
        ("fuel-board-id-word", u64::MAX),
        ("fuel-bedrock-revision", 15),
        ("fuel-ioc3-subsystem-id", 65535),
        ("fuel-l1-type-code", 255),
    ];
    if offering.product == "octane2" {
        return validate_octane2_cpu(inputs);
    }
    if offering.product != "fuel" {
        if !inputs.is_empty() {
            return Err("launch inputs are only supported for Fuel and Octane2".into());
        }
        return Ok(());
    }
    for (key, maximum) in REQUIRED {
        let value = inputs.get(*key).ok_or_else(|| {
            format!("Fuel requires --{key}; this experimental board input has no product default")
        })?;
        let number = number(value)?;
        if number > *maximum
            || (*key == "fuel-l1-type-code" && number == 0)
            || (*key == "fuel-board-id-word" && number & 61440 != 16384)
        {
            return Err(format!("invalid {key}: {value}").into());
        }
    }
    if inputs.len() != REQUIRED.len() {
        return Err("unknown Fuel launch input".into());
    }
    Ok(())
}

pub fn add_graphics(args: &mut Vec<String>, offering: &Offering, graphics: &str) -> Result<()> {
    validate_graphics(offering, graphics)?;
    let device = match graphics {
        "rad4" => Some("psitech-rad4,addr=5".to_string()),
        "si" | "esi" if offering.product != "octane" && offering.product != "octane2" => {
            Some(format!("sgi-mgras,slot=io3,board={graphics}"))
        }
        "infinite-reality" if offering.product != "onyx2" => Some("sgi-kona,slot=io3".to_string()),
        _ => None,
    };
    if let Some(device) = device {
        args.extend(["-device".into(), device]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{preset, runtime, MachineFile, Network};
    use std::path::Path;

    fn file(o: &Offering, graphics: &str) -> MachineFile {
        MachineFile {
            format: crate::MACHINE_FORMAT,
            machine: Machine {
                model: o.product.clone(),
                topology: Some(o.topology.clone()),
                population: o.cpus_per_node.clone(),
                inputs: BTreeMap::new(),
                nodes: o.nodes,
                cpus_per_node: o.cpus_per_node[0],
                memory_per_node: format!("{}MiB", o.memory.default),
                graphics: graphics.into(),
            },
            identity: None,
            network: Network::default(),
            drive: vec![],
        }
    }

    #[test]
    fn starter_profiles_are_unique_and_cover_implemented_families() {
        let catalog = crate::test_catalogue();
        let profiles = presets(&catalog);
        let names: std::collections::HashSet<_> = profiles.iter().map(|(name, _)| name).collect();
        assert_eq!(names.len(), profiles.len());
        assert_eq!(profiles.len(), 12);
        for product in [
            "origin200",
            "origin2000",
            "origin300",
            "onyx2",
            "octane",
            "octane2",
            "fuel",
        ] {
            assert!(profiles.iter().any(|(_, o)| o.product == product));
        }
        for name in [
            "origin200-impact",
            "octane-impact",
            "octane2-impact",
            "onyx2-infinite-reality",
            "origin300-v12-direct-2",
            "origin300-vbrick-2",
        ] {
            assert!(preset(&catalog, name).is_ok());
        }
        assert!(preset(&catalog, "origin300-two-chassis-4-2").is_err());
        assert!(catalog
            .offerings
            .iter()
            .any(|o| o.topology == "origin300-two-chassis" && o.cpus_per_node == [4, 2]));
        for legacy in [
            "origin200-1",
            "origin200-2",
            "origin200-dual",
            "origin2000-8",
            "origin300-2",
        ] {
            assert!(preset(&catalog, legacy).is_ok());
        }
    }

    #[test]
    fn origin200_impact_uses_the_gigachannel_slot() {
        let catalog = crate::test_catalogue();
        let o = preset(&catalog, "origin200-impact").unwrap();
        let args = runtime::arguments(
            Path::new("/machine"),
            &file(o, "si"),
            o,
            runtime::Display::Local,
        )
        .unwrap();
        assert!(args
            .windows(2)
            .any(|p| p[0] == "-M" && p[1].contains("topology=origin200-gigachannel")));
        assert!(args
            .windows(2)
            .any(|p| p == ["-device", "sgi-mgras,slot=io3,board=si"]));
        assert!(!args.iter().any(|a| a.contains("psitech")));
    }

    #[test]
    fn octane_impact_uses_machine_graphics_and_embedded_network() {
        let catalog = crate::test_catalogue();
        let o = preset(&catalog, "octane-impact").unwrap();
        let args = runtime::arguments(
            Path::new("/machine"),
            &file(o, "si"),
            o,
            runtime::Display::Vnc { port: 5901 },
        )
        .unwrap();
        assert!(
            args.windows(2)
                .any(|p| p[0] == "-M"
                    && p[1].starts_with("octane,topology=octane,graphics-board=si,"))
        );
        assert!(args.contains(&"nic,model=sgi-ioc3-eth,netdev=net0".into()));
        assert!(validate_graphics(o, "vpro").is_err());
    }

    #[test]
    fn onyx2_uses_its_fitted_infinite_reality_pipe() {
        let catalog = crate::test_catalogue();
        let o = preset(&catalog, "onyx2-infinite-reality").unwrap();
        let args = runtime::arguments(
            Path::new("/machine"),
            &file(o, "infinite-reality"),
            o,
            runtime::Display::None,
        )
        .unwrap();
        assert!(args.iter().any(|a| a.contains("topology=onyx2-deskside")));
        assert!(!args.iter().any(|a| a.contains("sgi-kona")));
        assert!(args
            .iter()
            .any(|a| a.starts_with("driver=raw,node-name=node1-flash,")));
    }

    #[test]
    fn heterogeneous_cpu_populations_are_explicit() {
        let catalog = crate::test_catalogue();
        for o in catalog
            .offerings
            .iter()
            .filter(|o| !matches!(o.product.as_str(), "fuel" | "octane2"))
        {
            let graphics = default_graphics(o);
            let f = file(o, graphics);
            let args =
                runtime::arguments(Path::new("/machine"), &f, o, runtime::Display::None).unwrap();
            assert!(args
                .windows(2)
                .any(|p| p[0] == "-smp" && p[1] == o.smp.to_string()));
            if !matches!(o.product.as_str(), "octane" | "octane2") {
                let counts = o
                    .cpus_per_node
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(":");
                assert!(args
                    .iter()
                    .any(|a| a.contains(&format!("population={counts}"))));
                assert!(args
                    .iter()
                    .any(|a| a.contains(&format!("nodes={}", o.nodes))));
            }
        }
    }

    #[test]
    fn octane2_requires_explicit_cpu_properties_on_cpu_option() {
        let catalog = crate::test_catalogue();
        let o = preset(&catalog, "octane2-impact").unwrap();
        let mut f = file(o, "si");
        assert!(
            runtime::arguments(Path::new("/machine"), &f, o, runtime::Display::None)
                .unwrap_err()
                .to_string()
                .contains("r12000-prid")
        );
        // Synthetic configuration from QEMU tests/qtest/octane-test.c.
        f.machine.inputs = [
            ("r12000-prid", "0xe24"),
            ("r12000-fpu-id", "0x7654"),
            ("r12000-reset-mode", "0x1aa683"),
            ("r12000-scache-bytes", "2097152"),
            ("r12000-scache-block-words", "32"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        let args =
            runtime::arguments(Path::new("/machine"), &f, o, runtime::Display::None).unwrap();
        let cpu = args.windows(2).find(|p| p[0] == "-cpu").unwrap();
        let machine = args.windows(2).find(|p| p[0] == "-M").unwrap();
        for key in OCTANE2_CPU_INPUTS {
            assert!(cpu[1].contains(&format!("{key}={}", f.machine.inputs[*key])));
            assert!(!machine[1].contains(key));
            let mut missing = f.machine.inputs.clone();
            missing.remove(*key);
            assert!(validate_inputs(o, &missing).is_err());
        }
    }

    #[test]
    fn fuel_requires_explicit_inputs_and_rejects_vpro() {
        let catalog = crate::test_catalogue();
        let o = preset(&catalog, "fuel-1").unwrap();
        let mut f = file(o, "none");
        assert!(runtime::arguments(Path::new("/machine"), &f, o, runtime::Display::None).is_err());
        f.machine.inputs = [
            ("fuel-board-id-word", "0x4000"),
            ("fuel-bedrock-revision", "0"),
            ("fuel-ioc3-subsystem-id", "0"),
            ("fuel-l1-type-code", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        let args =
            runtime::arguments(Path::new("/machine"), &f, o, runtime::Display::None).unwrap();
        assert!(args.iter().any(|a| a.contains("fuel-board-id-word=0x4000")));
        assert!(validate_graphics(o, "vpro").is_err());
        assert!(preset(&catalog, "origin350-2").is_err());
        assert!(preset(&catalog, "tezro-4").is_err());
    }
}
