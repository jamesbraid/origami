use crate::{Machine, Machines, Offering, ProcessorOption, Result};
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
    /// The graphics a machine gets unless `create --graphics` names others.
    pub graphics: &'static str,
}

pub const STARTERS: &[Profile] = &[
    Profile {
        id: "origin200-1",
        topology: "origin200",
        population: &[1],
        graphics: "none",
    },
    Profile {
        id: "origin200-2",
        topology: "origin200",
        population: &[2],
        graphics: "none",
    },
    Profile {
        id: "origin200-dual",
        topology: "origin200-dual",
        population: &[2, 2],
        graphics: "none",
    },
    // The two-CPU Origin 200 desktop, set up for a RAD4 install.
    Profile {
        id: "origin200-rad4",
        topology: "origin200",
        population: &[2],
        graphics: "rad4",
    },
    Profile {
        id: "origin2000-8",
        topology: "origin2000-rack",
        population: &[2, 2, 2, 2],
        graphics: "none",
    },
    Profile {
        id: "origin300-2",
        topology: "origin300",
        population: &[2],
        graphics: "none",
    },
    Profile {
        id: "origin200-impact",
        topology: "origin200-gigachannel",
        population: &[1],
        graphics: "si",
    },
    Profile {
        id: "octane-impact",
        topology: "octane",
        population: &[1],
        graphics: "si",
    },
    Profile {
        id: "octane2-impact",
        topology: "octane2",
        population: &[1],
        graphics: "si",
    },
    Profile {
        id: "onyx2-infinite-reality",
        topology: "onyx2-deskside",
        population: &[2, 2],
        graphics: "infinite-reality",
    },
    Profile {
        id: "fuel-1",
        topology: "fuel",
        population: &[1],
        graphics: "none",
    },
    Profile {
        id: "origin300-v12-direct-2",
        topology: "origin300-v12-direct",
        population: &[2],
        graphics: "vpro",
    },
    Profile {
        id: "origin300-vbrick-2",
        topology: "origin300-vbrick",
        population: &[2],
        graphics: "vpro",
    },
];

pub fn presets(machines: &Machines) -> Vec<(String, &Offering)> {
    STARTERS
        .iter()
        .filter_map(|profile| {
            machines
                .offerings
                .iter()
                .find(|o| {
                    o.topology == profile.topology
                        && o.cpus_per_node == profile.population
                        && fits_default_processor(machines, o)
                })
                .map(|o| (profile.id.to_string(), o))
        })
        .collect()
}

/// The processor modules `topology` offers, its default first; empty where
/// it offers no choice.
pub fn processor_options<'a>(machines: &'a Machines, topology: &str) -> &'a [ProcessorOption] {
    machines
        .topologies
        .iter()
        .find(|t| t.name == topology)
        .map_or(&[], |t| &t.processor_options)
}

pub fn processor_option<'a>(
    machines: &'a Machines,
    topology: &str,
    id: &str,
) -> Result<&'a ProcessorOption> {
    let options = processor_options(machines, topology);
    if options.is_empty() {
        return Err(format!("{topology} offers no processor choice").into());
    }
    options
        .iter()
        .find(|option| option.assembly.id == id)
        .ok_or_else(|| {
            let ids: Vec<_> = options.iter().map(|o| o.assembly.id.as_str()).collect();
            format!(
                "{topology} has no processor module {id}; choose {}",
                ids.join(", ")
            )
            .into()
        })
}

/// Whether `offering` fits its topology's default module, as every
/// offering of a topology without a choice does.
pub fn fits_default_processor(machines: &Machines, offering: &Offering) -> bool {
    processor_options(machines, &offering.topology)
        .first()
        .is_none_or(|default| default.assembly.id == offering.processor.id)
}

/// Whether `offering` selects a module other than its topology's default.
pub fn selects_processor(offering: &Offering) -> bool {
    offering
        .machine_options
        .split(',')
        .any(|option| option.starts_with("processor="))
}

