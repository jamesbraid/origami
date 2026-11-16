use crate::cli::{CreateArgs, GraphicsArgs};
use origami_engine::{Engine, EngineStores};
use origami_machine::dir::MachineDir;
use origami_machine::model::Machine;
use origami_machine::{control, graphics};
use origami_machine::{
    preset, read_machine, validate_create_inputs, Drive, MachineFile, MachineInit, Machines,
    Offering, Result,
};
use origami_qemu::{runtime, QemuEngine};
use std::fs;
use std::path::Path;
use std::sync::Arc;

/// QEMU's machine list, through the engine interface.
pub fn machines(rt: &runtime::Runtime) -> Result<Arc<Machines>> {
    QemuEngine::new(rt.clone()).catalogue()
}

/// Check `file` through the engine interface and return its offering.
pub fn validate<'a>(
    rt: &runtime::Runtime,
    machines: &'a Arc<Machines>,
    dir: &Path,
    file: &MachineFile,
) -> Result<&'a Offering> {
    let engine = QemuEngine::with_catalogue(rt.clone(), machines.clone());
    let machine = Machine::from_file(machines, file)?;
    engine
        .validate(&machine, &MachineDir::new(dir))
        .into_result()?;
    origami_machine::offering(machines, &file.machine)
}

pub fn create_machine(
    rt: &runtime::Runtime,
    machines: &Arc<Machines>,
    args: &CreateArgs,
) -> Result<()> {
    let options = &args.options;
    let preset_offer = preset(machines, &args.preset)?;
    let offer = match &options.processor {
        Some(processor) => {
            origami_machine::profiles::with_processor(machines, preset_offer, processor)?
        }
        None => preset_offer,
    };
    let request = origami_machine::profiles::graphics_request(
        &args.preset,
        options.graphics.graphics.as_deref(),
        options.graphics.graphics_slot.as_deref(),
        options.graphics.settings()?,
        options.graphics.graphics_pipes,
    );
    // A pipe may sit on a node board the preset does not have, as the second
    // pipe of an Onyx2 rack does. That machine gets the nodes the pipes need,
    // with the CPUs per node the preset has.
    let (offer, nodes_added) = match graphics::record(machines, &offer.topology)
        .zip(options.graphics.graphics_pipes)
        .map(|(record, pipes)| graphics::nodes_for_pipes(record, pipes))
    {
        Some(nodes) if nodes > offer.nodes => (
            origami_machine::profiles::with_nodes(machines, offer, nodes)?,
            true,
        ),
        _ => (offer, false),
    };
    let pipes = graphics::select(machines, &offer.topology, offer.nodes, &request)?;
    let pipe_count = pipes.len();
    let inputs = origami_machine::settings::resolve(
        machines,
        offer,
        origami_machine::profiles::preset_inputs(&args.preset, options.hardware.inputs()?),
    )?;
    validate_create_inputs(
        &args.dir,
        offer,
        options.memory_per_node,
        options.mac.as_deref(),
    )?;
    if let Some(emu_id) = options.emu_id {
        origami_machine::emu_id::check_range(emu_id)?;
    }
    let tool = rt.machine_init();
    // Check the tool before any download that it would consume.
    if !tool.is_file() {
        return Err(format!("packaged machine init tool missing: {}", tool.display()).into());
    }
    let engine = QemuEngine::with_catalogue(rt.clone(), machines.clone());
    let firmware = engine.firmware(offer, options.prom.as_deref(), options.io_prom.as_deref())?;
    // Held until machine.toml is written, so a second create rescans the
    // siblings only after this emu-id is on disk.
    let _emu_ids = origami_machine::emu_id::lock(&args.dir)?;
    let emu_id = origami_machine::emu_id::assign(
        machines,
        &args.dir,
        offer,
        options.emu_id,
        options.mac.is_none(),
    )?;
    let mac = origami_machine::mac::assign(
        machines,
        &args.dir,
        offer,
        options.mac.as_deref(),
        Some(emu_id),
    )?;
    origami_machine::create_configured(
        &args.dir,
        offer,
        &MachineInit {
            stores: &EngineStores(&engine),
            boot_prom: &firmware.boot_prom,
            io_prom: firmware.io_prom.as_deref(),
        },
        options.memory_per_node,
        mac.map(|mac| origami_machine::Identity::new(&mac, Some(emu_id))),
        pipes,
        inputs,
    )?;
    println!("created {}", args.dir.display());
    if nodes_added {
        println!(
            "{pipe_count} graphics pipes need {} nodes, so the machine has {} nodes",
            offer.nodes, offer.nodes
        );
    }
    if offer.cpus_per_node != preset_offer.cpus_per_node {
        println!(
            "{} holds {} CPUs, so each node has {}",
            offer.processor.id, offer.cpus_per_node[0], offer.cpus_per_node[0]
        );
    }
    Ok(())
}

