use origami::runtime::{self, Display};
use origami::{
    catalogue, catalogue_sha256, create, preset, presets, read_machine, resolve, validate, Drive,
    Network, Origin300Create, PortForward, Result,
};
use origami::{control, install};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match command() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("origami: {error}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> &'static str {
    "usage:\n  origami machines\n  origami create DIR --preset PRESET --prom FILE [--memory-per-node MiB] [--spd-dimm2 FILE --spd-dimm3 FILE --mac MAC]\n  origami validate DIR\n  origami show DIR\n  origami show-command DIR [--display local|vnc|none] [--vnc-port PORT]\n  origami run DIR [--display local|vnc|none] [--vnc-port PORT] [--background]\n  origami status DIR\n  origami console DIR\n  origami stop DIR\n  origami drive-create DIR SIZE-MiB\n  origami drive-attach DIR FILE --type disk|cdrom|tape --target N\n  origami drive-detach DIR NAME\n  origami network-set DIR --mode user|none|private [--endpoint PATH --mac MAC]\n  origami network-forward-add DIR NAME --protocol tcp|udp --host-port PORT --guest-port PORT\n  origami network-forward-remove DIR NAME\n  origami install-init DIR --media-root PATH --mac MAC\n  origami install-addon DIR --name NAME --source PATH --install PRODUCT.SUBSYSTEM [--base DIR --dist DIR]\n  origami install-check DIR\n  origami install-serve DIR\n  origami install-apply DIR [--addon NAME]\n  origami install-finish DIR\n  origami version"
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
    if args.iter().any(|arg| arg == "--vnc-port") && optional(args, "--vnc-port").is_none() {
        return Err("missing value for --vnc-port".into());
    }
    Display::parse(optional(args, "--display").unwrap_or(default))?
        .with_vnc_port(optional(args, "--vnc-port"))
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
            if offer.product != "origin300"
                && ["--spd-dimm2", "--spd-dimm3", "--mac"]
                    .iter()
                    .any(|flag| args.iter().any(|arg| arg == flag))
            {
                return Err("SPD and identity inputs apply only to Origin 300".into());
            }
            let identity = if offer.product == "origin300" {
                Some(Origin300Create {
                    spd_dimm2: Path::new(value(&args, "--spd-dimm2")?),
                    spd_dimm3: Path::new(value(&args, "--spd-dimm3")?),
                    mac: optional(&args, "--mac").unwrap_or("08:00:69:12:34:56"),
                })
            } else {
                None
            };
            let memory_per_node = if args.iter().any(|arg| arg == "--memory-per-node") {
                Some(value(&args, "--memory-per-node")?.parse::<u32>()?)
            } else {
                None
            };
            create(
                Path::new(path),
                offer,
                Path::new(value(&args, "--prom")?),
                memory_per_node,
                identity,
            )?;
            println!("created {path}");
        }
        "validate" | "show" | "show-command" | "run" | "_serve" => {
            let dir = directory(&args)?;
            let file = read_machine(&dir)?;
            let offer = validate(&catalog, &dir, &file)?;
            match action.as_str() {
                "validate" => println!("valid: {} ({} CPUs)", offer.topology, offer.smp),
                "show" => {
                    println!(
                        "{}: {} CPUs, {} nodes, {} per node, {} graphics",
                        file.machine.model,
                        offer.smp,
                        offer.nodes,
                        file.machine.memory_per_node,
                        file.machine.graphics
                    );
                    println!(
                        "firmware: {}",
                        resolve(&dir, &file.firmware.image).display()
                    );
                    println!("network: {}", file.network.mode);
                    for forward in &file.network.forward {
                        let status = if file.network.mode == "user" {
                            ""
                        } else {
                            " (inactive until user networking)"
                        };
                        println!(
                            "forward {}: {} 127.0.0.1:{} -> guest:{}{}",
                            forward.name,
                            forward.protocol,
                            forward.host_port,
                            forward.guest_port,
                            status
                        );
                    }
                    if let Some(identity) = &file.identity {
                        println!("IO8 MAC: {}", identity.mac);
                        println!(
                            "SPD DIMM 2: {}",
                            resolve(&dir, &identity.spd_dimm2).display()
                        );
                        println!(
                            "SPD DIMM 3: {}",
                            resolve(&dir, &identity.spd_dimm3).display()
                        );
                    }
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
                "run" | "_serve" => {
                    let display = display(&args, &file.machine.graphics)?;
                    if action == "run" && args.iter().any(|arg| arg == "--background") {
                        runtime::start_background(&dir, display)?;
                    } else {
                        let status = if action == "_serve" {
                            runtime::serve(&dir, &file, offer, display)?
                        } else {
                            runtime::run(&dir, &file, offer, display)?
                        };
                        if !status.success() {
                            return Err(format!("QEMU exited with {status}").into());
                        }
                    }
                }
                _ => unreachable!(),
            }
        }
        "status" => {
            let dir = directory(&args)?;
            if control::is_running(&dir)? {
                let record = control::read(&dir)?;
                println!("running: pid {}", record.pid);
            } else if control::is_locked(&dir)? {
                println!("running: foreground or starting");
            } else {
                println!("stopped");
            }
        }
        "console" => control::console(&directory(&args)?)?,
        "stop" => {
            control::stop(&directory(&args)?)?;
            println!("stop requested");
        }
        "drive-create" => {
            let dir = directory(&args)?;
            let _lock = control::lock_for_edit(&dir, "changing drives")?;
            let size: u64 = args.get(1).ok_or("missing size in MiB")?.parse()?;
            if size == 0 || size > 131072 {
                return Err("disk size must be 1..131072 MiB".into());
            }
            let mut file = read_machine(&dir)?;
            let offer = validate(&catalog, &dir, &file)?;
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
            let _lock = control::lock_for_edit(&dir, "changing drives")?;
            let path = PathBuf::from(args.get(1).ok_or("missing image path")?).canonicalize()?;
            let kind = value(&args, "--type")?;
            if !matches!(kind, "disk" | "cdrom" | "tape") {
                return Err("drive type must be disk, cdrom, or tape".into());
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
        "drive-detach" => {
            let dir = directory(&args)?;
            let name = args.get(1).ok_or("missing drive name")?;
            let _lock = control::lock_for_edit(&dir, "detaching a drive")?;
            let mut file = read_machine(&dir)?;
            let matches: Vec<_> = file
                .drive
                .iter()
                .enumerate()
                .filter(|(_, drive)| drive.name == *name)
                .map(|(index, _)| index)
                .collect();
            if matches.len() != 1 {
                return Err(
                    format!("expected one drive named {name}, found {}", matches.len()).into(),
                );
            }
            file.drive.remove(matches[0]);
            validate(&catalog, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("detached {name}");
        }
        "network-set" => {
            let dir = directory(&args)?;
            let _lock = control::lock_for_edit(&dir, "changing its network")?;
            let mut file = read_machine(&dir)?;
            let mode = value(&args, "--mode")?;
            let forward = file.network.forward.clone();
            file.network = match mode {
                "private" => Network {
                    mode: mode.into(),
                    endpoint: Some(value(&args, "--endpoint")?.into()),
                    mac: Some(value(&args, "--mac")?.into()),
                    forward,
                },
                "user" | "none" => Network {
                    mode: mode.into(),
                    endpoint: None,
                    mac: None,
                    forward,
                },
                _ => return Err("network mode must be user, none, or private".into()),
            };
            validate(&catalog, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("network: {mode}");
        }
        "network-forward-add" => {
            let dir = directory(&args)?;
            let _lock = control::lock_for_edit(&dir, "changing its network")?;
            let mut file = read_machine(&dir)?;
            let name = args.get(1).ok_or("missing forward name")?;
            file.network.forward.push(PortForward {
                name: name.clone(),
                protocol: value(&args, "--protocol")?.into(),
                host_port: value(&args, "--host-port")?.parse()?,
                guest_port: value(&args, "--guest-port")?.parse()?,
            });
            validate(&catalog, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("forward added: {name}");
        }
        "network-forward-remove" => {
            let dir = directory(&args)?;
            let _lock = control::lock_for_edit(&dir, "changing its network")?;
            let mut file = read_machine(&dir)?;
            let name = args.get(1).ok_or("missing forward name")?;
            let count = file.network.forward.len();
            file.network.forward.retain(|forward| forward.name != *name);
            if file.network.forward.len() == count {
                return Err(format!("unknown forward: {name}").into());
            }
            validate(&catalog, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("forward removed: {name}");
        }
        "install-init" => {
            let dir = directory(&args)?;
            let _lock = control::lock_for_edit(&dir, "changing its network")?;
            let mut file = read_machine(&dir)?;
            let path = install::init(
                &dir,
                Path::new(value(&args, "--media-root")?),
                value(&args, "--mac")?,
                &mut file,
            )?;
            println!("created {}", path.display());
        }
        "install-addon" => {
            let dir = directory(&args)?;
            let _lock = control::lock_for_edit(&dir, "changing its install add-ons")?;
            let file = read_machine(&dir)?;
            validate(&catalog, &dir, &file)?;
            let name = value(&args, "--name")?;
            let install = args
                .iter()
                .enumerate()
                .filter(|(_, arg)| *arg == "--install")
                .map(|(index, _)| {
                    args.get(index + 1)
                        .cloned()
                        .ok_or("missing value for --install".into())
                })
                .collect::<Result<Vec<_>>>()?;
            let path = install::add_addon(
                &dir,
                name,
                Path::new(value(&args, "--source")?),
                optional(&args, "--base"),
                optional(&args, "--dist"),
                &install,
            )?;
            println!("configured add-on {name} in {}", path.display());
        }
        "install-apply" => {
            let dir = directory(&args)?;
            let file = read_machine(&dir)?;
            validate(&catalog, &dir, &file)?;
            let addon = if args.iter().any(|arg| arg == "--addon") {
                Some(value(&args, "--addon")?)
            } else {
                None
            };
            install::apply(&dir, &file, addon)?;
        }
        "install-finish" => {
            let dir = directory(&args)?;
            let file = read_machine(&dir)?;
            validate(&catalog, &dir, &file)?;
            install::finish_rad4(&dir, &file)?;
        }
        "install-check" | "install-serve" => {
            let dir = directory(&args)?;
            let file = read_machine(&dir)?;
            validate(&catalog, &dir, &file)?;
            if action == "install-check" {
                install::check(&dir, &file)?;
                println!("install media validated");
            } else {
                let status = install::serve(&dir, &file)?;
                if !status.success() {
                    return Err(format!("Instigator exited with {status}").into());
                }
            }
        }
        "version" => {
            println!("origami {}", env!("CARGO_PKG_VERSION"));
            println!("catalogue=sha256:{}", catalogue_sha256());
            let executable = env::current_exe()?;
            let manifest = executable
                .parent()
                .ok_or("cannot locate origami executable directory")?
                .join("../share/sgi/source-revisions.txt");
            if manifest.is_file() {
                print!("{}", fs::read_to_string(manifest)?);
            } else {
                println!("product=unpackaged");
                println!("qemu={}", env!("SGI_QEMU_REVISION"));
                println!("instigator={}", env!("SGI_INSTIGATOR_REVISION"));
            }
        }
        _ => return Err(usage().into()),
    }
    Ok(())
}
