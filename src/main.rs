mod cli;
mod commands;

use clap::Parser;
use cli::{Action, DriveKind, NetworkMode, NvramAction};
use origami::runtime;
use origami::{control, graphics, install, listing, nvram, shares};
use origami::{machines, read_machine, validate, Drive, Network, PortForward, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    // Rust ignores SIGPIPE, so printing to a closed pipe such as
    // `origami machines | head` panics. Exit the way other Unix tools do.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
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
    match action {
        Action::Version => unreachable!(),
        Action::Machines { preset, all } => {
            let machines = machines()?;
            let text = match (preset, all) {
                (Some(preset), _) => listing::detail(&machines, &preset)?,
                (None, true) => listing::all(&machines)?,
                (None, false) => listing::table(&machines)?,
            };
            print!("{text}");
        }
        Action::Create(args) => commands::create_machine(&machines()?, &args)?,
        Action::Validate(args) => {
            let dir = directory(&args.dir)?;
            let file = read_machine(&dir)?;
            let machines = machines()?;
            let offer = validate(&machines, &dir, &file)?;
            for warning in origami::mac::duplicate_warnings(&machines, &dir, &file) {
                eprintln!("warning: {warning}");
            }
            match &file.machine.processor {
                Some(processor) => println!(
                    "valid: {} ({} CPUs, {processor})",
                    offer.topology, offer.smp
                ),
                None => println!("valid: {} ({} CPUs)", offer.topology, offer.smp),
            }
        }
        Action::Upgrade { dir, prom, io_prom } => {
            let dir = directory(&dir)?;
            match origami::migrate::upgrade(
                &dir,
                &machines()?,
                prom.as_deref(),
                io_prom.as_deref(),
            )? {
                Some(notes) => {
                    println!("upgraded {}", dir.display());
                    for note in notes {
                        println!("note: {note}; the guest sees this at its next boot");
                    }
                }
                None => println!("{} is current; it needs no upgrade", dir.display()),
            }
        }
        Action::Show(args) => {
            let dir = directory(&args.dir)?;
            let file = read_machine(&dir)?;
            let machines = machines()?;
            let offer = validate(&machines, &dir, &file)?;
            println!(
                "{}: {} CPUs, {} nodes, {} per node",
                file.machine.model, offer.smp, offer.nodes, file.machine.memory_per_node,
            );
            for line in graphics::show_lines(&machines, &offer.topology, &file.pipe) {
                println!("{line}");
            }
            match &file.machine.processor {
                Some(id) => println!("processor: {} ({id})", offer.processor),
                None => println!("processor: {}", offer.processor),
            }
            println!("state: {}", dir.join("state").display());
            let console = file.machine.console.as_deref();
            println!(
                "console: {}",
                console.map_or_else(|| runtime::default_console(offer), Ok)?
            );
            match offer.default_network() {
                Some(adapter) => println!(
                    "network: {} ({}{})",
                    file.network.mode,
                    adapter
                        .interface
                        .as_deref()
                        .map_or_else(String::new, |name| format!("{name}, ")),
                    adapter.chip()
                ),
                None => println!("network: {}", file.network.mode),
            }
            if !file.qemu.extra_args.is_empty() {
                println!(
                    "qemu extra-args (unsupported): {}",
                    file.qemu
                        .extra_args
                        .iter()
                        .map(|arg| runtime::shell_quote(arg))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            for share in &file.share {
                println!("share {}: {}", share.name, share.path.display());
                if let Ok(lines) = shares::mount_lines(&file, share) {
                    println!("  {}", lines[1]);
                }
            }
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
            let machines = machines()?;
            let offer = validate(&machines, &dir, &file)?;
            let display = args.display.resolve(!file.pipe.is_empty())?;
            let exe = runtime::qemu_path()?;
            let ports = (!file.share.is_empty()).then(shares::Ports::placeholder);
            let arguments =
                runtime::arguments_with(&machines, &dir, &file, offer, display, ports.as_ref())?;
            let mut arguments = arguments;
            runtime::append_extra_args(&mut arguments, &file, &args.qemu_args)?;
            // One line, runnable as printed: one shell word per argument.
            print!("{}", runtime::shell_quote(&exe.display().to_string()));
            for arg in arguments {
                print!(" {}", runtime::shell_quote(&arg));
            }
            println!();
        }
        Action::Run(args) => {
            let dir = directory(&args.launch.machine.dir)?;
            let file = read_machine(&dir)?;
            let machines = machines()?;
            let offer = validate(&machines, &dir, &file)?;
            let display = args.launch.display.resolve(!file.pipe.is_empty())?;
            if args.background {
                runtime::start_background(&dir, display, &args.launch.qemu_args)?;
            } else {
                let status = runtime::run(
                    &machines,
                    &dir,
                    &file,
                    offer,
                    display,
                    &args.launch.qemu_args,
                )?;
                if !status.success() {
                    return Err(format!("QEMU exited with {status}").into());
                }
            }
        }
        Action::Serve(args) => {
            let dir = directory(&args.machine.dir)?;
            let file = read_machine(&dir)?;
            let machines = machines()?;
            let offer = validate(&machines, &dir, &file)?;
            let display = args.display.resolve(!file.pipe.is_empty())?;
            let status = runtime::serve(&machines, &dir, &file, offer, display, &args.qemu_args)?;
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
        Action::GraphicsSet { dir, graphics } => {
            let dir = directory(&dir)?;
            commands::set_graphics(&machines()?, &dir, &graphics)?
        }
        Action::DriveCreate { dir, size } => {
            let dir = directory(&dir)?;
            commands::create_disk(&machines()?, &dir, size)?
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
            validate(&machines()?, &dir, &file)?;
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
            validate(&machines()?, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("detached {name}");
        }
        Action::ConsoleSet { dir, port } => {
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "changing its console")?;
            let mut file = read_machine(&dir)?;
            file.machine.console = port;
            let machines = machines()?;
            let offer = validate(&machines, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            let console = file.machine.console.as_deref();
            println!(
                "console: {}",
                console.map_or_else(|| runtime::default_console(offer), Ok)?
            );
        }
        Action::Nvram(args) => {
            let dir = directory(&args.dir)?;
            let _lock = control::lock_for_edit(&dir, "reading or changing its NVRAM")?;
            let file = read_machine(&dir)?;
            let machines = machines()?;
            let offer = validate(&machines, &dir, &file)?;
            let mut store = nvram::open(&dir, offer)?;
            match args.action {
                None => {
                    for (name, value) in store.variables() {
                        println!("{name}={value}");
                    }
                }
                Some(NvramAction::Get { name }) => {
                    let (name, value) = store.get(&name)?;
                    println!("{name}={value}");
                }
                Some(NvramAction::Set { settings }) => {
                    // Every setting is checked before the store is written.
                    let mut names = Vec::new();
                    for (name, value) in &settings {
                        names.push(store.set(name, value)?);
                    }
                    nvram::save(&dir, &store)?;
                    for name in names {
                        println!("{name}={}", store.get(name)?.1);
                    }
                }
            }
        }
        Action::NetworkSet {
            dir,
            mode,
            endpoint,
            interface,
            share_address,
            mac,
        } => {
            use NetworkMode::{Private, Tap, Vmnet};
            if endpoint.is_some() && !matches!(mode, Private | Vmnet) {
                return Err("--endpoint requires --mode private or vmnet".into());
            }
            if interface.is_some() && mode != Tap {
                return Err("--interface requires --mode tap".into());
            }
            if mac.is_some() && !matches!(mode, Private | Tap | Vmnet) {
                return Err("--mac requires --mode private, tap or vmnet".into());
            }
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "changing its network")?;
            let mut file = read_machine(&dir)?;
            file.network = Network {
                mode: mode.as_str().into(),
                endpoint,
                interface,
                forward: file.network.forward.clone(),
                share_address,
            };
            let machines = machines()?;
            // User and none networking run without one; the rest need it.
            let assigned = if matches!(mode, Private | Tap | Vmnet) {
                origami::ensure_machine_mac(&machines, &dir, &mut file, mac.as_deref())?
            } else {
                origami::set_machine_mac(&machines, &dir, &mut file, mac.as_deref())?;
                None
            };
            validate(&machines, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            if let Some(mac) = assigned {
                println!("assigned MAC {mac}");
            }
            println!("network: {}", mode.as_str());
            if mode != NetworkMode::User && !file.network.forward.is_empty() {
                println!("forwards inactive in {} mode", mode.as_str());
            }
        }
        Action::Share {
            dir,
            name,
            host_dir,
            remove,
        } => {
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "changing its shares")?;
            let mut file = read_machine(&dir)?;
            if remove {
                let count = file.share.len();
                file.share.retain(|share| share.name != name);
                if file.share.len() == count {
                    return Err(format!("unknown share: {name}").into());
                }
                validate(&machines()?, &dir, &file)?;
                fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
                println!("share removed: {name}");
            } else {
                let host_dir = host_dir.ok_or("share needs a host folder")?;
                let path = std::path::absolute(&host_dir)?;
                if !path.is_dir() {
                    return Err(format!("{} is not a directory", host_dir.display()).into());
                }
                shares::check_name(&name)?;
                if file.share.iter().any(|share| share.name == name) {
                    return Err(format!(
                        "share {name} exists; remove it first: origami share {} {name} --remove",
                        dir.display()
                    )
                    .into());
                }
                file.share.push(origami::Share {
                    name: name.clone(),
                    path,
                });
                validate(&machines()?, &dir, &file)?;
                fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
                let share = file.share.last().expect("share just added");
                println!("share added: {name}");
                for line in shares::mount_lines(&file, share)? {
                    println!("{line}");
                }
            }
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
            validate(&machines()?, &dir, &file)?;
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
            validate(&machines()?, &dir, &file)?;
            fs::write(dir.join("machine.toml"), toml::to_string_pretty(&file)?)?;
            println!("forward removed: {name}");
        }
        Action::InstallInit {
            dir,
            media_root,
            mac,
            profile,
        } => {
            if let Some(old) = profile.old_name() {
                return Err(
                    format!("--profile {old} is now --profile {}", profile.as_str()).into(),
                );
            }
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "changing its network")?;
            let mut file = read_machine(&dir)?;
            let path = if let Some(root) = media_root {
                install::init_profile(
                    &machines()?,
                    &dir,
                    &root,
                    mac.as_deref(),
                    &mut file,
                    install::DEFAULT_RELEASE,
                    profile.as_str(),
                )?
            } else {
                install::init_remote_profile(
                    &machines()?,
                    &dir,
                    mac.as_deref(),
                    &mut file,
                    install::DEFAULT_RELEASE,
                    profile.as_str(),
                )?
            };
            println!("created {}", path.display());
            if install::needs_rad4_driver(&file) {
                println!("configured add-on rad4 for the RAD4 graphics");
            }
        }
        Action::InstallAddon {
            dir,
            name,
            source,
            install: selections,
            base,
            dist,
            media_root,
        } => {
            let dir = directory(&dir)?;
            let _lock = control::lock_for_edit(&dir, "changing its install add-ons")?;
            let file = read_machine(&dir)?;
            validate(&machines()?, &dir, &file)?;
            let path = match source {
                Some(source) => install::add_addon(
                    &dir,
                    &name,
                    &source,
                    base.as_deref(),
                    dist.as_deref(),
                    &selections,
                )?,
                None => install::add_known_addon(&dir, &name, media_root.as_deref())?,
            };
            println!("configured add-on {name} in {}", path.display());
        }
        Action::InstallApply { dir, addon } => {
            let dir = directory(&dir)?;
            let file = read_machine(&dir)?;
            validate(&machines()?, &dir, &file)?;
            install::apply(&dir, &file, addon.as_deref())?;
        }
        Action::InstallCheck { machine, full } => {
            let dir = directory(&machine.dir)?;
            let file = read_machine(&dir)?;
            validate(&machines()?, &dir, &file)?;
            install::check(&dir, &file, full)?;
            println!("install media validated");
        }
        Action::InstallServe(args) => {
            let dir = directory(&args.dir)?;
            let file = read_machine(&dir)?;
            validate(&machines()?, &dir, &file)?;
            let status = install::serve(&dir, &file)?;
            if !status.success() {
                return Err(format!("Instigator exited with {status}").into());
            }
        }
    }
    Ok(())
}