/// The offering like `offering` but fitting module `id`. A module that
/// holds a fixed number of CPUs sets each node's count.
pub fn with_processor<'a>(
    machines: &'a Machines,
    offering: &Offering,
    id: &str,
) -> Result<&'a Offering> {
    let option = processor_option(machines, &offering.topology, id)?;
    let mut population = offering.cpus_per_node.clone();
    if !population
        .iter()
        .all(|count| option.cpus_per_node.contains(count))
    {
        match option.cpus_per_node[..] {
            [count] => population.fill(count),
            _ => {
                return Err(format!("{id} does not fit {population:?} CPUs per node").into());
            }
        }
    }
    machines
        .offerings
        .iter()
        .find(|o| {
            o.topology == offering.topology && o.cpus_per_node == population && o.processor.id == id
        })
        .ok_or_else(|| {
            format!(
                "{} does not offer {id} with {population:?} CPUs per node",
                offering.topology
            )
            .into()
        })
}

/// The processor choices `origami machines` lists: for each set of presets
/// offering the same modules, those presets and a line per module.
pub fn processor_choices(machines: &Machines) -> Vec<(Vec<String>, Vec<String>)> {
    let mut groups: Vec<(Vec<String>, Vec<String>)> = vec![];
    for (name, offering) in presets(machines) {
        let options = processor_options(machines, &offering.topology);
        let lines: Vec<String> = options
            .iter()
            .enumerate()
            .filter_map(|(index, option)| {
                let processor = &machines
                    .offerings
                    .iter()
                    .find(|o| {
                        o.topology == offering.topology && o.processor.id == option.assembly.id
                    })?
                    .processor;
                let counts: Vec<_> = option.cpus_per_node.iter().map(u32::to_string).collect();
                Some(format!(
                    "{:20} {processor}, {} CPUs per node, up to {} MiB per node{}",
                    option.assembly.id,
                    counts.join(" or "),
                    option.accepted_memory.iter().max().unwrap_or(&0),
                    if index == 0 { " (default)" } else { "" }
                ))
            })
            .collect();
        if lines.is_empty() {
            continue;
        }
        match groups.iter_mut().find(|(_, group)| *group == lines) {
            Some((names, _)) => names.push(name),
            None => groups.push((vec![name], lines)),
        }
    }
    groups
}

/// The graphics a preset selects, or None for an unknown preset.
pub fn preset_graphics(name: &str) -> Option<&'static str> {
    STARTERS
        .iter()
        .find(|profile| profile.id == name)
        .map(|profile| profile.graphics)
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
/// machine list's overrides name the values a launch may replace, and its
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
                processor: None,
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
        let machines = crate::test_machines();
        let o = preset(&machines, "origin2000-8").unwrap();
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
        let machines = crate::test_machines();
        let o = preset(&machines, "octane-impact").unwrap();
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
        let machines = crate::test_machines();
        for o in machines.offerings.iter() {
            let f = file(o, graphics(o)[0]);
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
    fn listed_overrides_go_to_their_target() {
        let machines = crate::test_machines();
        let o = preset(&machines, "fuel-1").unwrap();
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
        let octane = preset(&machines, "octane-impact").unwrap();
        let mut f = file(octane, "si");
        f.machine.inputs = [("flash-select", "on"), ("r10000-prid", "0xe24")]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let args =
            runtime::arguments(Path::new("/machine"), &f, octane, runtime::Display::None).unwrap();
        assert!(option(&args, "-M").contains(",flash-select=on"));
        assert_eq!(option(&args, "-cpu"), "R10000,r10000-prid=0xe24");
        for o in machines.offerings.iter() {
            assert!(takes_mac(o), "{}", o.topology);
        }
        assert!(validate_graphics(o, "vpro").is_err());
        assert!(preset(&machines, "origin350-2").is_err());
        assert!(preset(&machines, "tezro-4").is_err());
    }

    #[test]
    fn origin200_rad4_is_origin200_2_with_rad4_graphics() {
        let starter = |id| STARTERS.iter().find(|p| p.id == id).unwrap();
        let (rad4, headless) = (starter("origin200-rad4"), starter("origin200-2"));
        assert_eq!(rad4.topology, headless.topology);
        assert_eq!(rad4.population, headless.population);
        assert_eq!(preset_graphics("origin200-rad4"), Some("rad4"));
        for name in ["origin200-1", "origin200-2", "origin200-dual"] {
            assert_eq!(preset_graphics(name), Some("none"), "{name}");
        }
        let machines = crate::test_machines();
        let origin200 = preset(&machines, "origin200-1").unwrap();
        assert!(validate_graphics(origin200, "rad4").is_ok());
    }
}
