//! The runner half: builds the machine with iris linked in and hosts it for
//! `origami _serve`. Needs the `runner` feature.

use crate::config::{self, Ports};
use crate::session::CONSOLE;
use crate::{catalogue, ENGINE};
use iris::gdb_stub::CpuDebug;
use iris::gfx_display::GfxDisplay;
use iris::workstation::Workstation;
use origami_engine::{
    ConsoleInfo, ConsoleIo, Control, DisplayEndpoint, Facts, MachineId, Result, RunState, Session,
    Stop, Wake,
};
use origami_host::{Exit, Hosted};
use origami_machine::control::lock_for_edit;
use origami_machine::dir::MachineDir;
use origami_machine::{read_machine, MachineFile};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a screen dump waits for the PROM to set a video mode.
const FIRST_PICTURE: Duration = Duration::from_secs(30);

/// Host the iris machine in `dir` until it exits. It is the machine in
/// `machine_file` when one is given, else the one `machine.toml` describes.
pub fn serve(
    dir: &Path,
    machine_file: Option<&Path>,
    started_by: &str,
    foreground: bool,
    hold: bool,
    display: bool,
) -> Result<Exit> {
    install_panic_exit();
    let file = match machine_file {
        Some(path) => MachineFile::parse(&std::fs::read_to_string(path)?)?,
        None => read_machine(dir)?,
    };
    let catalogue = catalogue::catalogue();
    let offering = origami_machine::validate(&catalogue, dir, &file)?;
    config::check(&file)?;
    let lock = lock_for_edit(dir, "starting it again")?;
    let mac = file
        .identity
        .as_ref()
        .map_or(crate::nvram::DEFAULT_MAC, |i| &i.mac);
    let mac = crate::nvram::parse_mac(mac).ok_or("the identity MAC is malformed")?;
    for item in &offering.storage {
        if let Some(kind) = crate::nvram::Kind::of(&item.initial) {
            crate::nvram::seed(
                &origami_machine::stores::state_path(dir, item),
                kind,
                mac,
                config::prom_console(&file),
            )?;
        }
    }
    let ports = Ports::free()?;
    let text = config::iris_toml(offering, &file, &MachineDir::new(dir), ports)?;
    let removable = file
        .drive
        .iter()
        .filter(|drive| drive.kind == "cdrom")
        .map(|drive| (drive.name.clone(), drive.target as usize))
        .collect::<Vec<_>>();
    // A guest power-off ends the machine, not this process: the host reports it.
    iris::workstation::set_process_exit_enabled(false);
    let display = display.then(|| crate::display_socket(dir));
    let name = format!("iris-{}", std::process::id());
    origami_host::host(dir, started_by, foreground, |wake| {
        let machine = build(&text)?;
        let session = Arc::new(LocalSession::start(
            machine,
            removable.clone(),
            hold,
            wake,
            display.as_deref(),
        )?);
        Ok(Hosted {
            session,
            engine: ENGINE,
            engine_console: ports.console,
            qmp_port: 0,
            name,
            engine_pid: 0,
            removable: removable.into_iter().map(|(name, _)| name).collect(),
            connect_console: true,
            keep: Box::new(lock),
        })
    })
}

#[cfg(target_os = "linux")]
type Published = origami_display_producer::Producer;
#[cfg(not(target_os = "linux"))]
type Published = std::convert::Infallible;

#[cfg(target_os = "linux")]
fn publish_display(
    display: &Arc<dyn GfxDisplay>,
    machine: &dyn Workstation,
    path: &Path,
) -> Result<Published> {
    Ok(crate::publish::start(
        display.clone(),
        machine.get_ps2(),
        path,
    )?)
}

#[cfg(not(target_os = "linux"))]
fn publish_display(_: &Arc<dyn GfxDisplay>, _: &dyn Workstation, _: &Path) -> Result<Published> {
    Err("publishing an iris display needs Linux".into())
}

/// A panic on any iris thread ends the runner at once with its message in
/// the log, instead of leaving a machine with a dead thread running.
fn install_panic_exit() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("iris runner panicked: {info}");
        // SAFETY: _exit takes no pointers; it skips the exit handlers a half
        // torn-down iris would trip over.
        unsafe { libc::_exit(101) }
    }));
}

/// Build the machine on a thread with the stack iris needs; a bad
/// configuration panics inside iris, which becomes an error here.
fn build(text: &str) -> Result<Box<dyn Workstation>> {
    let text = text.to_owned();
    let built = std::thread::Builder::new()
        .name("iris-build".into())
        .stack_size(256 << 20)
        .spawn(
            move || -> std::result::Result<Box<dyn Workstation>, String> {
                let mut cfg: iris::config::MachineConfig =
                    toml::from_str(&text).map_err(|e| e.to_string())?;
                cfg.settle_jitv2_threads();
                iris::workstation::build(cfg)
            },
        )?
        .join()
        .map_err(|_| "iris panicked while building the machine")?;
    Ok(built?)
}

