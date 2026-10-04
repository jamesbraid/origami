use crate::cli::CreateArgs;
use origami::{
    catalogue, preset, read_machine, validate, validate_create_inputs, Drive, Origin300Create,
    Result,
};
use origami::{control, runtime};
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn create_machine(args: &CreateArgs) -> Result<()> {
    let catalog = catalogue()?;
    let offer = preset(&catalog, &args.preset)?;
    let options = &args.options;
    if offer.product != "origin300"
        && (options.spd_dimm2.is_some() || options.spd_dimm3.is_some() || options.mac.is_some())
    {
        return Err("SPD and identity inputs apply only to Origin 300".into());
    }
    let identity = match (&options.spd_dimm2, &options.spd_dimm3) {
        (Some(dimm2), Some(dimm3)) => Some(Origin300Create {
            spd_dimm2: dimm2,
            spd_dimm3: dimm3,
            mac: options.mac.as_deref().unwrap_or("08:00:69:12:34:56"),
        }),
        (None, None) => None,
        _ => return Err("both --spd-dimm2 and --spd-dimm3 are required".into()),
    };
    if identity.is_none() && options.mac.is_some() {
        return Err(
            "--mac requires the explicit --spd-dimm2 and --spd-dimm3 identity inputs".into(),
        );
    }
    let graphics = options
        .graphics
        .as_deref()
        .or_else(|| args.preset.ends_with("-impact").then_some("si"));
    origami::profiles::validate_graphics(
        offer,
        graphics.unwrap_or(origami::profiles::default_graphics(offer)),
    )?;
    let inputs = options.hardware.inputs();
    origami::profiles::validate_inputs(offer, &inputs)?;
    validate_create_inputs(&args.dir, offer, options.memory_per_node, identity.as_ref())?;
    let prom = match &options.prom {
        Some(path) => path.clone(),
        None => origami::assets::acquire(&args.preset)?,
    };
    origami::create_configured(
        &args.dir,
        offer,
        &prom,
        options.memory_per_node,
        identity,
        graphics,
        inputs,
    )?;
    println!("created {}", args.dir.display());
    Ok(())
}

pub fn create_disk(dir: &Path, size: u64) -> Result<()> {
    let _lock = control::lock_for_edit(dir, "changing drives")?;
    if size == 0 || size > 131072 {
        return Err("disk size must be 1..131072 MiB".into());
    }
    let mut file = read_machine(dir)?;
    let catalog = catalogue()?;
    let offer = validate(&catalog, dir, &file)?;
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
        .storage
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
