//! The host threads behind QEMU's emulated CPUs, for a load display.

use origami_engine::{
    named_thread_times, thread_times, CpuReading, HostThread, Result, CAN_NAME_THREADS,
};
use qapi::qmp;
use std::sync::Mutex;

enum Found {
    Threads(Vec<HostThread>),
    /// QMP's ids are unusable; the threads are found by their names.
    Named(usize),
    Unavailable(&'static str),
}

/// The thread ids QMP reported, fetched once per session.
#[derive(Default)]
pub(crate) struct VcpuThreads(Mutex<Option<Found>>);

impl VcpuThreads {
    /// Cumulative times per CPU. `qmp` runs only until a query succeeds.
    pub(crate) fn read(
        &self,
        pid: u32,
        qmp: impl FnOnce() -> Result<crate::control::Qmp>,
    ) -> Result<CpuReading> {
        let mut known = self.0.lock().unwrap();
        if known.is_none() {
            let infos = qmp()?.execute(&qmp::query_cpus_fast {})?;
            // The reply is a union on the target; the two fields are common.
            let mut cpus: Vec<(i64, u64)> = infos
                .iter()
                .filter_map(|cpu| {
                    let cpu = serde_json::to_value(cpu).ok()?;
                    Some((cpu["cpu-index"].as_i64()?, cpu["thread-id"].as_u64()?))
                })
                .collect();
            cpus.sort_unstable();
            *known = Some(match unusable(pid, &cpus) {
                Some(_) if CAN_NAME_THREADS => Found::Named(cpus.len()),
                Some(why) => Found::Unavailable(why),
                None => Found::Threads(
                    cpus.iter()
                        .map(|&(_, tid)| HostThread { pid, tid })
                        .collect(),
                ),
            });
        }
        match known.as_ref() {
            Some(Found::Threads(threads)) => Ok(CpuReading::Times(
                threads
                    .iter()
                    .map(|&t| thread_times(t))
                    .collect::<std::io::Result<_>>()?,
            )),
            Some(&Found::Named(count)) => {
                let times = named_thread_times(pid, vcpu_index)?;
                // One thread for all CPUs (single-threaded TCG) is not per-CPU load.
                Ok(match times.len() {
                    0 => CpuReading::Unavailable("QEMU's CPU threads are not named".into()),
                    1 if count > 1 => {
                        CpuReading::Unavailable("the emulated CPUs share one host thread".into())
                    }
                    _ => CpuReading::Times(times),
                })
            }
            Some(Found::Unavailable(why)) => Ok(CpuReading::Unavailable((*why).into())),
            None => unreachable!("filled above"),
        }
    }
}

/// The CPU number in a QEMU vCPU thread name, `CPU 3/TCG`; the one thread
/// of single-threaded TCG is CPU 0.
fn vcpu_index(name: &str) -> Option<usize> {
    if name == "ALL CPUs/TCG" {
        return Some(0);
    }
    name.strip_prefix("CPU ")?
        .strip_suffix("/TCG")?
        .parse()
        .ok()
}

/// Why QMP's `(cpu index, thread id)` pairs cannot be told apart per CPU.
fn unusable(pid: u32, cpus: &[(i64, u64)]) -> Option<&'static str> {
    // On macOS QEMU reports its process id for every thread.
    if cpus.iter().any(|&(_, tid)| tid == u64::from(pid)) {
        return Some("QEMU does not report per-CPU thread ids on this host");
    }
    let mut tids: Vec<u64> = cpus.iter().map(|&(_, tid)| tid).collect();
    tids.sort_unstable();
    tids.dedup();
    (tids.len() != cpus.len()).then_some("the emulated CPUs share one host thread")
}

#[cfg(test)]
mod tests {
    use super::{unusable, vcpu_index};

    #[test]
    fn thread_ids_must_be_distinct_and_not_the_process() {
        assert_eq!(unusable(10, &[(0, 11), (1, 12)]), None);
        assert!(unusable(10, &[(0, 10), (1, 10)]).is_some());
        assert!(unusable(10, &[(0, 11), (1, 11)]).is_some());
    }

    #[test]
    fn vcpu_threads_are_named_by_cpu_number() {
        assert_eq!(vcpu_index("CPU 3/TCG"), Some(3));
        assert_eq!(vcpu_index("ALL CPUs/TCG"), Some(0));
        assert_eq!(vcpu_index("call_rcu"), None);
    }
}