enum Phase {
    /// Built and not yet released.
    Prelaunch,
    Running,
    Paused,
    Exited {
        code: Option<i32>,
        last_error: Option<String>,
    },
}

/// iris's machine as a session, inside the runner.
pub struct LocalSession {
    machine: Mutex<Option<Box<dyn Workstation>>>,
    cpu: Arc<dyn CpuDebug>,
    display: Option<Arc<dyn GfxDisplay>>,
    /// The display protocol 4 producer, until the machine stops.
    published: Mutex<Option<Published>>,
    removable: Vec<(String, usize)>,
    /// Held across a transition, so a status read never sees a reset or a
    /// resume half done as a guest power-off.
    phase: Mutex<Phase>,
    wake: Wake,
}

impl LocalSession {
    fn start(
        mut machine: Box<dyn Workstation>,
        removable: Vec<(String, usize)>,
        hold: bool,
        wake: Wake,
        publish: Option<&Path>,
    ) -> Result<Self> {
        machine.register_system_controller();
        let cpu = machine.get_cpu_debug();
        let display = machine.get_display();
        let published = match (publish, &display) {
            (Some(path), Some(display)) => Some(publish_display(display, &*machine, path)?),
            (Some(_), None) => return Err("this machine has no display to publish".into()),
            _ => None,
        };
        machine.start();
        // Start paused after the fact: iris starts the CPU with the machine.
        if hold {
            cpu.stop();
        }
        Ok(Self {
            machine: Mutex::new(Some(machine)),
            cpu,
            display,
            published: Mutex::new(published),
            removable,
            phase: Mutex::new(if hold {
                Phase::Prelaunch
            } else {
                Phase::Running
            }),
            wake,
        })
    }

    fn machine<R>(&self, f: impl FnOnce(&mut dyn Workstation) -> R) -> Result<R> {
        let mut guard = self.machine.lock().unwrap_or_else(|e| e.into_inner());
        let machine = guard.as_mut().ok_or("the machine has exited")?;
        Ok(f(machine.as_mut()))
    }

    fn end(&self, phase: &mut Phase, code: Option<i32>, last_error: Option<String>) {
        *phase = Phase::Exited { code, last_error };
        (self.wake)(MachineId(0));
    }
}

impl Session for LocalSession {
    fn id(&self) -> MachineId {
        MachineId(0)
    }

    fn display(&self) -> DisplayEndpoint {
        DisplayEndpoint::None
    }

    fn consoles(&self) -> Vec<ConsoleInfo> {
        vec![ConsoleInfo {
            name: CONSOLE.into(),
            primary: true,
        }]
    }

    fn open_console(&self, _name: &str) -> Result<Box<dyn ConsoleIo>> {
        Err("the runner serves the console through its broker".into())
    }

