//! The About window's `hinv` egg: an IRIX-style inventory of this host,
//! read locally and built here so all three apps show the same text.

use std::path::Path;

/// What the host reports. `None` means the platform would not say.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Host {
    pub cpus: usize,
    pub model: Option<String>,
    pub mhz: Option<u32>,
    pub memory_bytes: Option<u64>,
    pub gpu: Option<String>,
    pub free_bytes: Option<u64>,
    /// (name, product) per library machine; `None` when the library would not list.
    pub machines: Option<Vec<(String, String)>>,
}

pub(crate) fn format(h: &Host) -> String {
    let plural = if h.cpus == 1 { "" } else { "s" };
    let mut out = format!("{} ", h.cpus.max(1));
    if let Some(mhz) = h.mhz {
        out += &format!("{mhz} MHZ ");
    }
    if let Some(model) = &h.model {
        out += &format!("{model} ");
    }
    out += &format!("Processor{plural}\n");
    if let Some(b) = h.memory_bytes {
        out += &format!("Main memory size: {} Mbytes\n", b >> 20);
    }
    if let Some(gpu) = &h.gpu {
        out += &format!("Graphics board: {gpu}\n");
    }
    if let Some(b) = h.free_bytes {
        out += &format!("Library volume: {} Gbytes free\n", b >> 30);
    }
    if let Some(machines) = &h.machines {
        if machines.is_empty() {
            out += "No emulated systems\n";
        }
        for (name, product) in machines {
            out += &format!("Emulated system: {name} ({product})\n");
        }
    }
    out
}

/// Reads the host. Touches the disk and the GPU: call off the UI thread.
pub(crate) fn gather(library: &Path, machines: Option<Vec<(String, String)>>) -> Host {
    let (model, mhz) = os::cpu();
    Host {
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get()),
        model: model.map(|m| collapse(&m)).filter(|m| !m.is_empty()),
        mhz,
        memory_bytes: os::memory(),
        gpu: gpu(),
        free_bytes: os::free(library),
        machines,
    }
}

/// Vendors pad model strings with spaces, and some already end in "Processor".
fn collapse(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.strip_suffix(" Processor")
        .map_or(s.clone(), str::to_string)
}

#[cfg(feature = "render")]
fn gpu() -> Option<String> {
    let gpu = crate::engines::gpu::shared().ok()?;
    Some(gpu.adapter.get_info().name)
}

#[cfg(not(feature = "render"))]
fn gpu() -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
mod os {
    use std::path::Path;

    pub fn cpu() -> (Option<String>, Option<u32>) {
        let info = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let field = |key: &str| {
            info.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        };
        let model = field("model name")
            .or_else(|| field("Model"))
            .or_else(|| field("Hardware"));
        let max_khz =
            std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .map(|k| k / 1000);
        let mhz = max_khz.or_else(|| field("cpu MHz")?.parse::<f64>().ok().map(|m| m as u32));
        (model, mhz)
    }

    pub fn memory() -> Option<u64> {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb = info.lines().find_map(|l| l.strip_prefix("MemTotal:"))?;
        Some(
            kb.trim()
                .trim_end_matches("kB")
                .trim()
                .parse::<u64>()
                .ok()?
                << 10,
        )
    }

    pub fn free(p: &Path) -> Option<u64> {
        super::statvfs_free(p)
    }
}

#[cfg(target_os = "macos")]
mod os {
    use std::ffi::CString;
    use std::path::Path;

    fn sysctl<T: Default + Copy>(name: &str) -> Option<T> {
        let name = CString::new(name).ok()?;
        let mut v = T::default();
        let mut len = std::mem::size_of::<T>();
        // SAFETY: `v` is `len` bytes of plain data and `name` is NUL-terminated.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut v as *mut T).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0).then_some(v)
    }

