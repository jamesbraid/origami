//! origami-gui: a tab for each running machine, any tab poppable into its own
//! window and docked back, over the product's own runner.

mod core;
mod keymap;
mod pointer;
mod qemu;
mod termkeys;
mod textgrid;
mod ui;
#[cfg(test)]
mod zero_copy_tests;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use origami_core::facade::{Config, Core, OperationState, StopKind};

slint::include_modules!();

const USAGE: &str = "usage: origami-gui [options] MACHINE_DIR...

Starts each machine (or attaches to one already running) with the shared-memory
display and shows it in a tab. Closing the window leaves the machines running;
stop them with `origami stop DIR`, or pass --stop-on-exit.

options:
  --stop-on-exit        stop the machines this run started when the window closes
  --debug               show the display protocol's state and the counters
                        under each machine (the Debug button toggles it)
  --view NAME           the view each tab starts in: display (the default),
                        serial or logs
  --pointer MODE        how the mouse reaches the guest: capture (the default;
                        click the picture to capture it, Ctrl+Alt, or
                        Ctrl+Option on a Mac, to release it) or absolute
                        (follow the pointer's place in the picture; the
                        Absolute button toggles it)
  --stats-file PATH     write counters and each machine's views as JSON to
                        PATH twice a second
  --popout-cycles N     pop the first machine out and dock it N times, then quit
                        (a test hook)
  --gpu-info            print the adapter, whether it can import QEMU's memory
                        without a copy, and the `--display-gpu` value that
                        asks QEMU to export on it, then quit
  -h, --help            this text

The environment variable ORIGAMI_GUI_ZERO_COPY=0 keeps the GPU from importing
memory, so the changed lines are copied instead.";

