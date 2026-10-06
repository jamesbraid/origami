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
}

pub const STARTERS: &[Profile] = &[
    Profile {
        id: "origin200-1",
        topology: "origin200",
        population: &[1],
    },
    Profile {
        id: "origin200-2",
        topology: "origin200",
        population: &[2],
    },
    Profile {
        id: "origin200-dual",
        topology: "origin200-dual",
        population: &[2, 2],
    },
    Profile {
        id: "origin2000-8",
        topology: "origin2000-rack",
        population: &[2, 2, 2, 2],
    },
    Profile {
        id: "origin300-2",
        topology: "origin300",
        population: &[2],
    },
    Profile {
        id: "origin200-impact",
        topology: "origin200-gigachannel",
        population: &[1],
    },
    Profile {
        id: "octane-impact",
        topology: "octane",
        population: &[1],
    },
    Profile {
        id: "octane2-impact",
        topology: "octane2",
        population: &[1],
    },
    Profile {
        id: "onyx2-infinite-reality",
        topology: "onyx2-deskside",
        population: &[2, 2],
    },
    Profile {
        id: "fuel-1",
        topology: "fuel",
        population: &[1],
    },
    Profile {
        id: "origin300-v12-direct-2",
        topology: "origin300-v12-direct",
        population: &[2],
    },
    Profile {
        id: "origin300-vbrick-2",
        topology: "origin300-vbrick",
        population: &[2],
    },
];

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

/// Where a launch input is set: `machine` (-M) or `cpu` (-cpu). The
/// catalogue's overrides name the values a launch may replace, and its
/// hardware inputs the further options a configuration binds.
pub fn input_target<'a>(offering: &'a Offering, key: &str) -> Option<&'a str> {
    if let Some(item) = offering.overrides.iter().find(|o| o.property == key) {
        return Some(&item.target);
    }
    offering
        .hardware_inputs
        .iter()
        .any(|input| input.option == key)
        .then_some("machine")
}

pub fn validate_inputs(offering: &Offering, inputs: &BTreeMap<String, String>) -> Result<()> {
    for input in offering.hardware_inputs.iter().filter(|i| i.required) {
        if !inputs.contains_key(&input.option) {
            return Err(format!(
                "{} requires --set {}=VALUE",
                offering.topology, input.option
            )
            .into());
        }
    }
    for (key, value) in inputs {
        // The machine MAC is the identity's, so it has one source.
        if key == "mac" {
            return Err("set the machine MAC with --mac".into());
        }
        match input_target(offering, key) {
            Some("machine" | "cpu") => (),
            Some(target) => {
                return Err(format!("{key} has unsupported override target {target}").into())
            }
            None => {
                let mut names: Vec<_> = offering
                    .overrides
                    .iter()
                    .map(|o| o.property.as_str())
                    .chain(offering.hardware_inputs.iter().map(|i| i.option.as_str()))
                    .filter(|name| *name != "mac")
                    .collect();
                names.sort();
                return Err(format!(
                    "{} does not take {key}; it takes {}",
                    offering.topology,
                    names.join(", ")
                )
                .into());
            }
        }
        // Values are QEMU's to judge, but must stay one option value.
        if value.is_empty()
            || value
                .chars()
                .any(|c| c == ',' || c == '=' || c.is_whitespace())
        {
            return Err(format!("invalid {key} value: {value:?}").into());
        }
    }
    Ok(())
}

/// Whether the offering's machine takes a `mac` property.
pub fn takes_mac(offering: &Offering) -> bool {
    input_target(offering, "mac") == Some("machine")
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
    fn impact_boards_use_the_xio_slot_and_onyx2_its_fitted_pipe() {
        let catalog = crate::test_catalogue();
        let o = preset(&catalog, "origin2000-8").unwrap();
        let args = runtime::arguments(
            Path::new("/machine"),
            &file(o, "si"),
            o,
            runtime::Display::Local,
        )
        .unwrap();
        assert!(args
            .windows(2)
            .any(|p| p == ["-device", "sgi-mgras,slot=io3,board=si"]));
        let mut args = vec![];
        add_graphics(&mut args, o, "infinite-reality").unwrap();
        assert_eq!(args, ["-device", "sgi-kona,slot=io3"]);
        // An Onyx2 has its pipe built in, so no device is added for it.
        let mut onyx2 = o.clone();
        onyx2.product = "onyx2".into();
        onyx2.topology = "onyx2-deskside".into();
        let mut args = vec![];
        add_graphics(&mut args, &onyx2, "infinite-reality").unwrap();
        assert!(args.is_empty());
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
    fn catalogue_overrides_go_to_their_target() {
        let catalog = crate::test_catalogue();
        let o = preset(&catalog, "fuel-1").unwrap();
        let mut f = file(o, "none");
        let option = |args: &[String], name: &str| {
            args.windows(2)
                .find(|p| p[0] == name)
                .map(|p| p[1].clone())
                .unwrap()
        };
        let args =
            runtime::arguments(Path::new("/machine"), &f, o, runtime::Display::None).unwrap();
        assert!(!option(&args, "-M").contains("board-id-word"));
        assert_eq!(option(&args, "-cpu"), o.cpu);
        f.machine.inputs = [
            ("board-id-word", "0x4000"),
            ("l1-revision", "1.2.3"),
            ("r14000-prid", "0xf14"),
            ("fuel-mac-eeprom", "mac.bin"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        let args =
            runtime::arguments(Path::new("/machine"), &f, o, runtime::Display::None).unwrap();
        let machine = option(&args, "-M");
        assert!(machine.contains(",board-id-word=0x4000"), "{machine}");
        assert!(machine.contains(",l1-revision=1.2.3"), "{machine}");
        assert!(machine.contains(",fuel-mac-eeprom=mac.bin"), "{machine}");
        assert!(!machine.contains("r14000-prid"), "{machine}");
        assert_eq!(
            option(&args, "-cpu"),
            format!("{},r14000-prid=0xf14", o.cpu)
        );

        let mut inputs = f.machine.inputs.clone();
        inputs.insert("fuel-board-id-word".into(), "0x4000".into());
        let error = validate_inputs(o, &inputs).unwrap_err().to_string();
        assert!(error.contains("board-id-word, "), "{error}");
        let mac = [("mac".to_string(), "08:00:69:12:34:56".to_string())].into();
        assert!(validate_inputs(o, &mac).is_err());
        let mut required = o.clone();
        required.hardware_inputs.push(crate::InputBinding {
            option: "board-revision".into(),
            required: true,
        });
        let error = validate_inputs(&required, &BTreeMap::new()).unwrap_err();
        assert!(error.to_string().contains("board-revision"), "{error}");

        // Octane takes its flash straps and processor words the same way.
        let octane = preset(&catalog, "octane-impact").unwrap();
        let mut f = file(octane, "si");
        f.machine.inputs = [("flash-select", "on"), ("r10000-prid", "0xe24")]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let args =
            runtime::arguments(Path::new("/machine"), &f, octane, runtime::Display::None).unwrap();
        assert!(option(&args, "-M").contains(",flash-select=on"));
        assert_eq!(option(&args, "-cpu"), "R10000,r10000-prid=0xe24");
        for o in catalog.offerings.iter() {
            assert!(takes_mac(o), "{}", o.topology);
        }
        assert!(validate_graphics(o, "vpro").is_err());
        assert!(preset(&catalog, "origin350-2").is_err());
        assert!(preset(&catalog, "tezro-4").is_err());
    }
}
