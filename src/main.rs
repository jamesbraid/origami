mod cli;

use clap::Parser;
use cli::{Action, NvramAction};
use origami_core::facade::{connect_install_console, InstallStart};
use origami_core::{Change, DriveRequest, InstallAddon, InstallInit, Report, Status, Stream};
use origami_engine::Engine;
use origami_iris::IrisEngine;
use origami_machine::{control, graphics, install, shares, Result};
use origami_qemu::{runtime, QemuEngine};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::Arc;

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

/// The engines this executable runs machines on.
struct Engines {
    qemu: QemuEngine,
    iris: IrisEngine,
}

impl Engines {
    fn new(rt: &runtime::Runtime) -> Result<Self> {
        Ok(Self {
            qemu: QemuEngine::new(rt.clone()),
            iris: IrisEngine::new(std::env::current_exe()?),
        })
    }

    /// The engine the machine in `dir` names; QEMU where the file cannot be
    /// read, so its own errors speak.
    fn of(&self, dir: &Path) -> &dyn Engine {
        match origami_machine::read_machine(dir) {
            Ok(file) if file.engine.id == origami_iris::ENGINE => &self.iris,
            _ => &self.qemu,
        }
    }

    /// The engine that offers `preset`.
    fn offering(&self, preset: &str) -> &dyn Engine {
        let offered = |engine: &dyn Engine| {
            engine
                .catalogue()
                .is_ok_and(|machines| origami_machine::preset(&machines, preset).is_ok())
        };
        if offered(&self.iris) {
            &self.iris
        } else {
            &self.qemu
        }
    }
}

/// Apply one edit to the machine in `dir` and print what it says.
fn change(engines: &Engines, dir: &Path, change: Change) -> Result<()> {
    let dir = origami_core::directory(dir)?;
    emit(&origami_core::apply_change(
        engines.of(&dir),
        &dir,
        &change,
    )?);
    Ok(())
}

/// Host the iris machine in `dir` in this process, then leave without the
/// exit handlers: tearing down a stopped iris machine can fault.
#[cfg(feature = "iris")]
fn serve_iris(
    dir: &Path,
    machine_file: Option<&Path>,
    started_by: &str,
    foreground: bool,
    hold: bool,
    display: bool,
) -> Result<()> {
    use std::io::Write;
    let exit =
        origami_iris::runner::serve(dir, machine_file, started_by, foreground, hold, display)?;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    origami_iris::runner::exit_now(if exit.success() { 0 } else { 1 })
}

#[cfg(not(feature = "iris"))]
fn serve_iris(_: &Path, _: Option<&Path>, _: &str, _: bool, _: bool, _: bool) -> Result<()> {
    Err("this origami was built without iris; rebuild it with --features iris".into())
}

