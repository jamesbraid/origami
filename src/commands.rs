use crate::cli::{CreateArgs, GraphicsArgs};
use origami::{control, graphics, runtime};
use origami::{
    preset, read_machine, validate, validate_create_inputs, Drive, MachineInit, Machines, Result,
};
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn create_machine(machines: &Machines, args: &CreateArgs) -> Result<()> {
    let options = &args.options;
    let preset_offer = preset(machines, &args.preset)?;
    let offer = match &options.processor {
        Some(processor) => origami::profiles::with_processor(machines, preset_offer, processor)?,
        None => preset_offer,
    };
    let request = origami::profiles::graphics_request(
        &args.preset,
        options.graphics.graphics.as_deref(),
        options.graphics.graphics_slot.as_deref(),
        options.graphics.settings()?,
    );
    let pipes = graphics::select(machines, &offer.topology, offer.nodes, &request)?;
    let inputs = options.hardware.inputs()?;
    origami::profiles::validate_inputs(offer, &inputs)?;
    validate_create_inputs(
        &args.dir,
        offer,
        options.memory_per_node,
        options.mac.as_deref(),
    )?;
    let tool = runtime::machine_init_path()?;
    // Check the tool before any download that it would consume.
    if !tool.is_file() {
        return Err(format!("packaged machine init tool missing: {}", tool.display()).into());
    }
    let (prom, io_prom) =
        origami::firmware(offer, options.prom.as_deref(), options.io_prom.as_deref())?;
    origami::create_configured(
        &args.dir,
        offer,
        &MachineInit {
            tool: &tool,
            boot_prom: &prom,
            io_prom: io_prom.as_deref(),
        },
        options.memory_per_node,
        options.mac.as_deref(),
        pipes,
        inputs,
    )?;
    println!("created {}", args.dir.display());
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
pub fn set_graphics(machines: &Machines, dir: &Path, args: &GraphicsArgs) -> Result<()> {
    let _lock = control::lock_for_edit(dir, "changing its graphics")?;
    let mut file = read_machine(dir)?;
    let offer = validate(machines, dir, &file)?;
    if args.graphics.is_none() {
        return Err("name the graphics with --graphics PRODUCT or --graphics none".into());
    }
    let mut request = graphics::Request {
        product: args.graphics.as_deref(),
        slot: args.graphics_slot.as_deref(),
        settings: args.settings()?,
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
    validate(machines, dir, &file)?;
    fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
    println!("graphics changed; the guest sees them at its next boot");
    Ok(())
}

pub fn create_disk(machines: &Machines, dir: &Path, size: u64) -> Result<()> {
    let _lock = control::lock_for_edit(dir, "changing drives")?;
    let mut file = read_machine(dir)?;
    let offer = validate(machines, dir, &file)?;
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
    if !offer
        .scsi_adapters
        .iter()
        .any(|s| s.bus == "scsi.0" && s.targets.contains(&1))
    {
        return Err("this machine has no system disk target".into());
    }
    let path = dir.join("drives/system.qcow2");
    if path.exists() {
        return Err(format!("disk exists: {}", path.display()).into());
    }
    let output = Command::new(runtime::qemu_img_path()?)
        .args(["create", "-f", "qcow2"])
        .arg(&path)
        .arg(format!("{size}M"))
        .output()?;
    if !output.status.success() {
        return Err(format!("qemu-img: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    file.drive.push(Drive {
        name: "system".into(),
        kind: "disk".into(),
        bus: 0,
        target: 1,
        image: "drives/system.qcow2".into(),
        read_only: false,
    });
    fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
    println!("created {}", path.display());
    Ok(())
}