/// Change the graphics of the stopped machine in `dir`, where every slot
/// and setting the change touches may change between boots.
pub fn set_graphics(
    rt: &runtime::Runtime,
    machines: &Arc<Machines>,
    dir: &Path,
    args: &GraphicsArgs,
) -> Result<()> {
    let _lock = control::lock_for_edit(dir, "changing its graphics")?;
    let mut file = read_machine(dir)?;
    let offer = validate(rt, machines, dir, &file)?;
    if args.graphics.is_none() {
        return Err("name the graphics with --graphics PRODUCT or --graphics none".into());
    }
    let mut request = graphics::Request {
        product: args.graphics.as_deref(),
        slot: args.graphics_slot.as_deref(),
        settings: args.settings()?,
        pipes: args.graphics_pipes,
    };
    // Changing a setting of the pipe it has keeps that pipe's other values.
    if let Some(current) = file.pipe.iter().find(|pipe| {
        request.product == Some(pipe.product.as_str())
            && request.slot.is_none_or(|slot| slot == pipe.slot)
    }) {
        let mut settings = current.settings.clone();
        settings.append(&mut request.settings);
        request.settings = settings;
        request.slot = Some(&current.slot);
    }
    let pipes = graphics::select(machines, &offer.topology, offer.nodes, &request)?;
    if pipes == file.pipe {
        println!("graphics unchanged");
        return Ok(());
    }
    graphics::check_change(machines, &offer.topology, &file.pipe, &pipes)?;
    file.pipe = pipes;
    validate(rt, machines, dir, &file)?;
    fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
    println!("graphics changed; the guest sees them at its next boot");
    Ok(())
}

pub fn create_disk(
    rt: &runtime::Runtime,
    machines: &Arc<Machines>,
    dir: &Path,
    size: u64,
) -> Result<()> {
    let _lock = control::lock_for_edit(dir, "changing drives")?;
    let mut file = read_machine(dir)?;
    let offer = validate(rt, machines, dir, &file)?;
    if file.drive.iter().any(|d| d.name == "system") {
        return Err("system disk already attached".into());
    }
    if file
        .drive
        .iter()
        .any(|drive| drive.bus == 0 && drive.target == 1)
    {
        return Err("system disk target 1 is occupied".into());
    }
    if !offer.scsi_bus(0).is_some_and(|s| s.targets.contains(&1)) {
        return Err("this machine has no system disk target".into());
    }
    let engine = QemuEngine::with_catalogue(rt.clone(), machines.clone());
    let path = engine.create_disk(&dir.join("drives/system"), size)?;
    file.drive.push(Drive {
        name: "system".into(),
        kind: "disk".into(),
        bus: 0,
        target: 1,
        image: format!("drives/{}", path.file_name().unwrap().to_string_lossy()),
        read_only: false,
        overlay: false,
    });
    fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
    println!("created {}", path.display());
    Ok(())
}