struct Args {
    dirs: Vec<PathBuf>,
    stop_on_exit: bool,
    stats_file: Option<PathBuf>,
    popout_cycles: u32,
    gpu_info: bool,
    debug: bool,
    view: ui::View,
    pointer: pointer::PointerMode,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        dirs: Vec::new(),
        stop_on_exit: false,
        stats_file: None,
        popout_cycles: 0,
        gpu_info: false,
        debug: false,
        view: ui::View::Display,
        pointer: pointer::PointerMode::Capture,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--stop-on-exit" => a.stop_on_exit = true,
            "--gpu-info" => a.gpu_info = true,
            "--debug" => a.debug = true,
            "--view" => {
                a.view = match it.next().as_deref() {
                    Some("display") => ui::View::Display,
                    Some("serial") => ui::View::Serial,
                    Some("logs") => ui::View::Logs,
                    _ => return Err("--view needs display, serial or logs".into()),
                }
            }
            "--pointer" => {
                a.pointer = match it.next().as_deref() {
                    Some("capture") => pointer::PointerMode::Capture,
                    Some("absolute") => pointer::PointerMode::Absolute,
                    _ => return Err("--pointer needs capture or absolute".into()),
                }
            }
            "--stats-file" => {
                a.stats_file = Some(it.next().ok_or("--stats-file needs a path")?.into())
            }
            "--popout-cycles" => {
                a.popout_cycles = it
                    .next()
                    .and_then(|n| n.parse().ok())
                    .ok_or("--popout-cycles needs a number")?
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s}\n{USAGE}")),
            _ => a.dirs.push(arg.into()),
        }
    }
    if a.dirs.is_empty() && !a.gpu_info {
        return Err(USAGE.into());
    }
    Ok(a)
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("origami-gui: {e}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<(), String> {
    let args = parse_args()?;
    let gpu = origami_render::Gpu::new()?;
    eprintln!("origami-gui: GPU {}", gpu.describe());
    if args.gpu_info {
        println!("adapter: {}", gpu.describe());
        println!("zero copy: {}", gpu.import.describe());
        if let Some(uuid) = gpu.import.export_uuid() {
            println!("display-gpu: {uuid}");
        }
        return Ok(());
    }

    let runner = qemu::RunnerEngine::locate_runner().map_err(|e| e.to_string())?;
    let runtime = origami_qemu::Runtime::locate().map_err(|e| e.to_string())?;
    let engine = Arc::new(qemu::RunnerEngine::new(runtime, runner));
    // Before any machine starts: with an importer the machine is started to
    // export its scanout memory on this adapter, and its regions are imported.
    eprintln!("origami-gui: zero copy: {}", gpu.import.describe());
    if let Some(importer) = origami_render::GpuImporter::new(&gpu) {
        engine.set_importer(importer);
    }
    let core =
        Core::new(Config::new("."), vec![engine.clone()], None).map_err(|e| e.to_string())?;
    let mut machines = Vec::new();
    for dir in &args.dirs {
        let machine = core
            .start_machine(&dir.to_string_lossy())
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        machines.push(machine);
    }

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_custom_application_handler(ui::RawPointer)
        .renderer_name("femtovg-wgpu".into())
        .require_wgpu_30(slint::wgpu_30::WGPUConfiguration::Manual {
            instance: gpu.instance.clone(),
            adapter: gpu.adapter.clone(),
            device: gpu.device.clone(),
            queue: gpu.queue.clone(),
        })
        .select()
        .map_err(|e| format!("cannot start the window system: {e}"))?;

    let counters = ui::install(
        engine.clone(),
        gpu.clone(),
        machines.clone(),
        args.debug,
        args.view,
        args.pointer,
    );

    let write_stats = {
        let counters = counters.clone();
        let gpu = gpu.clone();
        let engine = engine.clone();
        let shown = machines.clone();
        move |path: &std::path::Path| {
            let c = &counters;
            let g = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
            let per: Vec<String> = shown
                .iter()
                .filter_map(|m| {
                    let dir = std::path::Path::new(m.key());
                    engine.stats(dir).map(|s| {
                        format!(
                            "{{\"frames\":{},\"rows\":{},\"imported_rows\":{},\"copied_bytes\":{},\"backend\":{:?},\"running\":{},\"head\":{},\"cursor\":{}}}",
                            s.frames,
                            s.lines,
                            s.imported_lines,
                            s.copied_bytes,
                            engine.display_backend(dir).unwrap_or_default(),
                            engine.is_running(dir),
                            m.heads().first().map_or(
                                "null".to_string(),
                                |h| format!("{{\"width\":{},\"height\":{}}}", h.width, h.height)
                            ),
                            m.cursor(0, u32::MAX).ok().flatten().map_or(
                                "null".to_string(),
                                |c| format!(
                                    "{{\"x\":{},\"y\":{},\"on\":{},\"hot_x\":{},\"hot_y\":{}}}",
                                    c.x, c.y, c.on, c.hot_x, c.hot_y
                                )
                            )
                        )
                    })
                })
                .collect();
            let text = format!(
                "{{\"wakes\":{},\"frames_taken\":{},\"rows_uploaded\":{},\"bytes_uploaded\":{},\"frames_imported\":{},\"rows_imported\":{},\"zero_copy\":{:?},\"before_renders\":{},\"passes\":{},\"popouts\":{},\"docks\":{},\"keys_sent\":{},\"moves_sent\":{},\"input_errors\":{},\"gpu_errors\":{},\"engine\":[{}],\"machines\":{}}}\n",
                g(&c.wakes), g(&c.frames_taken), g(&c.rows_uploaded), g(&c.bytes_uploaded),
                g(&c.frames_imported), g(&c.rows_imported), gpu.import.describe(), g(&c.before_renders),
                g(&c.passes), g(&c.popouts), g(&c.docks), g(&c.keys_sent), g(&c.moves_sent), g(&c.input_errors),
                gpu.errors.count(), per.join(","), ui::views_json()
            );
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    };
    let _stats_timer = args.stats_file.as_ref().map(|path| {
        let timer = slint::Timer::default();
        let (path, write) = (path.clone(), write_stats.clone());
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(500),
            move || write(&path),
        );
        timer
    });

    // A test hook: pop the first machine out and back, waiting for the
    // window that shows it to render each time.
    let _cycle_timer = (args.popout_cycles > 0).then(|| {
        let timer = slint::Timer::default();
        let cycles = args.popout_cycles;
        let stats = args.stats_file.clone();
        let write = write_stats.clone();
        let mut state = Cycle {
            done: 0,
            waiting_for: None,
            since: std::time::Instant::now(),
            quitting: false,
        };
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(20),
            move || {
                let (finished, write, stats) = (state.step(cycles), write.clone(), stats.clone());
                if finished && !state.quitting {
                    state.quitting = true;
                    ui::with_ui(move |u| {
                        eprintln!(
                            "origami-gui: {} pop-out/dock cycles, {} GPU errors",
                            u.counters.popouts.load(Ordering::Relaxed),
                            u.gpu_errors()
                        );
                        if let Some(p) = &stats {
                            write(p);
                        }
                        // Leave the window up a moment so a test can look at it.
                        slint::Timer::single_shot(Duration::from_secs(2), || {
                            let _ = slint::quit_event_loop();
                        });
                    });
                }
            },
        );
        timer
    });

    ui::run();
    ui::shutdown();
    if let Some(path) = &args.stats_file {
        write_stats(path);
    }
    if args.stop_on_exit {
        for m in &machines {
            eprintln!("origami-gui: stopping {}", m.name());
            let result = m.stop(StopKind::Clean).map(|op| op.wait(None));
            match result {
                Ok(OperationState::Failed(e)) | Err(e) => {
                    eprintln!("origami-gui: stopping {}: {e}", m.name());
                }
                Ok(_) => {}
            }
        }
    }
    core.shutdown();
    Ok(())
}

/// The pop-out loop: out, wait for the pop-out to render, back, wait for the
/// main window to render, and again.
struct Cycle {
    done: u32,
    /// (renders to beat, whether we are waiting on the pop-out)
    waiting_for: Option<(u64, bool)>,
    since: std::time::Instant,
    quitting: bool,
}

impl Cycle {
    /// One timer tick. True once all cycles are done.
    fn step(&mut self, cycles: u32) -> bool {
        let (done, waiting) = (self.done, self.waiting_for);
        let since = self.since;
        let mut next: Option<(Option<(u64, bool)>, u32)> = None;
        ui::with_ui_now(|u| {
            if u.machine_count() == 0 {
                return;
            }
            match waiting {
                None if done < cycles && since.elapsed() > Duration::from_millis(1500) => {
                    u.popout(0);
                    next = Some((Some((u.renders(0), true)), done));
                }
                Some((beat, true))
                    if u.renders(0) > beat || since.elapsed() > Duration::from_secs(3) =>
                {
                    u.dock(0);
                    next = Some((Some((u.renders(0), false)), done));
                }
                Some((beat, false))
                    if u.renders(0) > beat || since.elapsed() > Duration::from_secs(3) =>
                {
                    next = Some((None, done + 1));
                }
                _ => {}
            }
        });
        if let Some((w, d)) = next {
            self.waiting_for = w;
            self.done = d;
            self.since = std::time::Instant::now();
            if w.is_none() && d == 0 {
                self.since = std::time::Instant::now();
            }
        }
        self.done >= cycles
    }
}
