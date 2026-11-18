//! Host-side CPU time of the threads that run a machine's emulated CPUs.
//! This is what the host spent on each CPU, not the guest's own user, system
//! or idle time: an idle guest CPU that the emulator spins on shows as busy.

use std::time::Duration;

/// Cumulative host CPU time of one emulated CPU's thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub user: Duration,
    pub sys: Duration,
}

/// A host thread: the process it belongs to and its OS thread id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostThread {
    pub pid: u32,
    pub tid: u64,
}

/// What a session can say about its CPUs' host threads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CpuReading {
    /// The engine cannot measure this, for a reason fit for a status line.
    Unavailable(String),
    /// Cumulative times, one entry per emulated CPU in CPU order.
    Times(Vec<CpuTimes>),
}

/// One emulated CPU's share of one host core over an interval, in percent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuLoad {
    pub cpu: u32,
    pub user_pct: f32,
    pub sys_pct: f32,
}

/// The load of each CPU between two readings `wall` apart. A CPU whose
/// counters went backwards (its thread was replaced) or that is new reads 0.
pub fn load_between(prev: &[CpuTimes], now: &[CpuTimes], wall: Duration) -> Vec<CpuLoad> {
    let wall = wall.as_secs_f64().max(1e-9);
    let pct = |d: Duration| (d.as_secs_f64() / wall * 100.0).clamp(0.0, 100.0) as f32;
    now.iter()
        .enumerate()
        .map(|(cpu, now)| {
            let (user, sys) = match prev.get(cpu) {
                Some(prev) if now.user >= prev.user && now.sys >= prev.sys => {
                    (pct(now.user - prev.user), pct(now.sys - prev.sys))
                }
                _ => (0.0, 0.0),
            };
            // Counters are read at slightly different moments; keep the sum
            // a possible percentage.
            let sys = sys.min(100.0 - user);
            CpuLoad {
                cpu: cpu as u32,
                user_pct: user,
                sys_pct: sys,
            }
        })
        .collect()
}

/// The index in an iris CPU thread's name: `MIPS-CPU`, `MIPS-CPU1`, ...
pub fn iris_cpu_index(name: &str) -> Option<usize> {
    let rest = name.strip_prefix("MIPS-CPU")?;
    if rest.is_empty() {
        Some(0)
    } else {
        rest.parse().ok()
    }
}

/// Cumulative times of `thread`.
pub fn thread_times(thread: HostThread) -> std::io::Result<CpuTimes> {
    os::thread_times(thread)
}

/// Cumulative times of the threads of `pid` whose names `index` maps to a
/// CPU number, ordered by that number. Missing CPUs read as zero.
pub fn named_thread_times(
    pid: u32,
    index: fn(&str) -> Option<usize>,
) -> std::io::Result<Vec<CpuTimes>> {
    let mut out: Vec<CpuTimes> = Vec::new();
    for (name, times) in os::threads_of(pid)? {
        if let Some(cpu) = index(&name) {
            if out.len() <= cpu {
                out.resize(cpu + 1, CpuTimes::default());
            }
            out[cpu] = times;
        }
    }
    Ok(out)
}

/// Whether [`named_thread_times`] works on this host.
pub const CAN_NAME_THREADS: bool = cfg!(any(target_os = "linux", target_os = "macos"));

#[cfg(target_os = "linux")]
mod os {
    use super::{CpuTimes, HostThread};
    use std::io;
    use std::time::Duration;

    /// utime and stime (fields 14 and 15) of a `/proc/.../stat` line, in
    /// clock ticks of `hz` per second. The name, field 2, may hold spaces and
    /// parentheses, so the fields are counted after its last `)`.
    pub(super) fn parse_stat(line: &str, hz: u64) -> Option<CpuTimes> {
        let rest = &line[line.rfind(')')? + 1..];
        let mut fields = rest.split_whitespace().skip(11);
        let ticks = |s: &str| {
            s.parse::<u64>()
                .ok()
                .map(|t| Duration::from_micros(t * 1_000_000 / hz))
        };
        Some(CpuTimes {
            user: ticks(fields.next()?)?,
            sys: ticks(fields.next()?)?,
        })
    }

