use sgi::install;
use sgi::runtime::{self, Display};
use sgi::{
    catalogue, create, preset, presets, read_machine, resolve, validate, Drive, Network, Result,
};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match command() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sgi: {error}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> &'static str {
    "usage:\n  sgi machines\n  sgi create DIR --preset PRESET --prom FILE\n  sgi validate DIR\n  sgi show DIR\n  sgi show-command DIR [--display local|vnc|none]\n  sgi run DIR [--display local|vnc|none]\n  sgi drive-create DIR SIZE-MiB\n  sgi drive-attach DIR FILE --type disk|cdrom --target N\n  sgi network-set DIR --mode user|none|private [--endpoint PATH --mac MAC]\n  sgi install-init DIR --media-root PATH --mac MAC\n  sgi install-check DIR\n  sgi install-serve DIR\n  sgi version"
}

fn value<'a>(args: &'a [String], flag: &str) -> Result<&'a str> {
    let pos = args
        .iter()
        .position(|a| a == flag)
        .ok_or_else(|| format!("missing {flag}"))?;
    Ok(args
        .get(pos + 1)
        .ok_or_else(|| format!("missing value for {flag}"))?)
}

fn optional<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|pos| args.get(pos + 1))
        .map(String::as_str)
}

fn directory(args: &[String]) -> Result<PathBuf> {
    let name = args.first().ok_or("missing machine directory")?;
    Ok(PathBuf::from(name).canonicalize()?)
}

fn display(args: &[String], graphics: &str) -> Result<Display> {
    let default = if graphics == "none" { "none" } else { "local" };
    Display::parse(optional(args, "--display").unwrap_or(default))
}

fn command() -> Result<()> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        return Err(usage().into());
    }
    let action = args.remove(0);
    let catalog = catalogue()?;
    match action.as_str() {
        "machines" => {
            for (name, offer) in presets(&catalog) {
                let capability = if name == "origin200-1" {
                    "RAD4 graphics"
                } else if name == "origin300-2" {
                    "experimental firmware"
                } else {
                    "serial console"
                };
                println!(
                    "{name:18} {:10} {} CPUs, {} nodes, {capability}",
                    offer.product, offer.smp, offer.nodes
                );
            }
        }
        "create" => {
            let path = args.first().ok_or("missing destination directory")?;
            let offer = preset(&catalog, value(&args, "--preset")?)?;
            create(Path::new(path), offer, Path::new(value(&args, "--prom")?))?;
            println!("created {path}");
        }
        "validate" | "show" | "show-command" | "run" => {
            let dir = directory(&args)?;
            let file = read_machine(&dir)?;
            let offer = validate(&catalog, &dir, &file)?;
            match action.as_str() {
                "validate" => println!("valid: {} ({} CPUs)", offer.topology, offer.smp),
                "show" => {
                    println!(
                        "{}: {} CPUs, {} nodes, {} graphics",
                        file.machine.model, offer.smp, offer.nodes, file.machine.graphics
                    );
                    println!(
                        "firmware: {}",
                        resolve(&dir, &file.firmware.image).display()
                    );
                    println!("network: {}", file.network.mode);
                    for drive in &file.drive {
                        println!(
                            "{}: {} at scsi.{}:{} ({})",
                            drive.name, drive.kind, drive.bus, drive.target, drive.image
                        );
                    }
                }
                "show-command" => {
                    let display = display(&args, &file.machine.graphics)?;
                    let exe = runtime::qemu_path()?;
                    let arguments = runtime::arguments(&dir, &file, offer, display)?;
                    println!("{}", exe.display());
                    for arg in arguments {
                        println!("  {arg}");
                    }
                }
                "run" => {
                    let display = display(&args, &file.machine.graphics)?;
                    let status = runtime::run(&dir, &file, offer, display)?;
                    if !status.success() {
                        return Err(format!("QEMU exited with {status}").into());
                    }
                }
                _ => unreachable!(),
            }
        }
        "drive-create" => {
            let dir = directory(&args)?;
            let size: u64 = args.get(1).ok_or("missing size in MiB")?.parse()?;
            if size == 0 || size > 131072 {
                return Err("disk size must be 1..131072 MiB".into());
            }
            let mut file = read_machine(&dir)?;
            let offer = validate(&catalog, &dir, &file)?;
            if file.drive.iter().any(|d| d.name == "system") {
                return Err("system disk already attached".into());
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
                .args([
                    "create",
                    "-f",
                    "qcow2",
                    path.to_str().ok_or("non-UTF8 disk path")?,
                    &format!("{size}M"),
                ])
                .output()?;
            if !output.status.success() {
                return Err(
                    format!("qemu-img: {}", String::from_utf8_lossy(&output.stderr)).into(),
                );
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
        }
        "drive-attach" => {
            let dir = directory(&args)?;
            let path = PathBuf::from(args.get(1).ok_or("missing image path")?).canonicalize()?;
            let kind = value(&args, "--type")?;
            if !matches!(kind, "disk" | "cdrom") {
                return Err("drive type must be disk or cdrom".into());
            }
            let target: u32 = value(&args, "--target")?.parse()?;
            let mut file = read_machine(&dir)?;
            file.drive.push(Drive {
                name: format!("{kind}{target}"),
                kind: kind.into(),
                bus: 0,
                target,
                image: path.display().to_string(),
                read_only: kind == "cdrom",
            });
            validate(&catalog, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("attached {}", path.display());
        }
        "network-set" => {
            let dir = directory(&args)?;
            let mut file = read_machine(&dir)?;
            let mode = value(&args, "--mode")?;
            file.network = match mode {
                "private" => Network {
                    mode: mode.into(),
                    endpoint: Some(value(&args, "--endpoint")?.into()),
                    mac: Some(value(&args, "--mac")?.into()),
                },
                "user" | "none" => Network {
                    mode: mode.into(),
                    endpoint: None,
                    mac: None,
                },
                _ => return Err("network mode must be user, none, or private".into()),
            };
            validate(&catalog, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("network: {mode}");
        }
        "install-init" => {
            let dir = directory(&args)?;
            let mut file = read_machine(&dir)?;
            let path = install::init(
                &dir,
                Path::new(value(&args, "--media-root")?),
                value(&args, "--mac")?,
                &mut file,
            )?;
            println!("created {}", path.display());
        }
        "install-check" | "install-serve" => {
            let dir = directory(&args)?;
            let file = read_machine(&dir)?;
            validate(&catalog, &dir, &file)?;
            if action == "install-check" {
                let media = install::read_media(&dir)?;
                install::config(&dir, &file, &media)?;
                println!("install media ready");
            } else {
                let status = install::serve(&dir, &file)?;
                if !status.success() {
                    return Err(format!("Instigator exited with {status}").into());
                }
            }
        }
        "version" => println!("sgi {}", env!("CARGO_PKG_VERSION")),
        _ => return Err(usage().into()),
    }
    Ok(())
}