fn command(action: Action) -> Result<()> {
    let rt = runtime::Runtime::locate()?;
    if matches!(action, Action::Version) {
        version_report(&rt);
        return Ok(());
    }
    let engines = Engines::new(&rt)?;
    match action {
        Action::Version => unreachable!(),
        // The list is QEMU's. iris's machines are experimental: named, they
        // show and create like any other.
        Action::Machines { preset, all } => {
            let engine = preset
                .as_deref()
                .map_or(&engines.qemu as &dyn Engine, |p| engines.offering(p));
            print!("{}", origami_core::listing(engine, preset.as_deref(), all)?);
        }
        Action::Create(args) => {
            let spec = args.spec()?;
            emit(&origami_core::create(
                engines.offering(&args.preset),
                &spec,
            )?)
        }
        Action::Validate(args) => {
            let dir = origami_core::directory(&args.dir)?;
            emit(&origami_core::validate(engines.of(&dir), &dir)?);
        }
        Action::Upgrade { dir, prom, io_prom } => {
            let dir = origami_core::directory(&dir)?;
            match origami_core::upgrade(engines.of(&dir), &dir, prom.as_deref(), io_prom.as_deref())
            {
                Ok(report) => emit(&report),
                Err(incomplete) => {
                    emit(&incomplete.report);
                    return Err(incomplete.error);
                }
            }
        }
        Action::Show(args) => {
            let dir = origami_core::directory(&args.dir)?;
            match origami_core::show(engines.of(&dir), &dir) {
                Ok(report) => emit(&report),
                Err(incomplete) => {
                    emit(&incomplete.report);
                    return Err(incomplete.error);
                }
            }
        }
        Action::ShowCommand(args) => {
            let dir = origami_core::directory(&args.machine.dir)?;
            let loaded = origami_core::open(engines.of(&dir), &dir)?;
            let (machines, file) = (&loaded.catalogue, &loaded.file);
            if file.engine.id != origami_machine::model::QEMU_ENGINE {
                return Err(format!("{} machines have no QEMU command", file.engine.id).into());
            }
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
            match origami_core::apply_first_boot(engines.of(&dir), &dir) {
                Ok(report) => emit(&report),
                Err(error) => {
                    eprintln!("warning: could not apply the first-boot settings: {error}")
                }
            }
            let loaded = origami_core::open(engines.of(&dir), &dir)?;
            let (machines, file) = (&loaded.catalogue, &loaded.file);
            let offer = loaded.offering()?;
            let display = args.launch.display.resolve(!file.pipe.is_empty())?;
            let iris = file.engine.id == origami_iris::ENGINE;
            if iris && !args.launch.qemu_args.is_empty() {
                return Err("--qemu-arg does not apply to an iris machine".into());
            }
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
                runtime::Display::Shm { .. }
                | runtime::Display::ShmV4 { .. }
                | runtime::Display::None => (),
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
            } else if iris {
                let shared = matches!(display, runtime::Display::Shm { .. });
                serve_iris(&dir, None, "cli", true, args.hold, shared)?;
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
            if args.machine_file.is_some() {
                let shared = matches!(launch.display.resolve(false)?, runtime::Display::Shm { .. });
                return serve_iris(
                    &dir,
                    args.machine_file.as_deref(),
                    &args.started_by,
                    false,
                    args.hold,
                    shared,
                );
            }
            match origami_core::apply_first_boot(engines.of(&dir), &dir) {
                Ok(report) => emit(&report),
                Err(error) => {
                    eprintln!("warning: could not apply the first-boot settings: {error}")
                }
            }
            let loaded = origami_core::open(engines.of(&dir), &dir)?;
            let (machines, file) = (&loaded.catalogue, &loaded.file);
            let offer = loaded.offering()?;
            if file.engine.id == origami_iris::ENGINE {
                let shared = matches!(launch.display.resolve(false)?, runtime::Display::Shm { .. });
                return serve_iris(&dir, None, &args.started_by, false, args.hold, shared);
            }
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
            if let Some(record) = origami_host::client::running(&dir)? {
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
                emit(&Status::Stopped(control::read_exit(&dir)).report());
            }
        }
        Action::Console(args) => {
            origami_host::client::console(&origami_core::directory(&args.dir)?)?
        }
        Action::Stop(args) => {
            let dir = origami_core::directory(&args.dir)?;
            // A machine that never ran, or has ended, has no run record.
            if control::read(&dir).is_err_and(|e| {
                e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            }) {
                emit(&Report::one("not running"));
            } else {
                if args.force {
                    origami_host::client::force_stop(&dir)?;
                } else {
                    origami_host::client::stop(&dir)?;
                }
                emit(&Report::one("stopped"));
            }
        }
        Action::Reset(args) => {
            origami_host::client::reset(&origami_core::directory(&args.dir)?)?;
            emit(&Report::one("reset"));
        }
        Action::GraphicsSet { dir, graphics } => {
            change(&engines, &dir, Change::SetGraphics(graphics.request()?))?
        }
        Action::DriveCreate { dir, size } => {
            change(&engines, &dir, Change::CreateDisk { size_mib: size })?
        }
        Action::DriveAttach {
            dir,
            file,
            kind,
            target,
            overlay,
        } => change(
            &engines,
            &dir,
            Change::AttachDrive(DriveRequest {
                file,
                kind: kind.into(),
                target,
                overlay,
            }),
        )?,
        Action::DriveDetach { dir, name } => change(&engines, &dir, Change::DetachDrive(name))?,
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
            emit(&origami_core::apply_changes(
                engines.of(&dir),
                &dir,
                &changes,
            )?);
        }
        Action::ConsoleSet { dir, port } => change(&engines, &dir, Change::SetConsole(port))?,
        Action::Nvram(args) => {
            let dir = origami_core::directory(&args.dir)?;
            match args.action {
                None => emit(&origami_core::nvram_read(engines.of(&dir), &dir, None)?),
                Some(NvramAction::Get { name }) => emit(&origami_core::nvram_read(
                    engines.of(&dir),
                    &dir,
                    Some(&name),
                )?),
                Some(NvramAction::Set { settings }) => emit(&origami_core::apply_change(
                    engines.of(&dir),
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
            &engines,
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
            emit(&origami_core::apply_change(engines.of(&dir), &dir, &edit)?);
        }
        Action::NetworkForwardAdd {
            dir,
            name,
            protocol,
            host_port,
            guest_port,
        } => change(
            &engines,
            &dir,
            Change::AddForward(origami_machine::PortForward {
                name,
                protocol: protocol.as_str().into(),
                host_port,
                guest_port,
            }),
        )?,
        Action::NetworkForwardRemove { dir, name } => {
            change(&engines, &dir, Change::RemoveForward(name))?
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
            emit(&origami_core::install_init(engines.of(&dir), &dir, &init)?);
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
            emit(&origami_core::install_addon(
                engines.of(&dir),
                &dir,
                &addon,
            )?);
        }
        Action::InstallApply { dir } => {
            let dir = origami_core::directory(&dir)?;
            let loaded = origami_core::open(engines.of(&dir), &dir)?;
            // The lease turns other viewers read-only until this ends.
            install::apply(
                &dir,
                &loaded.file,
                &|dir| {
                    let attached = InstallStart::Attached;
                    connect_install_console(dir, attached, Arc::new(|| false))
                },
                &mut std::io::stdout(),
                false,
            )?;
        }
        Action::InstallCheck { machine, full } => {
            let dir = origami_core::directory(&machine.dir)?;
            emit(&origami_core::install_check(
                engines.of(&dir),
                &dir,
                full,
                || rt.instigator().map(Path::to_path_buf),
            )?);
        }
        Action::InstallServe(args) => {
            let dir = origami_core::directory(&args.dir)?;
            let loaded = origami_core::open(engines.of(&dir), &dir)?;
            let status = install::serve(|| Ok(rt.instigator()?.to_path_buf()), &dir, &loaded.file)?;
            if !status.success() {
                return Err(format!("Instigator exited with {status}").into());
            }
        }
    }
    Ok(())
}