    fn hz() -> u64 {
        // SAFETY: sysconf takes no pointers.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if hz > 0 {
            hz as u64
        } else {
            100
        }
    }

    fn bad(what: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, what.to_string())
    }

    pub(super) fn thread_times(t: HostThread) -> io::Result<CpuTimes> {
        let path = format!("/proc/{}/task/{}/stat", t.pid, t.tid);
        parse_stat(&std::fs::read_to_string(path)?, hz()).ok_or_else(|| bad("unreadable stat"))
    }

    pub(super) fn threads_of(pid: u32) -> io::Result<Vec<(String, CpuTimes)>> {
        let hz = hz();
        // procfs can cut the task listing short when a thread exits mid-read;
        // a vanished entry is that sign, so list again.
        let mut out = Vec::new();
        for _ in 0..8 {
            out.clear();
            let mut vanished = false;
            for entry in std::fs::read_dir(format!("/proc/{pid}/task"))? {
                let dir = entry?.path();
                let (Ok(name), Ok(stat)) = (
                    std::fs::read_to_string(dir.join("comm")),
                    std::fs::read_to_string(dir.join("stat")),
                ) else {
                    vanished = true;
                    continue;
                };
                if let Some(times) = parse_stat(&stat, hz) {
                    out.push((name.trim_end().to_string(), times));
                }
            }
            if !vanished {
                break;
            }
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stat_fields_are_counted_after_the_last_paren() {
            let line = "42 (MIPS (CPU) 1) R 1 2 3 4 5 6 7 8 9 10 250 75 0 0 20 0 1 0 5 0 0";
            let t = parse_stat(line, 100).unwrap();
            assert_eq!(t.user, Duration::from_millis(2500));
            assert_eq!(t.sys, Duration::from_millis(750));
        }

        #[test]
        fn this_process_threads_are_listed_with_names_and_times() {
            let (ready, wait) = std::sync::mpsc::channel();
            let (done, hold) = std::sync::mpsc::channel::<()>();
            let busy = std::thread::Builder::new()
                .name("MIPS-CPU1".into())
                .spawn(move || {
                    ready.send(()).unwrap();
                    let _ = hold.recv();
                })
                .unwrap();
            wait.recv().unwrap();
            let times =
                super::super::named_thread_times(std::process::id(), super::super::iris_cpu_index)
                    .unwrap();
            drop(done);
            busy.join().unwrap();
            assert_eq!(times.len(), 2, "{times:?}");
            assert_eq!(times[0], CpuTimes::default());
        }

        #[test]
        fn a_busy_thread_accumulates_cpu_time() {
            let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u64;
            let thread = HostThread {
                pid: std::process::id(),
                tid,
            };
            let before = thread_times(thread).unwrap();
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_millis(400) {
                std::hint::black_box(0);
            }
            let after = thread_times(thread).unwrap();
            // Some hosts charge a spin's clock reads to system time.
            let spent = |t: CpuTimes| t.user + t.sys;
            assert!(
                spent(after) - spent(before) >= Duration::from_millis(200),
                "{before:?} {after:?}"
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::{CpuTimes, HostThread};
    use std::io;
    use std::time::Duration;

    // Not in the libc crate. From <sys/proc_info.h>. The list holds thread
    // handles, which PROC_PIDTHREADINFO takes (the 64-bit-id flavor does not).
    const PROC_PIDLISTTHREADS: libc::c_int = 6;

    /// `pth_user_time` and `pth_system_time` are mach absolute-time ticks.
    // libc deprecates mach_timebase_info for the mach2 crate; one call is not worth a dependency.
    #[allow(deprecated)]
    fn nanos(ticks: u64) -> Duration {
        let mut base = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: base is a valid out pointer.
        unsafe { libc::mach_timebase_info(&mut base) };
        let (n, d) = (u128::from(base.numer.max(1)), u128::from(base.denom.max(1)));
        Duration::from_nanos((u128::from(ticks) * n / d) as u64)
    }

    fn info(pid: u32, tid: u64) -> io::Result<libc::proc_threadinfo> {
        // SAFETY: an all-zero proc_threadinfo is valid; proc_pidinfo fills it.
        let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_threadinfo>() as libc::c_int;
        // SAFETY: the buffer is `size` bytes.
        let got = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTHREADINFO,
                tid,
                (&mut info as *mut libc::proc_threadinfo).cast(),
                size,
            )
        };
        if got == size {
            Ok(info)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn thread_times(t: HostThread) -> io::Result<CpuTimes> {
        let i = info(t.pid, t.tid)?;
        Ok(CpuTimes {
            user: nanos(i.pth_user_time),
            sys: nanos(i.pth_system_time),
        })
    }

    pub(super) fn threads_of(pid: u32) -> io::Result<Vec<(String, CpuTimes)>> {
        let mut ids = vec![0u64; 1024];
        // SAFETY: the buffer is `len * 8` bytes.
        let bytes = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                PROC_PIDLISTTHREADS,
                0,
                ids.as_mut_ptr().cast(),
                (ids.len() * 8) as libc::c_int,
            )
        };
        if bytes < 0 {
            return Err(io::Error::last_os_error());
        }
        ids.truncate(bytes as usize / 8);
        let mut out = Vec::new();
        for tid in ids {
            // A thread can exit between the listing and the read.
            let Ok(i) = info(pid, tid) else { continue };
            let name = i
                .pth_name
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c as u8 as char)
                .collect();
            out.push((
                name,
                CpuTimes {
                    user: nanos(i.pth_user_time),
                    sys: nanos(i.pth_system_time),
                },
            ));
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::super::{iris_cpu_index, named_thread_times};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        #[test]
        fn a_named_busy_thread_is_found_and_accumulates_cpu_time() {
            let (ready, wait) = std::sync::mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let spin = stop.clone();
            let busy = std::thread::Builder::new()
                .name("MIPS-CPU1".into())
                .spawn(move || {
                    ready.send(()).unwrap();
                    while !spin.load(Ordering::Relaxed) {
                        std::hint::black_box(0);
                    }
                })
                .unwrap();
            wait.recv().unwrap();
            // The thread runs until told to stop, so a slow runner cannot
            // end it before the sample; poll for the CPU time, with a bound.
            let end = Instant::now() + Duration::from_secs(10);
            let times = loop {
                let times = named_thread_times(std::process::id(), iris_cpu_index).unwrap();
                let busy_enough = times
                    .get(1)
                    .is_some_and(|t| t.user + t.sys >= Duration::from_millis(100));
                if busy_enough || Instant::now() > end {
                    break times;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            stop.store(true, Ordering::Relaxed);
            busy.join().unwrap();
            assert_eq!(times.len(), 2, "{times:?}");
            assert!(
                times[1].user + times[1].sys >= Duration::from_millis(100),
                "{times:?}"
            );
        }
    }
}

#[cfg(windows)]
mod os {
    use super::{CpuTimes, HostThread};
    use std::io;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetThreadTimes, OpenThread, THREAD_QUERY_LIMITED_INFORMATION,
    };

    /// A FILETIME span is in units of 100 ns.
    fn span(t: FILETIME) -> Duration {
        let ticks = (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        Duration::from_nanos(ticks * 100)
    }

    pub(super) fn thread_times(t: HostThread) -> io::Result<CpuTimes> {
        // SAFETY: plain handle calls; every pointer is a local the call fills.
        unsafe {
            let handle = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, t.tid as u32);
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut create, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
            let ok = GetThreadTimes(handle, &mut create, &mut exit, &mut kernel, &mut user);
            let error = io::Error::last_os_error();
            CloseHandle(handle);
            if ok == 0 {
                return Err(error);
            }
            Ok(CpuTimes {
                user: span(user),
                sys: span(kernel),
            })
        }
    }

    pub(super) fn threads_of(_pid: u32) -> io::Result<Vec<(String, CpuTimes)>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "thread names are not read on Windows",
        ))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod os {
    use super::{CpuTimes, HostThread};
    use std::io;

    pub(super) fn thread_times(_: HostThread) -> io::Result<CpuTimes> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "not measured on this host",
        ))
    }

    pub(super) fn threads_of(_: u32) -> io::Result<Vec<(String, CpuTimes)>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "not measured on this host",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(user_ms: u64, sys_ms: u64) -> CpuTimes {
        CpuTimes {
            user: Duration::from_millis(user_ms),
            sys: Duration::from_millis(sys_ms),
        }
    }

    #[test]
    fn load_is_the_time_delta_over_the_wall_time() {
        let prev = [t(1000, 100), t(0, 0)];
        let now = [t(1300, 150), t(500, 0)];
        let load = load_between(&prev, &now, Duration::from_millis(500));
        assert_eq!(
            load,
            vec![
                CpuLoad {
                    cpu: 0,
                    user_pct: 60.0,
                    sys_pct: 10.0
                },
                CpuLoad {
                    cpu: 1,
                    user_pct: 100.0,
                    sys_pct: 0.0
                },
            ]
        );
    }

    #[test]
    fn a_replaced_thread_or_new_cpu_reads_zero_and_the_sum_stays_a_percentage() {
        let load = load_between(&[t(900, 0)], &[t(100, 0), t(50, 0)], Duration::from_secs(1));
        assert_eq!((load[0].user_pct, load[1].user_pct), (0.0, 0.0));
        let load = load_between(&[t(0, 0)], &[t(500, 600)], Duration::from_secs(1));
        assert_eq!((load[0].user_pct, load[0].sys_pct), (50.0, 50.0));
    }

    #[test]
    fn iris_cpu_threads_are_named_by_index() {
        assert_eq!(iris_cpu_index("MIPS-CPU"), Some(0));
        assert_eq!(iris_cpu_index("MIPS-CPU3"), Some(3));
        assert_eq!(iris_cpu_index("MIPS-Debug"), None);
    }
}
