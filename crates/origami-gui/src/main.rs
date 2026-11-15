//! origami-gui: a tab for each running machine, any tab poppable into its own
//! window and docked back, over the product's own runner.

mod core;
mod gfx;
mod keymap;
mod qemu;
mod ui;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::core::Engine;

slint::include_modules!();

const USAGE: &str = "usage: origami-gui [options] MACHINE_DIR...

Starts each machine (or attaches to one already running) with the shared-memory
display and shows it in a tab. Closing the window leaves the machines running;
stop them with `origami stop DIR`, or pass --stop-on-exit.

options:
  --stop-on-exit        stop the machines this run started when the window closes
  --stats-file PATH     write counters as JSON to PATH twice a second
  --popout-cycles N     pop the first machine out and dock it N times, then quit
                        (a test hook)
  -h, --help            this text";

struct Args {
    dirs: Vec<PathBuf>,
    stop_on_exit: bool,
    stats_file: Option<PathBuf>,
    popout_cycles: u32,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        dirs: Vec::new(),
        stop_on_exit: false,
        stats_file: None,
        popout_cycles: 0,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--stop-on-exit" => a.stop_on_exit = true,
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
    if a.dirs.is_empty() {
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
    let gpu = gfx::Gpu::new()?;
    eprintln!("origami-gui: GPU {}", gpu.describe());

    let runner = qemu::QemuEngine::locate_runner().map_err(|e| e.to_string())?;
    let engine = Arc::new(qemu::QemuEngine::new(runner));
    eprintln!("origami-gui: engine {}", engine.id());
    let mut machines = Vec::new();
    for dir in &args.dirs {
        let id = engine
            .start(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = dir
            .canonicalize()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| dir.display().to_string());
        machines.push((id, name));
    }

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg-wgpu".into())
        .require_wgpu_30(slint::wgpu_30::WGPUConfiguration::Manual {
            instance: gpu.instance.clone(),
            adapter: gpu.adapter.clone(),
            device: gpu.device.clone(),
            queue: gpu.queue.clone(),
        })
        .select()
        .map_err(|e| format!("cannot start the window system: {e}"))?;

    let counters = ui::install(engine.clone(), gpu.clone(), machines.clone());

    let write_stats = {
        let counters = counters.clone();
        let gpu = gpu.clone();
        let engine = engine.clone();
        let ids: Vec<_> = machines.iter().map(|(id, _)| *id).collect();
        move |path: &std::path::Path| {
            let c = &counters;
            let g = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
            let per: Vec<String> = ids
                .iter()
                .filter_map(|id| {
                    engine.stats(*id).map(|s| {
                        format!(
                            "{{\"frames\":{},\"rows\":{},\"torn\":{},\"running\":{}}}",
                            s.frames,
                            s.rows,
                            s.torn,
                            engine.is_running(*id)
                        )
                    })
                })
                .collect();
            let text = format!(
                "{{\"wakes\":{},\"frames_taken\":{},\"rows_uploaded\":{},\"bytes_uploaded\":{},\"before_renders\":{},\"passes\":{},\"popouts\":{},\"docks\":{},\"keys_sent\":{},\"moves_sent\":{},\"input_errors\":{},\"gpu_errors\":{},\"engine\":[{}]}}\n",
                g(&c.wakes), g(&c.frames_taken), g(&c.rows_uploaded), g(&c.bytes_uploaded), g(&c.before_renders),
                g(&c.passes), g(&c.popouts), g(&c.docks), g(&c.keys_sent), g(&c.moves_sent), g(&c.input_errors),
                gpu.errors.count(), per.join(",")
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
    if let Some(path) = &args.stats_file {
        write_stats(path);
    }
    if args.stop_on_exit {
        for (id, name) in &machines {
            eprintln!("origami-gui: stopping {name}");
            if let Err(e) = engine.stop(*id) {
                eprintln!("origami-gui: stopping {name}: {e}");
            }
        }
    }
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
                None => {
                    if done < cycles && since.elapsed() > Duration::from_millis(1500) {
                        u.popout(0);
                        next = Some((Some((u.renders(0), true)), done));
                    }
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
