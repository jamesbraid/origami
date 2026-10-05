use crate::cli::CreateArgs;
use origami::{control, runtime};
use origami::{
    preset, read_machine, validate, validate_create_inputs, Catalog, Drive, MachineInit, Result,
};
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn create_machine(catalog: &Catalog, args: &CreateArgs) -> Result<()> {
    let offer = preset(catalog, &args.preset)?;
    let options = &args.options;
    let graphics = options
        .graphics
        .as_deref()
        .or_else(|| args.preset.ends_with("-impact").then_some("si"));
    origami::profiles::validate_graphics(
        offer,
        graphics.unwrap_or(origami::profiles::default_graphics(offer)),
    )?;
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
    let prom = match &options.prom {
        Some(path) => path.clone(),
        None => origami::assets::acquire(&args.preset)?,
    };
    let io_prom = match &options.io_prom {
        Some(path) => Some(path.clone()),
        None => origami::assets::acquire_io(&args.preset)?,
    };
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
    Ok(())
}

pub fn create_disk(catalog: &Catalog, dir: &Path, size: u64) -> Result<()> {
    let _lock = control::lock_for_edit(dir, "changing drives")?;
    let mut file = read_machine(dir)?;
    let offer = validate(catalog, dir, &file)?;
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
