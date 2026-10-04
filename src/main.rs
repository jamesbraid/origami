mod cli;
mod commands;

use clap::Parser;
use cli::{Command as Action, DriveKind, NetworkMode};
use origami::runtime;
use origami::{
    catalogue, presets, read_machine, validate, Catalog, Drive, Network, PortForward, Result,
};
use origami::{control, install};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    if std::env::args_os().any(|arg| arg.to_str().is_none()) {
        eprintln!("origami: arguments must be valid UTF-8");
        return ExitCode::FAILURE;
    }
    let cli = cli::Cli::parse();
    let action = if cli.version {
        Action::Version
    } else {
        cli.command.expect("Clap requires a command")
    };
    match command(action) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("origami: {error}");
            ExitCode::FAILURE
        }
    }
}

fn directory(path: &Path) -> Result<PathBuf> {
    path.canonicalize().map_err(|error| {
        format!("cannot open machine directory '{}': {error}. Use the directory you created with origami create", path.display()).into()
    })
}

fn version_report() {
    let version = option_env!("VERGEN_GIT_DESCRIBE").unwrap_or("unknown");
    println!("origami {version}");
    for (name, path, prefix) in [
        ("qemu", runtime::qemu_path(), "QEMU emulator version "),
        (
            "instigator",
            install::instigator_path(),
            "instigator version ",
        ),
    ] {
        let version = path.ok().and_then(|path| {
            let output = Command::new(path).arg("--version").output().ok()?;
            let text = String::from_utf8(output.stdout).ok()?;
            let line = text.lines().next()?.strip_prefix(prefix)?;
            output.status.success().then(|| line.trim().to_owned())
        });
        println!("{name} {}", version.as_deref().unwrap_or("unavailable"));
    }
}