    fn brand() -> Option<String> {
        let name = CString::new("machdep.cpu.brand_string").ok()?;
        let mut buf = [0u8; 128];
        let mut len = buf.len();
        // SAFETY: `buf` is `len` bytes and `name` is NUL-terminated.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        Some(
            String::from_utf8_lossy(&buf[..len])
                .trim_end_matches('\0')
                .to_string(),
        )
    }

    // Apple Silicon has no hw.cpufrequency, so no MHz there.
    pub fn cpu() -> (Option<String>, Option<u32>) {
        let hz: Option<u64> = sysctl("hw.cpufrequency_max");
        (brand(), hz.map(|h| (h / 1_000_000) as u32))
    }

    pub fn memory() -> Option<u64> {
        sysctl::<u64>("hw.memsize")
    }

    pub fn free(p: &Path) -> Option<u64> {
        super::statvfs_free(p)
    }
}

#[cfg(unix)]
fn statvfs_free(p: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    // SAFETY: zeroed statvfs is a valid out-parameter and `path` is NUL-terminated.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(path.as_ptr(), &mut st) } == 0)
        .then(|| (st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(windows)]
mod os {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    const CPU0: &str = "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub fn cpu() -> (Option<String>, Option<u32>) {
        let key = wide(CPU0);
        let value = |name: &str, flags: u32, buf: *mut u8, len: &mut u32| {
            let name = wide(name);
            // SAFETY: `buf` has `*len` bytes; the strings are NUL-terminated.
            let rc = unsafe {
                RegGetValueW(
                    HKEY_LOCAL_MACHINE,
                    key.as_ptr(),
                    name.as_ptr(),
                    flags,
                    std::ptr::null_mut(),
                    buf.cast(),
                    len,
                )
            };
            rc == 0
        };
        let mut name = [0u16; 128];
        let mut len = (name.len() * 2) as u32;
        let model = value(
            "ProcessorNameString",
            RRF_RT_REG_SZ,
            name.as_mut_ptr().cast(),
            &mut len,
        )
        .then(|| String::from_utf16_lossy(&name[..(len as usize / 2).saturating_sub(1)]));
        let mut mhz = 0u32;
        let mut len = 4u32;
        let mhz = value(
            "~MHz",
            RRF_RT_REG_DWORD,
            (&mut mhz as *mut u32).cast(),
            &mut len,
        )
        .then_some(mhz);
        (model, mhz)
    }

    pub fn memory() -> Option<u64> {
        // SAFETY: dwLength is set as the API requires.
        let mut st: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        (unsafe { GlobalMemoryStatusEx(&mut st) } != 0).then_some(st.ullTotalPhys)
    }

    pub fn free(p: &Path) -> Option<u64> {
        let path: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        // SAFETY: `path` is NUL-terminated and `free` is a valid out-pointer.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(free)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Host {
        Host {
            cpus: 12,
            model: Some("Apple M3 Pro".into()),
            mhz: Some(3200),
            memory_bytes: Some(36864 << 20),
            gpu: Some("Apple M3 Pro".into()),
            free_bytes: Some(994 << 30),
            machines: Some(vec![
                ("Studio".into(), "Octane2".into()),
                ("Desk".into(), "O2".into()),
            ]),
        }
    }

    #[test]
    fn formats_a_full_inventory() {
        assert_eq!(
            format(&sample()),
            "12 3200 MHZ Apple M3 Pro Processors\n\
             Main memory size: 36864 Mbytes\n\
             Graphics board: Apple M3 Pro\n\
             Library volume: 994 Gbytes free\n\
             Emulated system: Studio (Octane2)\n\
             Emulated system: Desk (O2)\n"
        );
    }

    #[test]
    fn omits_what_the_host_would_not_say() {
        let h = Host {
            cpus: 1,
            ..Host::default()
        };
        assert_eq!(format(&h), "1 Processor\n");
    }

    #[test]
    fn collapses_vendor_padding() {
        assert_eq!(collapse("Intel(R)  Core(TM)   i7 "), "Intel(R) Core(TM) i7");
        assert_eq!(collapse("AMD EPYC 56-Core Processor"), "AMD EPYC 56-Core");
    }

    #[test]
    fn reads_this_host() {
        let dir = std::env::temp_dir();
        let h = gather(&dir, Some(vec![]));
        assert!(h.cpus >= 1);
        assert!(h.memory_bytes.is_some_and(|b| b > 0));
        assert!(h.free_bytes.is_some());
        assert!(format(&h).contains("Main memory size: "));
    }
}
