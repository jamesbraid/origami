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
    /// The machine list's id of the offering the preset creates.
    pub offering: &'static str,
    /// The graphics product a machine gets unless `create --graphics` names
    /// another; None for the topology's catalogue default, which is no
    /// graphics where it has no default pipe.
    pub graphics: Option<&'static str>,
    /// The slot for that product; None for the catalogue's choice.
    pub slot: Option<&'static str>,
    /// Machine inputs the preset sets unless `create --set` gives them. They
    /// fill inputs the machine list marks required and gives no default.
    pub settings: &'static [(&'static str, &'static str)],
}

const fn profile(
    id: &'static str,
    offering: &'static str,
    graphics: Option<&'static str>,
    slot: Option<&'static str>,
) -> Profile {
    Profile {
        id,
        offering,
        graphics,
        slot,
        settings: &[],
    }
}

/// `profile` with machine inputs of its own.
const fn with_settings(
    profile: Profile,
    settings: &'static [(&'static str, &'static str)],
) -> Profile {
    Profile {
        settings,
        ..profile
    }
}

/// The O2 board inputs the machine list requires and leaves open, because
/// the hardware's values are unknown. These are the values an O2 was booted
/// to IRIX with, not measurements: the password clear input at 1, and the ROM
/// part unprotected with both boot blocks unlocked.
const O2_BOARD: &[(&str, &str)] = &[
    ("password-clear-input", "1"),
    ("rom-at29c040a-protection", "disabled"),
    ("rom-at29c040a-lower-lock", "unlocked"),
    ("rom-at29c040a-upper-lock", "unlocked"),
];

pub const STARTERS: &[Profile] = &[
    // Only iris runs the Indy and the Indigo2; its Octane2 has its own id.
    profile("indy", "indy.1", None, None),
    profile("indigo2", "indigo2.1", None, None),
    profile("octane2-iris", "octane2.1.iris", None, None),
    profile("origin200-1", "origin200.1.r10000-180mhz-1mb", None, None),
    profile("origin200-2", "origin200.2.r10000-180mhz-1mb", None, None),
    profile(
        "origin200-dual",
        "origin200-dual.2-2.r10000-180mhz-1mb",
        None,
        None,
    ),
    // The two-CPU Origin 200 desktop, set up for a RAD4 install.
    profile(
        "origin200-rad4",
        "origin200.2.r10000-180mhz-1mb",
        Some("rad4"),
        None,
    ),
    profile("origin2000-8", "origin2000-rack.2-2-2-2", None, None),
    // io3, where Origin 2000 IMPACT boards were captured and the IRIX
    // graphics disks were installed.
    profile(
        "origin2000-impact",
        "origin2000-deskside.2",
        Some("si"),
        Some("io3"),
    ),
    profile("origin300-2", "origin300.2", None, None),
    profile("origin3200", "origin3200.4-4.r12000-400mhz-8mb", None, None),
    profile(
        "origin3400",
        "origin3400.4-4-4-4.r12000-400mhz-8mb",
        None,
        None,
    ),
    profile("origin3900", "origin3900-ip53.4-4-4-4", None, None),
    profile(
        "origin200-impact",
        "origin200-gigachannel.1.r10000-180mhz-1mb",
        Some("si"),
        None,
    ),
    profile("octane-impact", "octane.1", Some("si"), None),
    profile("octane2-impact", "octane2.1", Some("si"), None),
    profile("octane2-vpro", "octane2.1", None, None),
    profile("onyx2-infinite-reality", "onyx2-deskside.2-2", None, None),
    // One node with its rack graphics module: the default InfiniteReality3.
    profile("onyx2-rack-infinite-reality", "onyx2-rack.1", None, None),
    profile("fuel-1", "fuel.1", None, None),
    // The R10000 O2. The catalogue's R12000 O2+ is a separate offering.
    with_settings(profile("o2", "o2.1", None, None), O2_BOARD),
    // The captured Onyx 350 module: four CPUs and its VPro at widget 12.
    profile("onyx350", "onyx350-ip53.4", Some("v12"), None),
    // Two C-bricks, an I-brick and the V-brick's VPro V12, its only pipe.
    profile(
        "onyx3000-vpro",
        "onyx3000.4-4.r12000-400mhz-8mb",
        None,
        None,
    ),
    // Four C-bricks and the fitted G-brick pipe's InfiniteReality3, on the
    // DG5-2 display generator. The topology fits one pipe of its four cabled.
    profile(
        "onyx3400-infinite-reality",
        "onyx3400-ir.4-4-4-4",
        None,
        None,
    ),
    // A four-CPU Tezro tower. Its topology has no default pipe, so the
    // preset names the V12 and the PIC widget it sits on.
    profile(
        "tezro",
        "tezro-ip53.4.r16000-700mhz-4mb",
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
                .find(|o| o.id == profile.offering)
                .map(|o| (profile.id.to_string(), o))
        })
        .collect()
}