fn command(action: Action) -> Result<()> {
    if matches!(action, Action::Version) {
        version_report();
        return Ok(());
    }
    // Starting QEMU for its catalogue waits until a command needs an offering.
    let loaded = std::cell::OnceCell::new();
    let catalog = || -> Result<&Catalog> {
        if let Some(catalog) = loaded.get() {
            return Ok(catalog);
        }
        let catalog = catalogue()?;
        Ok(loaded.get_or_init(|| catalog))
    };
    match action {
        Action::Version => unreachable!(),
        Action::Machines => {
            for (name, offer) in presets(catalog()?) {
                println!(
                    "{name:36} {} CPUs, {} nodes, graphics: {} (experimental)",
                    offer.smp,
                    offer.nodes,
                    origami::profiles::graphics(offer).join("|")
                );
            }
        }
        Action::Create(args) => commands::create_machine(catalog()?, &args)?,
        Action::Validate(args) => {
            let dir = directory(&args.dir)?;
            let file = read_machine(&dir)?;
            let offer = validate(catalog()?, &dir, &file)?;
            println!("valid: {} ({} CPUs)", offer.topology, offer.smp);
        }
        Action::Show(args) => {
            let dir = directory(&args.dir)?;
            let file = read_machine(&dir)?;
            let offer = validate(catalog()?, &dir, &file)?;
            println!(
                "{}: {} CPUs, {} nodes, {} per node, {} graphics",
                file.machine.model,
                offer.smp,
                offer.nodes,
                file.machine.memory_per_node,
                file.machine.graphics
            );
            println!("state: {}", dir.join("state").display());
            println!("network: {}", file.network.mode);
            for forward in &file.network.forward {
                let status = if file.network.mode == "user" {
                    ""
                } else {
                    " (inactive until user networking)"
                };
                println!(
                    "forward {}: {} 127.0.0.1:{} -> guest:{}{}",
                    forward.name, forward.protocol, forward.host_port, forward.guest_port, status
                );
            }
            if let Some(identity) = &file.identity {
                println!("identity MAC: {}", identity.mac);
            }
            for drive in &file.drive {
                println!(
                    "{}: {} at scsi.{}:{} ({})",
                    drive.name, drive.kind, drive.bus, drive.target, drive.image
                );
            }
        }
        Action::ShowCommand(args) => {
            let dir = directory(&args.machine.dir)?;
            let file = read_machine(&dir)?;
            let offer = validate(catalog()?, &dir, &file)?;
            let display = args.display.resolve(&file.machine.graphics)?;
            let exe = runtime::qemu_path()?;
            let arguments = runtime::arguments(&dir, &file, offer, display)?;
            println!("{}", exe.display());
            for arg in arguments {
                println!("  {arg}");
            }
        }
        Action::Run(args) => {
            let dir = directory(&args.launch.machine.dir)?;
            let file = read_machine(&dir)?;
            let offer = validate(catalog()?, &dir, &file)?;
            let display = args.launch.display.resolve(&file.machine.graphics)?;
            if args.background {
                runtime::start_background(&dir, display)?;
            } else {
                let status = runtime::run(&dir, &file, offer, display)?;
                if !status.success() {
                    return Err(format!("QEMU exited with {status}").into());
                }
            }
        }
        Action::Serve(args) => {
            let dir = directory(&args.machine.dir)?;
            let file = read_machine(&dir)?;
            let offer = validate(catalog()?, &dir, &file)?;
            let display = args.display.resolve(&file.machine.graphics)?;
            let status = runtime::serve(&dir, &file, offer, display)?;
            if !status.success() {
                return Err(format!("QEMU exited with {status}").into());
            }
        }
        Action::Status(args) => {
            let dir = directory(&args.dir)?;
            if control::is_running(&dir)? {
                let record = control::read(&dir)?;
                println!("running: pid {}", record.pid);
            } else if control::is_locked(&dir)? {
                println!("running: foreground or starting");
            } else {
                println!("stopped");
            }
        }
        Action::Console(args) => control::console(&directory(&args.dir)?)?,
        Action::Stop(args) => {
            control::stop(&directory(&args.dir)?)?;
            println!("stop requested");
        }
        Action::DriveCreate { dir, size } => {
            let dir = directory(&dir)?;
            commands::create_disk(catalog()?, &dir, size)?
        }
        Action::DriveAttach {
            dir,
            file,
            kind,
            target,
        } => {
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "changing drives")?;
            let path = file.canonicalize()?;
            let mut file = read_machine(&dir)?;
            file.drive.push(Drive {
                name: format!("{}{target}", kind.as_str()),
                kind: kind.as_str().into(),
                bus: 0,
                target,
                image: path.display().to_string(),
                read_only: kind == DriveKind::Cdrom,
            });
            validate(catalog()?, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("attached {}", path.display());
        }
        Action::DriveDetach { dir, name } => {
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "detaching a drive")?;
            let mut file = read_machine(&dir)?;
            let matches: Vec<_> = file
                .drive
                .iter()
                .enumerate()
                .filter(|(_, drive)| drive.name == name)
                .map(|(index, _)| index)
                .collect();
            if matches.len() != 1 {
                return Err(
                    format!("expected one drive named {name}, found {}", matches.len()).into(),
                );
            }
            file.drive.remove(matches[0]);
            validate(catalog()?, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("detached {name}");
        }
        Action::NetworkSet {
            dir,
            mode,
            endpoint,
            mac,
        } => {
            if mode != NetworkMode::Private && (endpoint.is_some() || mac.is_some()) {
                return Err("--endpoint and --mac require --mode private".into());
            }
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "changing its network")?;
            let mut file = read_machine(&dir)?;
            file.network = Network {
                mode: mode.as_str().into(),
                endpoint,
                forward: file.network.forward.clone(),
            };
            origami::set_machine_mac(&mut file, mac.as_deref())?;
            validate(catalog()?, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("network: {}", mode.as_str());
        }
        Action::NetworkForwardAdd {
            dir,
            name,
            protocol,
            host_port,
            guest_port,
        } => {
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "changing its network")?;
            let mut file = read_machine(&dir)?;
            file.network.forward.push(PortForward {
                name: name.clone(),
                protocol: protocol.as_str().into(),
                host_port,
                guest_port,
            });
            validate(catalog()?, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("forward added: {name}");
        }
        Action::NetworkForwardRemove { dir, name } => {
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "changing its network")?;
            let mut file = read_machine(&dir)?;
            let count = file.network.forward.len();
            file.network.forward.retain(|forward| forward.name != name);
            if file.network.forward.len() == count {
                return Err(format!("unknown forward: {name}").into());
            }
            validate(catalog()?, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("forward removed: {name}");
        }
        Action::InstallInit {
            dir,
            media_root,
            mac,
            profile,
        } => {
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "changing its network")?;
            let mut file = read_machine(&dir)?;
            let path = if let Some(root) = media_root {
                install::init_profile(
                    catalog()?,
                    &dir,
                    &root,
                    mac.as_deref(),
                    &mut file,
                    profile.as_str(),
                )?
            } else {
                install::init_remote_profile(
                    catalog()?,
                    &dir,
                    mac.as_deref(),
                    &mut file,
                    profile.as_str(),
                )?
            };
            println!("created {}", path.display());
        }
        Action::InstallAddon {
            dir,
            name,
            source,
            install: selections,
            base,
            dist,
        } => {
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "changing its install add-ons")?;
            let file = read_machine(&dir)?;
            validate(catalog()?, &dir, &file)?;
            let path = install::add_addon(
                &dir,
                &name,
                &source,
                base.as_deref(),
                dist.as_deref(),
                &selections,
            )?;
            println!("configured add-on {name} in {}", path.display());
        }
        Action::InstallApply { dir, addon } => {
            let dir = directory(&dir)?;
            let file = read_machine(&dir)?;
            validate(catalog()?, &dir, &file)?;
            install::apply(&dir, &file, addon.as_deref())?;
        }
        Action::InstallFinish(args) => {
            let dir = directory(&args.dir)?;
            let file = read_machine(&dir)?;
            validate(catalog()?, &dir, &file)?;
            install::finish_rad4(&dir, &file)?;
        }
        Action::InstallCheck(args) => {
            let dir = directory(&args.dir)?;
            let file = read_machine(&dir)?;
            validate(catalog()?, &dir, &file)?;
            install::check(&dir, &file)?;
            println!("install media validated");
        }
        Action::InstallServe(args) => {
            let dir = directory(&args.dir)?;
            let file = read_machine(&dir)?;
            validate(catalog()?, &dir, &file)?;
            let status = install::serve(&dir, &file)?;
            if !status.success() {
                return Err(format!("Instigator exited with {status}").into());
            }
        }
    }
    Ok(())
}
