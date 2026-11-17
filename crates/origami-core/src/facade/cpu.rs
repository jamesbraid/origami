//! The latest per-CPU load of a running machine, sampled off the caller's
//! thread. It is host CPU time of the emulated CPUs' threads, not the guest's
//! own user, system or idle time, which would need a guest agent.

use origami_engine::{load_between, CpuLoad, CpuReading, Session};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// How often the sampler reads the threads.
pub(crate) const INTERVAL: Duration = Duration::from_millis(500);
/// The sampler stops when nobody has asked for this long.
const IDLE: Duration = Duration::from_secs(5);
/// Retry delay after a failed thread query.
const RETRY: Duration = Duration::from_secs(2);

/// What a load reading is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuLoadSource {
    /// Host CPU time of the threads that emulate the CPUs.
    Host,
}

/// The latest sample. `cpus` is empty until two readings exist, or when the
/// load cannot be measured, in which case `unavailable` says why.
#[derive(Clone, Debug, PartialEq)]
pub struct CpuLoadReport {
    pub source: CpuLoadSource,
    pub cpus: Vec<CpuLoad>,
    pub unavailable: Option<String>,
}

impl CpuLoadReport {
    pub(crate) fn none(why: &str) -> Self {
        Self {
            source: CpuLoadSource::Host,
            cpus: vec![],
            unavailable: Some(why.into()),
        }
    }
}

pub(crate) struct CpuMonitor {
    session: Weak<dyn Session>,
    latest: Arc<Mutex<CpuLoadReport>>,
    stop: Arc<AtomicBool>,
    asked: Arc<AtomicU64>,
    start: Instant,
    thread: std::thread::Thread,
}

impl CpuMonitor {
    pub(crate) fn start(session: &Arc<dyn Session>, interval: Duration) -> Option<Self> {
        let latest = Arc::new(Mutex::new(CpuLoadReport {
            source: CpuLoadSource::Host,
            cpus: vec![],
            unavailable: None,
        }));
        let (stop, asked, start) = (
            Arc::new(AtomicBool::new(false)),
            Arc::default(),
            Instant::now(),
        );
        let weak = Arc::downgrade(session);
        let worker = (
            weak.clone(),
            latest.clone(),
            stop.clone(),
            Arc::clone(&asked),
        );
        let handle = std::thread::Builder::new()
            .name("origami-cpu-load".into())
            .spawn(move || sample(worker, interval, start))
            .ok()?;
        Some(Self {
            session: weak,
            latest,
            stop,
            asked,
            start,
            thread: handle.thread().clone(),
        })
    }

    /// Whether this monitor samples `session` and is still running.
    pub(crate) fn serves(&self, session: &Arc<dyn Session>) -> bool {
        !self.stop.load(Ordering::Acquire) && Weak::ptr_eq(&self.session, &Arc::downgrade(session))
    }

    pub(crate) fn latest(&self) -> CpuLoadReport {
        self.asked
            .store(self.start.elapsed().as_millis() as u64, Ordering::Release);
        self.latest.lock().unwrap().clone()
    }
}

impl Drop for CpuMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

type Worker = (
    Weak<dyn Session>,
    Arc<Mutex<CpuLoadReport>>,
    Arc<AtomicBool>,
    Arc<AtomicU64>,
);

fn sample((session, latest, stop, asked): Worker, interval: Duration, start: Instant) {
    let mut prev: Option<(Instant, Vec<origami_engine::CpuTimes>)> = None;
    let mut cpus = 0;
    while !stop.load(Ordering::Acquire) {
        let idle = start
            .elapsed()
            .saturating_sub(Duration::from_millis(asked.load(Ordering::Acquire)));
        let Some(session) = session.upgrade().filter(|_| idle < IDLE) else {
            break;
        };
        let mut wait = interval;
        match session.cpu_times() {
            Ok(CpuReading::Times(mut now)) => {
                // iris has no thread for a paused CPU: keep its bar.
                cpus = cpus.max(now.len());
                now.resize(cpus, Default::default());
                let at = Instant::now();
                if let Some((then, before)) = &prev {
                    *latest.lock().unwrap() = CpuLoadReport {
                        source: CpuLoadSource::Host,
                        cpus: load_between(before, &now, at - *then),
                        unavailable: None,
                    };
                }
                prev = Some((at, now));
            }
            Ok(CpuReading::Unavailable(why)) => {
                // Final for this session; the report stays until it is replaced.
                *latest.lock().unwrap() = CpuLoadReport::none(&why);
                return;
            }
            Err(error) => {
                prev = None;
                *latest.lock().unwrap() =
                    CpuLoadReport::none(&format!("CPU load is not readable yet: {error}"));
                wait = RETRY;
            }
        }
        drop(session);
        std::thread::park_timeout(wait);
    }
    // Let the next ask start a fresh sampler.
    stop.store(true, Ordering::Release);
}