/// The preset a machine of `offering_id` with `pipes` was made from: the one
/// whose graphics product is fitted, else the one that takes the default.
pub fn preset_name(offering_id: &str, pipes: &[crate::graphics::Pipe]) -> Option<&'static str> {
    let of = || STARTERS.iter().filter(|p| p.offering == offering_id);
    of().find(|p| {
        p.graphics
            .is_some_and(|g| pipes.iter().any(|f| f.product == g))
    })
    .or_else(|| of().find(|p| p.graphics.is_none()))
    .map(|p| p.id)
}

/// An Origin 200 starts with this much memory per node, not the 64 MiB the
/// catalogue lists first: IRIX 6.5 on 64 MiB has little left for a user.
const ORIGIN200_MEMORY_MIB: u32 = 256;

/// MiB per node a new machine from a preset with `offering` gets unless
/// `create --memory-per-node` says otherwise.
pub fn default_memory(offering: &Offering) -> u32 {
    if offering.product == "origin200" {
        ORIGIN200_MEMORY_MIB
    } else {
        offering.memory.default
    }
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
    offering.selects_processor
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

/// The offering like `offering` but with `nodes` nodes, each holding the
/// CPUs the first node of `offering` holds.
pub fn with_nodes<'a>(
    machines: &'a Machines,
    offering: &Offering,
    nodes: u32,
) -> Result<&'a Offering> {
    let population = vec![offering.cpus_per_node[0]; nodes as usize];
    machines
        .offerings
        .iter()
        .find(|o| {
            o.topology == offering.topology
                && o.cpus_per_node == population
                && o.processor.id == offering.processor.id
        })
        .ok_or_else(|| {
            format!(
                "{} does not offer {nodes} nodes of {} CPUs",
                offering.topology, offering.cpus_per_node[0]
            )
            .into()
        })
}

/// The preset named `name`.
pub fn starter(name: &str) -> Option<&'static Profile> {
    STARTERS.iter().find(|profile| profile.id == name)
}

/// Whether a machine made from `offering` (preset `name`, if it has one) has a
/// graphics pipe by default; false for one that is serial only by design. A
/// catalogue that cannot say is an error, not a headless machine.
pub fn has_default_display(
    machines: &crate::Machines,
    name: &str,
    offering: &crate::Offering,
) -> crate::Result<bool> {
    let request = graphics_request(name, None, None, Default::default(), None);
    crate::graphics::select(machines, &offering.topology, offering.nodes, &request)
        .map(|pipes| !pipes.is_empty())
}

/// The graphics request a preset makes, with `product`, `slot` and
/// `settings` from the command line in place of its own.
pub fn graphics_request<'a>(
    name: &str,
    product: Option<&'a str>,
    slot: Option<&'a str>,
    settings: BTreeMap<String, String>,
    pipes: Option<u32>,
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
        pipes,
    }
}

