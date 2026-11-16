mod cli;

use clap::Parser;
use cli::{Action, NvramAction};
use origami_core::{Change, DriveRequest, InstallAddon, InstallInit, Report, Status, Stream};
use origami_machine::{control, graphics, install, shares, Result};
use origami_qemu::{runtime, QemuEngine};
use std::collections::BTreeMap;
use std::path::Path;
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

/// Print what an operation had to say, warnings on the error stream.
fn emit(report: &Report) {
    for (stream, text) in report.rendered() {
        match stream {
            Stream::Out => println!("{text}"),
            Stream::Err => eprintln!("{text}"),
        }
    }
}

fn version_report(rt: &runtime::Runtime) {
    let version = option_env!("VERGEN_GIT_DESCRIBE").unwrap_or("unknown");
    println!("origami {version}");
    for (name, path, prefix) in [
        ("qemu", Ok(rt.qemu()), "QEMU emulator version "),
        (
            "instigator",
            rt.instigator().map(Path::to_path_buf),
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

/// Apply one edit to the machine in `dir` and print what it says.
fn change(engine: &QemuEngine, dir: &Path, change: Change) -> Result<()> {
    let dir = origami_core::directory(dir)?;
    emit(&origami_core::apply_change(engine, &dir, &change)?);
    Ok(())
}

fn command(action: Action) -> Result<()> {
    let rt = runtime::Runtime::locate()?;
    if matches!(action, Action::Version) {
        version_report(&rt);
        return Ok(());
    }
    let engine = QemuEngine::new(rt.clone());
    match action {
        Action::Version => unreachable!(),
        Action::Machines { preset, all } => {
            print!(
                "{}",
                origami_core::listing(&engine, preset.as_deref(), all)?
            );
        }
        Action::Create(args) => emit(&origami_core::create(&engine, &args.spec()?)?),
        Action::Validate(args) => {
            let dir = origami_core::directory(&args.dir)?;
            emit(&origami_core::validate(&engine, &dir)?);
        }
        Action::Upgrade { dir, prom, io_prom } => {
            let dir = origami_core::directory(&dir)?;
            match origami_core::upgrade(&engine, &dir, prom.as_deref(), io_prom.as_deref()) {
                Ok(report) => emit(&report),
                Err(incomplete) => {
                    emit(&incomplete.report);
                    return Err(incomplete.error);
                }
            }
        }
        Action::Show(args) => {
            let dir = origami_core::directory(&args.dir)?;
            match origami_core::show(&engine, &dir) {
                Ok(report) => emit(&report),
                Err(incomplete) => {
                    emit(&incomplete.report);
                    return Err(incomplete.error);
                }
            }
        }
        Action::ShowCommand(args) => {
            let dir = origami_core::directory(&args.machine.dir)?;
            let loaded = origami_core::open(&engine, &dir)?;
            let (machines, file) = (&loaded.catalogue, &loaded.file);
            let offer = loaded.offering()?;
            let display = args.display.resolve(!file.pipe.is_empty())?;
            let exe = rt.qemu();
            let ports = (!file.share.is_empty() && file.network.mode != "private")
                .then(shares::Ports::placeholder);
            let arguments =
                runtime::arguments_with(machines, &dir, file, offer, display, ports.as_ref())?;
            let mut arguments = arguments;
            arguments.splice(0..0, rt.data_arguments());
            runtime::append_extra_args(&mut arguments, file, &args.qemu_args)?;
            // One line, runnable as printed: one shell word per argument.
            print!("{}", runtime::shell_quote(&exe.display().to_string()));
            for arg in arguments {
                print!(" {}", runtime::shell_quote(&arg));
            }
            println!();
        }
        Action::Run(args) => {
            let dir = origami_core::directory(&args.launch.machine.dir)?;
            emit(&origami_core::apply_first_boot(&engine, &dir)?);
            let loaded = origami_core::open(&engine, &dir)?;
            let (machines, file) = (&loaded.catalogue, &loaded.file);
            let offer = loaded.offering()?;
            let display = args.launch.display.resolve(!file.pipe.is_empty())?;
            match display {
                runtime::Display::Vnc { port } => {
                    let servers = runtime::vnc_ports(machines, offer, file, port)?;
                    if servers.len() > 1 {
                        for (head, port) in servers {
                            println!("vnc 127.0.0.1:{port}: {} head {}", head.slot, head.head);
                        }
                    }
                }
                runtime::Display::Local => {
                    let heads = graphics::heads(machines, &offer.topology, &file.pipe);
                    if heads.len() > 1 {
                        println!("{} windows, one for each head", heads.len());
                    }
                }
                runtime::Display::Shm { .. } | runtime::Display::None => (),
            }
            if args.background {
                // The runner process warns into its log; say it here too.
                for warning in origami_machine::emu_id::running_twin_warnings(&dir, file) {
                    eprintln!("warning: {warning}");
                }
                let start = runtime::Start {
                    hold: args.hold,
                    no_audio: args.no_audio,
                    started_by: "cli",
                };
                let pid = runtime::start_background_with(
                    &std::env::current_exe()?,
                    &dir,
                    display,
                    &args.launch.qemu_args,
                    &start,
                )?;
                println!("running: pid {pid}");
            } else {
                let exit = origami_host::serve(origami_host::ServeArgs {
                    rt: &rt,
                    machines,
                    dir: &dir,
                    file,
                    offering: offer,
                    display,
                    once: &args.launch.qemu_args,
                    started_by: "cli",
                    foreground: true,
                    options: runtime::LaunchOptions {
                        hold: args.hold,
                        no_audio: args.no_audio,
                    },
                })?;
                if !exit.success() {
                    if let Some(hint) = runtime::network_hint(file) {
                        eprintln!("{hint}");
                    }
                    return Err(format!("QEMU exited with {exit}").into());
                }
            }
        }
        Action::Serve(args) => {
            let launch = &args.launch;
            let dir = origami_core::directory(&launch.machine.dir)?;
            emit(&origami_core::apply_first_boot(&engine, &dir)?);
            let loaded = origami_core::open(&engine, &dir)?;
            let (machines, file) = (&loaded.catalogue, &loaded.file);
            let offer = loaded.offering()?;
            let display = launch.display.resolve(!file.pipe.is_empty())?;
            let exit = origami_host::serve(origami_host::ServeArgs {
                rt: &rt,
                machines,
                dir: &dir,
                file,
                offering: offer,
                display,
                once: &launch.qemu_args,
                started_by: &args.started_by,
                foreground: false,
                options: runtime::LaunchOptions {
                    hold: args.hold,
                    no_audio: args.no_audio,
                },
            })?;
            if !exit.success() {
                return Err(format!("QEMU exited with {exit}").into());
            }
        }
        Action::Status(args) => {
            let dir = origami_core::directory(&args.dir)?;
            if let Some(record) = origami_qemu::control::running(&dir)? {
                if origami_host::client::runner_gone(&dir, &record)? {
                    emit(&Report::one(format!(
                        "runner gone, QEMU pid {} still running",
                        record.qemu_pid
                    )));
                } else {
                    emit(&Status::Running { pid: record.pid }.report());
                }
            } else if control::is_locked(&dir)? {
                emit(&Status::Starting.report());
            } else {
                emit(&Status::Stopped.report());
            }
        }
        Action::Console(args) => {
            origami_host::client::console(&origami_core::directory(&args.dir)?)?
        }
        Action::Stop(args) => {
            let dir = origami_core::directory(&args.dir)?;
            if args.force {
                origami_qemu::control::force_stop(&dir)?;
            } else {
                origami_host::client::stop(&dir)?;
            }
            emit(&Report::one("stopped"));
        }
        Action::Reset(args) => {
            origami_qemu::control::reset(&origami_core::directory(&args.dir)?)?;
            emit(&Report::one("reset"));
        }
        Action::GraphicsSet { dir, graphics } => {
            change(&engine, &dir, Change::SetGraphics(graphics.request()?))?
        }
        Action::DriveCreate { dir, size } => {
            change(&engine, &dir, Change::CreateDisk { size_mib: size })?
        }
        Action::DriveAttach {
            dir,
            file,
            kind,
            target,
            overlay,
        } => change(
            &engine,
            &dir,
            Change::AttachDrive(DriveRequest {
                file,
                kind: kind.into(),
                target,
                overlay,
            }),
        )?,
        Action::DriveDetach { dir, name } => change(&engine, &dir, Change::DetachDrive(name))?,
        Action::MachineSet {
            dir,
            processor,
            cpus_per_node,
            memory_per_node,
            settings,
        } => {
            let dir = origami_core::directory(&dir)?;
            let mut changes = vec![];
            if processor.is_some() || cpus_per_node.is_some() {
                changes.push(Change::SetProcessor {
                    module: processor,
                    cpus_per_node,
                });
            }
            changes.extend(memory_per_node.map(Change::SetMemory));
            if !settings.is_empty() {
                let mut named = BTreeMap::new();
                for (name, value) in settings {
                    if named.insert(name.clone(), value).is_some() {
                        return Err(format!("--set {name} given twice").into());
                    }
                }
                changes.push(Change::SetSettings(named));
            }
            emit(&origami_core::apply_changes(&engine, &dir, &changes)?);
        }
        Action::ConsoleSet { dir, port } => change(&engine, &dir, Change::SetConsole(port))?,
        Action::Nvram(args) => {
            let dir = origami_core::directory(&args.dir)?;
            match args.action {
                None => emit(&origami_core::nvram_read(&engine, &dir, None)?),
                Some(NvramAction::Get { name }) => {
                    emit(&origami_core::nvram_read(&engine, &dir, Some(&name))?)
                }
                Some(NvramAction::Set { settings }) => emit(&origami_core::apply_change(
                    &engine,
                    &dir,
                    &Change::SetNvram(settings),
                )?),
            }
        }
        Action::NetworkSet {
            dir,
            mode,
            endpoint,
            interface,
            share_address,
            subnet,
            mac,
        } => change(
            &engine,
            &dir,
            Change::SetNetwork(cli::network_change(
                mode,
                endpoint,
                interface,
                share_address,
                subnet,
                mac,
            )?),
        )?,
        Action::Share {
            dir,
            name,
            host_dir,
            remove,
        } => {
            let dir = origami_core::directory(&dir)?;
            let edit = if remove {
                Change::RemoveShare(name)
            } else {
                Change::AddShare {
                    name,
                    host_dir: host_dir.ok_or("share needs a host folder")?,
                }
            };
            emit(&origami_core::apply_change(&engine, &dir, &edit)?);
        }
        Action::NetworkForwardAdd {
            dir,
            name,
            protocol,
            host_port,
            guest_port,
        } => change(
            &engine,
            &dir,
            Change::AddForward(origami_machine::PortForward {
                name,
                protocol: protocol.as_str().into(),
                host_port,
                guest_port,
            }),
        )?,
        Action::NetworkForwardRemove { dir, name } => {
            change(&engine, &dir, Change::RemoveForward(name))?
        }
        Action::InstallInit {
            dir,
            media_root,
            mac,
            profile,
            release,
        } => {
            if let Some(old) = profile.old_name() {
                return Err(
                    format!("--profile {old} is now --profile {}", profile.as_str()).into(),
                );
            }
            let dir = origami_core::directory(&dir)?;
            let init = InstallInit {
                media_root,
                mac,
                release: release.as_str().into(),
                profile: profile.as_str().into(),
            };
            emit(&origami_core::install_init(&engine, &dir, &init)?);
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
            let dir = origami_core::directory(&dir)?;
            let addon = InstallAddon {
                name,
                source,
                install: selections,
                base,
                dist,
                media_root,
            };
            emit(&origami_core::install_addon(&engine, &dir, &addon)?);
        }
        Action::InstallApply { dir } => {
            let dir = origami_core::directory(&dir)?;
            let loaded = origami_core::open(&engine, &dir)?;
            install::apply(
                &dir,
                &loaded.file,
                &origami_qemu::connect_console,
                &mut std::io::stdout(),
            )?;
        }
        Action::InstallCheck { machine, full } => {
            let dir = origami_core::directory(&machine.dir)?;
            emit(&origami_core::install_check(&engine, &dir, full, || {
                rt.instigator().map(Path::to_path_buf)
            })?);
        }
        Action::InstallServe(args) => {
            let dir = origami_core::directory(&args.dir)?;
            let loaded = origami_core::open(&engine, &dir)?;
            let status = install::serve(|| Ok(rt.instigator()?.to_path_buf()), &dir, &loaded.file)?;
            if !status.success() {
                return Err(format!("Instigator exited with {status}").into());
            }
        }
    }
    Ok(())
}
