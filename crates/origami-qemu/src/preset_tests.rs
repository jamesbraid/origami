//! Launch arguments for the presets, which need the catalogue's engine data.

use crate::runtime;
use crate::test_machines;
use origami_machine::{preset, Machine, MachineFile, Network, Offering};
use std::collections::BTreeMap;
use std::path::Path;

fn file(o: &Offering) -> MachineFile {
    MachineFile {
        format: origami_machine::MACHINE_FORMAT,
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
        qemu: Default::default(),
    }
}

#[test]
fn octane_uses_embedded_network_and_no_population() {
    let machines = test_machines();
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
    let machines = test_machines();
    for o in machines.offerings.iter() {
        let mut f = file(o);
        // The O2 requires its board inputs.
        for setting in o
            .settings
            .iter()
            .filter(|s| s.required && s.default.is_none())
        {
            let definition = machines
                .setting_definitions
                .iter()
                .find(|d| d.name == setting.name)
                .unwrap();
            f.machine
                .inputs
                .insert(setting.name.clone(), definition.values[0].clone());
        }
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
            .any(|p| p[0] == "-smp" && p[1] == o.cpus.to_string()));
        // The O2 and Octanes have no node count to populate.
        if !matches!(o.product.as_str(), "o2" | "octane" | "octane2") {
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
fn settings_go_to_their_target() {
    let machines = test_machines();
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("mac.bin");
    std::fs::write(&record, [0u8; 512]).unwrap();
    let record = record.display().to_string();
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
    assert_eq!(option(&args, "-cpu"), crate::offering_data(o).unwrap().cpu);
    f.machine.inputs = [
        ("board-id-word", "0x4000"),
        ("l1-revision", "1.2.3"),
        ("prid", "0xf14"),
        ("fuel-mac-eeprom", record.as_str()),
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
    assert!(
        machine.contains(&format!(",fuel-mac-eeprom={record}")),
        "{machine}"
    );
    assert!(!machine.contains("prid"), "{machine}");
    assert_eq!(
        option(&args, "-cpu"),
        format!("{},r14000-prid=0xf14", crate::offering_data(o).unwrap().cpu)
    );

    // Octane takes its flash straps and processor words the same way.
    let octane = preset(&machines, "octane-impact").unwrap();
    let mut f = file(octane);
    f.machine.inputs = [("flash-select", "on"), ("prid", "0xe24")]
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
    assert!(preset(&machines, "origin350-2").is_err());
    assert!(preset(&machines, "tezro-4").is_err());
}
