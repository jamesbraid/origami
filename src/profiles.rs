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
    /// The graphics product a machine gets unless `create --graphics` names
    /// another; None for the topology's catalogue default, which is no
    /// graphics where it has no default pipe.
    pub graphics: Option<&'static str>,
    /// The slot for that product; None for the catalogue's choice.
    pub slot: Option<&'static str>,
}

const fn profile(
    id: &'static str,
    topology: &'static str,
    population: &'static [u32],
    graphics: Option<&'static str>,
    slot: Option<&'static str>,
) -> Profile {
    Profile {
        id,
        topology,
        population,
        graphics,
        slot,
    }
}

pub const STARTERS: &[Profile] = &[
    profile("origin200-1", "origin200", &[1], None, None),
    profile("origin200-2", "origin200", &[2], None, None),
    profile("origin200-dual", "origin200-dual", &[2, 2], None, None),
    // The two-CPU Origin 200 desktop, set up for a RAD4 install.
    profile("origin200-rad4", "origin200", &[2], Some("rad4"), None),
    profile("origin2000-8", "origin2000-rack", &[2, 2, 2, 2], None, None),
    // io3, where Origin 2000 IMPACT boards were captured and the IRIX
    // graphics disks were installed.
    profile(
        "origin2000-impact",
        "origin2000-deskside",
        &[2],
        Some("si"),
        Some("io3"),
    ),
    profile("origin300-2", "origin300", &[2], None, None),
    profile("origin3200", "origin3200", &[4, 4], None, None),
    profile("origin3400", "origin3400", &[4, 4, 4, 4], None, None),
    profile("origin3900", "origin3900", &[4, 4, 4, 4], None, None),
    profile(
        "origin200-impact",
        "origin200-gigachannel",
        &[1],
        Some("si"),
        None,
    ),
    profile("octane-impact", "octane", &[1], Some("si"), None),
    profile("octane2-impact", "octane2", &[1], Some("si"), None),
    profile("octane2-vpro", "octane2", &[1], None, None),
    profile(
        "onyx2-infinite-reality",
        "onyx2-deskside",
        &[2, 2],
        None,
        None,
    ),
    // One node with its rack graphics module: the default InfiniteReality3.
    profile(
        "onyx2-rack-infinite-reality",
        "onyx2-rack",
        &[1],
        None,
        None,
    ),
    profile("fuel-1", "fuel", &[1], None, None),
    // The captured Onyx 350 module: four CPUs and its VPro at widget 12.
    profile("onyx350-vpro", "onyx350-compat", &[4], Some("v12"), None),
    // Two C-bricks, an I-brick and the V-brick's VPro V12, its only pipe.
    profile("onyx3000-vpro", "onyx3000", &[4, 4], None, None),
    // A four-CPU Tezro tower. Its topology has no default pipe, so the
    // preset names the V12 and the PIC widget it sits on.
    profile(
        "tezro-vpro",
        "tezro-tower-multi",
        &[4],
        Some("v12"),
        Some("widget-12"),
    ),
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

/// The preset named `name`.
pub fn starter(name: &str) -> Option<&'static Profile> {
    STARTERS.iter().find(|profile| profile.id == name)
}

/// The graphics request a preset makes, with `product`, `slot` and
/// `settings` from the command line in place of its own.
pub fn graphics_request<'a>(
    name: &str,
    product: Option<&'a str>,
    slot: Option<&'a str>,
    settings: BTreeMap<String, String>,
) -> crate::graphics::Request<'a> {
    let profile = starter(name);
    let product = product.or(profile.and_then(|p| p.graphics));
    // The preset's slot goes with any product, and is overridden by a slot
    // given; a machine without graphics has none.
    let slot = slot.or_else(|| {
        profile
            .and_then(|p| p.slot)
            .filter(|_| product != Some("none"))
    });
    crate::graphics::Request {
        product,
        slot,
        settings,
    }
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

/// Whether the machine takes its keyboard and mouse over USB alone, so a
/// launch has to attach them. The machine list names no input devices, so
/// this keys on the product: an Origin 3000's I-brick has USB ports and no
/// PS/2 controller, while the other products model their PS/2 ports.
pub fn usb_input_only(offering: &Offering) -> bool {
    offering.product == "origin3000"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{preset, runtime, MachineFile, Network};
    use std::path::Path;

    fn file(o: &Offering) -> MachineFile {
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
                console: None,
            },
            identity: None,
            network: Network::default(),
            pipe: vec![],
            drive: vec![],
            qemu: Default::default(),
        }
    }

    #[test]
    fn octane_uses_embedded_network_and_no_population() {
        let machines = crate::test_machines();
        let o = preset(&machines, "octane-impact").unwrap();
        let args = runtime::arguments(
            &machines,
            Path::new("/machine"),
            &file(o),
            o,
            runtime::Display::Vnc { port: 5901 },
        )
        .unwrap();
        let machine = &args.windows(2).find(|p| p[0] == "-M").unwrap()[1];
        assert!(
            machine.starts_with("octane,topology=octane,nodes=1,"),
            "{machine}"
        );
        assert!(!machine.contains("population="), "{machine}");
        assert!(args.contains(&"nic,model=sgi-ioc3-eth,netdev=net0".into()));
    }

    #[test]
    fn heterogeneous_cpu_populations_are_explicit() {
        let machines = crate::test_machines();
        for o in machines.offerings.iter() {
            let f = file(o);
            let args = runtime::arguments(
                &machines,
                Path::new("/machine"),
                &f,
                o,
                runtime::Display::None,
            )
            .unwrap();
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
        let mut f = file(o);
        let option = |args: &[String], name: &str| {
            args.windows(2)
                .find(|p| p[0] == name)
                .map(|p| p[1].clone())
                .unwrap()
        };
        let args = runtime::arguments(
            &machines,
            Path::new("/machine"),
            &f,
            o,
            runtime::Display::None,
        )
        .unwrap();
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
        let args = runtime::arguments(
            &machines,
            Path::new("/machine"),
            &f,
            o,
            runtime::Display::None,
        )
        .unwrap();
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
        let mut f = file(octane);
        f.machine.inputs = [("flash-select", "on"), ("r10000-prid", "0xe24")]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let args = runtime::arguments(
            &machines,
            Path::new("/machine"),
            &f,
            octane,
            runtime::Display::None,
        )
        .unwrap();
        assert!(option(&args, "-M").contains(",flash-select=on"));
        assert_eq!(option(&args, "-cpu"), "R10000,r10000-prid=0xe24");
        for o in machines.offerings.iter() {
            assert!(takes_mac(o), "{}", o.topology);
        }
        assert!(preset(&machines, "origin350-2").is_err());
        assert!(preset(&machines, "tezro-4").is_err());
    }

    #[test]
    fn origin200_rad4_is_origin200_2_with_rad4_graphics() {
        let (rad4, headless) = (
            starter("origin200-rad4").unwrap(),
            starter("origin200-2").unwrap(),
        );
        assert_eq!(rad4.topology, headless.topology);
        assert_eq!(rad4.population, headless.population);
        assert_eq!(rad4.graphics, Some("rad4"));
        for name in ["origin200-1", "origin200-2", "origin200-dual"] {
            assert_eq!(starter(name).unwrap().graphics, None, "{name}");
        }
    }
}
