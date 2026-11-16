//! Creating a machine from a [`MachineSpec`].

use crate::change::apply_changes;
use crate::spec::MachineSpec;
use crate::{Change, Report};
use origami_engine::{Engine, EngineStores};
use origami_machine::{
    graphics, preset, profiles, validate_create_inputs, Identity, MachineInit, Result,
};
use std::fs;

/// Create the machine `spec` describes. Validates the whole request before
/// it downloads firmware or writes anything, and leaves no directory behind
/// when it fails.
pub fn create(engine: &dyn Engine, spec: &MachineSpec) -> Result<Report> {
    let machines = engine.catalogue()?;
    let machines = &*machines;
    let preset_offer = preset(machines, &spec.preset)?;
    let offer = match &spec.processor {
        Some(processor) => profiles::with_processor(machines, preset_offer, processor)?,
        None => preset_offer,
    };
    let request = profiles::graphics_request(
        &spec.preset,
        spec.graphics.product.as_deref(),
        spec.graphics.slot.as_deref(),
        spec.graphics.settings.clone(),
        spec.graphics.pipes,
    );
    // A pipe may sit on a node board the preset does not have, as the second
    // pipe of an Onyx2 rack does. That machine gets the nodes the pipes need,
    // with the CPUs per node the preset has.
    let for_pipes = graphics::record(machines, &offer.topology)
        .zip(spec.graphics.pipes)
        .map(|(record, pipes)| graphics::nodes_for_pipes(record, pipes))
        .filter(|nodes| *nodes > offer.nodes);
    let wanted = for_pipes.into_iter().chain(spec.nodes).max();
    let (offer, nodes_added) = match wanted {
        Some(nodes) if nodes > offer.nodes => (
            profiles::with_nodes(machines, offer, nodes)?,
            for_pipes.is_some(),
        ),
        _ => (offer, false),
    };
    let pipes = graphics::select(machines, &offer.topology, offer.nodes, &request)?;
    let pipe_count = pipes.len();
    let inputs = origami_machine::settings::resolve(
        machines,
        offer,
        profiles::preset_inputs(&spec.preset, spec.settings.clone()),
    )?;
    validate_create_inputs(
        &spec.dir,
        offer,
        spec.memory_per_node_mib,
        spec.identity.mac.as_deref(),
    )?;
    if let Some(emu_id) = spec.identity.emu_id {
        origami_machine::emu_id::check_range(emu_id)?;
    }
    // Check the engine's tools before any download that would consume them.
    engine.preflight_create()?;
    let firmware = engine.firmware(
        offer,
        spec.firmware.boot_prom.as_deref(),
        spec.firmware.io_prom.as_deref(),
    )?;
    // Held until machine.toml is written, so a second create rescans the
    // siblings only after this emu-id is on disk.
    let emu_ids = origami_machine::emu_id::lock(&spec.dir)?;
    let emu_id = origami_machine::emu_id::assign(
        machines,
        &spec.dir,
        offer,
        spec.identity.emu_id,
        spec.identity.mac.is_none(),
    )?;
    let mac = origami_machine::mac::assign(
        machines,
        &spec.dir,
        offer,
        spec.identity.mac.as_deref(),
        Some(emu_id),
    )?;
    origami_machine::create_configured(
        &spec.dir,
        offer,
        &MachineInit {
            stores: &EngineStores(engine),
            boot_prom: &firmware.boot_prom,
            io_prom: firmware.io_prom.as_deref(),
        },
        spec.memory_per_node_mib,
        mac.map(|mac| Identity::new(&mac, Some(emu_id))),
        pipes,
        inputs,
    )?;
    drop(emu_ids);
    let mut changes: Vec<Change> = spec
        .network
        .iter()
        .cloned()
        .map(Change::SetNetwork)
        .collect();
    changes.extend(spec.drives.iter().cloned().map(Change::AttachDrive));
    if !changes.is_empty() {
        if let Err(error) = apply_changes(engine, &spec.dir, &changes) {
            let _ = fs::remove_dir_all(&spec.dir);
            return Err(error);
        }
    }
    let mut report = Report::default();
    report.result(format!("created {}", spec.dir.display()));
    if nodes_added {
        report.detail(format!(
            "{pipe_count} graphics pipes need {} nodes, so the machine has {} nodes",
            offer.nodes, offer.nodes
        ));
    }
    if offer.cpus_per_node != preset_offer.cpus_per_node {
        report.detail(format!(
            "{} holds {} CPUs, so each node has {}",
            offer.processor.id, offer.cpus_per_node[0], offer.cpus_per_node[0]
        ));
    }
    Ok(report)
}