    fn control(&self, c: Control) -> Result<()> {
        let mut phase = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*phase, Phase::Exited { .. }) {
            return Err("the machine has exited".into());
        }
        match c {
            Control::Reset => {
                let fault = self.machine(|m| {
                    m.reset();
                    m.fatal_fault().map(|f| f.to_string())
                })?;
                if let Some(fault) = fault {
                    return Err(format!("iris cannot reset the machine: {fault}").into());
                }
                // A reset keeps a paused or held machine paused or held.
                if matches!(*phase, Phase::Running) {
                    self.cpu.start();
                }
            }
            Control::Pause => {
                if matches!(*phase, Phase::Running) {
                    self.cpu.stop();
                    *phase = Phase::Paused;
                }
            }
            Control::Resume => {
                if !matches!(*phase, Phase::Running) {
                    self.cpu.start();
                    *phase = Phase::Running;
                }
            }
            Control::PowerButton => return Err("iris does not model a power button".into()),
            Control::Nmi => return Err("iris does not model an NMI".into()),
        }
        Ok(())
    }

    fn media(&self, drive: &str, image: Option<&Path>) -> Result<()> {
        let Some(&(_, target)) = self.removable.iter().find(|(name, _)| name == drive) else {
            return Err(format!("{drive} is not a removable drive of this machine").into());
        };
        let changer = self
            .machine(|m| m.disc_changer())?
            .ok_or("this machine has no CD-ROM drive")?;
        match image {
            Some(image) => changer
                .load_disc(target, image.display().to_string())
                .map(drop),
            None => changer.eject_to_empty(target),
        }
        .map_err(Into::into)
    }

    fn screendump(&self, head: usize, path: &Path) -> Result<()> {
        if head != 0 {
            return Err("an iris machine has one head".into());
        }
        let display = self.display.as_ref().ok_or("this machine has no display")?;
        let deadline = Instant::now() + FIRST_PICTURE;
        let mut pixels = Vec::new();
        let (width, height) = loop {
            iris::gfx_display::wait_fresh_frame(display.as_ref(), Duration::from_millis(250));
            if let Some((w, h, _)) = iris::gfx_display::copy_frame(display.as_ref(), &mut pixels) {
                break (w, h);
            }
            if Instant::now() >= deadline {
                return Err("the machine has no picture yet".into());
            }
        };
        // 0xAABBGGRR words, R in the low byte, to a binary PPM.
        let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
        out.reserve(pixels.len() * 3);
        for p in &pixels {
            out.extend_from_slice(&[*p as u8, (*p >> 8) as u8, (*p >> 16) as u8]);
        }
        std::fs::write(path, out)?;
        Ok(())
    }

    fn stop(&self, how: Stop) -> Result<()> {
        // Closes the socket before the machine goes.
        drop(
            self.published
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
        );
        let mut phase = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*phase, Phase::Exited { .. }) {
            return Ok(());
        }
        let mut guard = self.machine.lock().unwrap_or_else(|e| e.into_inner());
        let mut flush = Ok(());
        if let Some(mut machine) = guard.take() {
            match how {
                // The order iris's own exit uses: flush, stop, flush.
                Stop::Clean { .. } => {
                    flush = machine.flush_disks();
                    machine.stop();
                    flush = flush.and(machine.flush_disks());
                }
                Stop::Force => machine.stop(),
            }
            // Dropping a stopped iris machine can fault on its way out; the
            // process is about to end, so leave it be.
            std::mem::forget(machine);
        }
        drop(guard);
        if let Err(error) = flush {
            let why = format!("flushing the disks failed, so guest writes may be lost: {error}");
            self.end(&mut phase, Some(1), Some(why.clone()));
            return Err(why.into());
        }
        let code = matches!(how, Stop::Clean { .. }).then_some(0);
        self.end(&mut phase, code, None);
        Ok(())
    }

    fn status(&self) -> Facts {
        let mut phase = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*phase, Phase::Running) {
            let fault = self
                .machine(|m| m.fatal_fault().map(|f| f.to_string()))
                .ok()
                .flatten();
            if let Some(fault) = fault {
                self.end(&mut phase, Some(1), Some(fault));
            } else if !self.cpu.is_running() {
                // Nothing here stopped it, so the guest powered itself off
                // (iris has already flushed the disks).
                self.end(&mut phase, Some(0), None);
            }
        }
        let state = match &*phase {
            Phase::Prelaunch => RunState::Prelaunch,
            Phase::Running => RunState::Running,
            Phase::Paused => RunState::Paused,
            Phase::Exited { code, last_error } => RunState::Exited {
                code: *code,
                last_error: last_error.clone(),
            },
        };
        Facts {
            state,
            pid: Some(std::process::id()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use origami_machine::model::Machine;

    #[test]
    fn the_generated_configuration_is_one_iris_accepts() {
        let catalogue = catalogue::catalogue();
        let offering = &catalogue.offerings[0];
        let mut machine =
            Machine::from_offering(offering, Some(256), None, vec![], Default::default());
        machine.engine.id = ENGINE.into();
        let text = config::iris_toml(
            offering,
            &machine.to_file(),
            &MachineDir::new("/m"),
            Ports::free().unwrap(),
        )
        .unwrap();
        let cfg: iris::config::MachineConfig = toml::from_str(&text).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.banks[..2], [128, 128]);
    }

    #[test]
    fn our_checksum_is_the_one_iris_writes() {
        let mut block = [0u8; 256];
        block[250..].copy_from_slice(&[8, 0, 0x69, 1, 2, 3]);
        iris::dev::sgi_nvram::write_defaults(&mut block, iris::dev::sgi_nvram::REV_IP22);
        // Same settings but the console, which ours puts on the graphics head.
        block[2] = b'g';
        block[0] = iris::dev::sgi_nvram::checksum(&block);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nvram.raw");
        std::fs::write(&path, vec![0u8; crate::nvram::SIZE as usize]).unwrap();
        crate::nvram::seed(&path, crate::nvram::Kind::Indy, [8, 0, 0x69, 1, 2, 3], b'g').unwrap();
        assert_eq!(std::fs::read(&path).unwrap()[64..64 + 256], block);
    }
}
