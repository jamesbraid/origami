use crate::{Catalog, Machine, Offering, Result};
use std::collections::BTreeMap;

pub fn legacy_topology(model: &str, nodes: u32) -> &str {
    match (model, nodes) {
        ("origin200", 2) => "origin200-dual",
        ("origin2000", _) => "origin2000-rack",
        _ => model,
    }
}

pub fn population(machine: &Machine) -> Vec<u32> {
    if machine.population.is_empty() {
        vec![machine.cpus_per_node; machine.nodes as usize]
    } else {
        machine.population.clone()
    }
}

pub fn presets(catalog: &Catalog) -> Vec<(String, &Offering)> {
    const STARTERS: &[(&str, &str, &[u32])] = &[
        ("origin200-1", "origin200", &[1]),
        ("origin200-2", "origin200", &[2]),
        ("origin200-dual", "origin200-dual", &[2, 2]),
        ("origin2000-8", "origin2000-rack", &[2, 2, 2, 2]),
        ("origin300-2", "origin300", &[2]),
        ("origin200-impact", "origin200-gigachannel", &[1]),
        ("octane-impact", "octane", &[1]),
        ("octane2-impact", "octane2", &[1]),
        ("onyx2-infinite-reality", "onyx2-deskside", &[2, 2]),
        ("fuel-1", "fuel", &[1]),
        ("origin300-v12-direct-2", "origin300-v12-direct", &[2]),
        ("origin300-vbrick-2", "origin300-vbrick", &[2]),
    ];
    STARTERS
        .iter()
        .filter_map(|(name, topology, population)| {
            catalog
                .offerings
                .iter()
                .find(|o| o.topology == *topology && o.cpus_per_node == *population)
                .map(|o| ((*name).to_string(), o))
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
    use crate::{preset, runtime, Firmware, MachineFile, Network};
    use std::path::Path;

    fn file(o: &Offering, graphics: &str) -> MachineFile {
        MachineFile {
            format: 1,
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
            firmware: Firmware {
                image: "firmware/prom.bin".into(),
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
        assert!(args
            .windows(2)
            .any(|p| p == ["-M", "octane,graphics-board=si"]));
        assert!(args.contains(&"nic,model=sgi-ioc3-eth,netdev=net0".into()));
        assert!(validate_graphics(o, "vpro").is_err());
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
