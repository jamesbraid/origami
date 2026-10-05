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

/// Board values QEMU accepts as optional machine-property overrides of its
/// catalogue defaults. QEMU checks each value and the machines it applies to.
// TODO: the catalogue's hardware-inputs does not list these family overrides
// yet. Drop this list once it does, so each offering accepts only its own.
pub const BOARD_OVERRIDES: &[&str] = &[
    "board-id-word",
    "ioc3-subsystem-id",
    "l1-type-code",
    "l1-revision",
    "bedrock-revision",
];

pub fn validate_inputs(offering: &Offering, inputs: &BTreeMap<String, String>) -> Result<()> {
    let listed = |key: &str| offering.hardware_inputs.iter().any(|i| i.option == key);
    for input in offering.hardware_inputs.iter().filter(|i| i.required) {
        if !inputs.contains_key(&input.option) {
            return Err(format!("{} requires --{}", offering.topology, input.option).into());
        }
    }
    for (key, value) in inputs {
        if !listed(key) && !BOARD_OVERRIDES.contains(&key.as_str()) {
            return Err(format!("{} does not take --{key}", offering.topology).into());
        }
        // Values are QEMU's to judge, but must stay one option value.
        if value.is_empty()
            || value
                .chars()
                .any(|c| c == ',' || c == '=' || c.is_whitespace())
        {
            return Err(format!("invalid --{key} value: {value:?}").into());
        }
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
                console: None,
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
        assert!(args.windows(2).any(|p| p[0] == "-M"
            && p[1].starts_with("octane,topology=octane,nodes=1,graphics-board=si,")));
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
        for o in catalog.offerings.iter() {
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
    fn fuel_board_values_are_optional_machine_overrides() {
        let catalog = crate::test_catalogue();
        let o = preset(&catalog, "fuel-1").unwrap();
        let mut f = file(o, "none");
        let args =
            runtime::arguments(Path::new("/machine"), &f, o, runtime::Display::None).unwrap();
        let machine = &args.windows(2).find(|p| p[0] == "-M").unwrap()[1];
        assert!(!BOARD_OVERRIDES.iter().any(|key| machine.contains(key)));
        f.machine.inputs = [("board-id-word", "0x4000"), ("l1-revision", "1.2.3")]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let args =
            runtime::arguments(Path::new("/machine"), &f, o, runtime::Display::None).unwrap();
        let machine = &args.windows(2).find(|p| p[0] == "-M").unwrap()[1];
        assert!(machine.contains(",board-id-word=0x4000"), "{machine}");
        assert!(machine.contains(",l1-revision=1.2.3"), "{machine}");
        f.machine
            .inputs
            .insert("fuel-mac-eeprom".into(), "mac.bin".into());
        assert!(validate_inputs(o, &f.machine.inputs).is_ok());
        f.machine
            .inputs
            .insert("fuel-board-id-word".into(), "0x4000".into());
        assert!(validate_inputs(o, &f.machine.inputs).is_err());
        let mut required = o.clone();
        required.hardware_inputs.push(crate::InputBinding {
            option: "board-revision".into(),
            required: true,
        });
        let error = validate_inputs(&required, &BTreeMap::new()).unwrap_err();
        assert!(error.to_string().contains("--board-revision"), "{error}");
        assert!(validate_graphics(o, "vpro").is_err());
        assert!(preset(&catalog, "origin350-2").is_err());
        assert!(preset(&catalog, "tezro-4").is_err());
    }
}
