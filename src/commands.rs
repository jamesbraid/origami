use crate::cli::CreateArgs;
use origami::{control, runtime};
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
    let graphics = options
        .graphics
        .as_deref()
        .or(origami::profiles::preset_graphics(&args.preset))
        .unwrap_or("none");
    origami::profiles::validate_graphics(offer, graphics)?;
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
        graphics,
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