/// `given` with the inputs preset `name` sets under them: `create --set` wins.
pub fn preset_inputs(name: &str, mut given: BTreeMap<String, String>) -> BTreeMap<String, String> {
    for (setting, value) in starter(name).map_or(&[][..], |profile| profile.settings) {
        given
            .entry((*setting).to_string())
            .or_insert_with(|| (*value).to_string());
    }
    given
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
    use crate::{preset, MachineFile, Network};

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
            share: vec![],
            firmware: None,
            firstboot: None,
            engine: crate::model::EngineChoice::qemu(&[]),
        }
    }

    #[test]
    fn presets_name_offerings_on_the_ip53_boards() {
        let machines = crate::test_machines();
        for (name, id) in [
            ("tezro", "tezro-ip53.4.r16000-700mhz-4mb"),
            ("onyx350", "onyx350-ip53.4"),
            ("origin3900", "origin3900-ip53.4-4-4-4"),
        ] {
            assert_eq!(preset(&machines, name).unwrap().id, id, "{name}");
        }
        // Every preset names an offering the machine list has, except the
        // ones only iris offers, and no preset fits an IP59 board or the
        // Onyx 4.
        let iris_only = STARTERS
            .iter()
            .filter(|p| matches!(p.offering, "indy.1" | "indigo2.1" | "octane2.1.iris"))
            .count();
        assert_eq!(presets(&machines).len(), STARTERS.len() - iris_only);
        for profile in STARTERS {
            assert!(
                !profile.offering.contains("ip59") && !profile.offering.starts_with("onyx4"),
                "{}",
                profile.id
            );
        }
        // A machine finds its offering by what it stores, and the IP59 Tezro
        // is a topology of its own.
        let tezro = preset(&machines, "tezro").unwrap();
        let mut f = file(tezro);
        assert_eq!(crate::offering(&machines, &f.machine).unwrap().id, tezro.id);
        f.machine.topology = Some("tezro-ip59".into());
        let on59 = crate::offering(&machines, &f.machine).unwrap();
        assert_eq!(on59.id, "tezro-ip59.4");
        assert_eq!(on59.processor.id, "r16000-1000mhz-16mb");
        // The renamed topology is told where it went.
        f.machine.topology = Some("tezro".into());
        let error = crate::offering(&machines, &f.machine)
            .unwrap_err()
            .to_string();
        assert!(error.contains("tezro-ip53"), "{error}");
    }

    #[test]
    fn a_serial_only_preset_has_no_default_display_and_others_have_one() {
        let machines = crate::test_machines();
        for (name, display) in [
            ("origin200-1", false),
            ("origin200-rad4", true),
            ("fuel-1", true),
        ] {
            let offering = preset(&machines, name).unwrap();
            assert_eq!(
                has_default_display(&machines, name, offering).unwrap(),
                display,
                "{name}"
            );
        }
        // Every offering the catalogue lists can say, with or without a preset.
        for offering in &machines.offerings {
            let preset = presets(&machines)
                .into_iter()
                .find(|(_, o)| o.id == offering.id)
                .map_or(String::new(), |(name, _)| name);
            has_default_display(&machines, &preset, offering)
                .unwrap_or_else(|e| panic!("{}: {e}", offering.id));
        }
        assert_eq!(crate::listing::machine_name("origin200"), "Origin 200");
        assert_eq!(crate::listing::machine_name("unheard-of"), "unheard-of");
    }

    #[test]
    fn a_preset_sets_inputs_that_create_set_overrides() {
        let given = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>()
        };
        let plain = preset_inputs("o2", BTreeMap::new());
        assert_eq!(plain["password-clear-input"], "1");
        assert_eq!(plain["rom-at29c040a-protection"], "disabled");
        assert_eq!(plain.len(), 4);
        let overridden = preset_inputs("o2", given(&[("password-clear-input", "0"), ("x", "y")]));
        assert_eq!(overridden["password-clear-input"], "0");
        assert_eq!(overridden["x"], "y");
        assert_eq!(overridden.len(), 5);
        // A preset with no inputs adds none.
        assert!(preset_inputs("fuel-1", BTreeMap::new()).is_empty());
        // The inputs satisfy what the machine list requires of the O2.
        let machines = crate::test_machines();
        let offering = preset(&machines, "o2").unwrap();
        crate::settings::validate(&machines, offering, &plain).unwrap();
    }

    #[test]
    fn origin200_rad4_is_origin200_2_with_rad4_graphics() {
        let (rad4, headless) = (
            starter("origin200-rad4").unwrap(),
            starter("origin200-2").unwrap(),
        );
        assert_eq!(rad4.offering, headless.offering);
        assert_eq!(rad4.graphics, Some("rad4"));
        for name in ["origin200-1", "origin200-2", "origin200-dual"] {
            assert_eq!(starter(name).unwrap().graphics, None, "{name}");
        }
    }

    #[test]
    fn origin200_presets_start_with_256_mib() {
        let machines = crate::test_machines();
        for name in [
            "origin200-1",
            "origin200-2",
            "origin200-dual",
            "origin200-rad4",
            "origin200-impact",
        ] {
            let offering = preset(&machines, name).unwrap();
            assert_eq!(default_memory(offering), 256, "{name}");
            assert!(offering.memory.accepted.contains(&256), "{name}");
        }
        // Other machines keep the catalogue's default.
        for name in ["origin2000-8", "octane2-vpro", "fuel-1"] {
            let offering = preset(&machines, name).unwrap();
            assert_eq!(default_memory(offering), offering.memory.default, "{name}");
        }
    }
}
