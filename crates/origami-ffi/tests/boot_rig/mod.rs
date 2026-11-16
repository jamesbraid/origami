//! What the booted-machine tests share: an X11 window the frame plane draws
//! into, the wake pipe, presenting, and comparing the window with a
//! screendump taken another way.
#![allow(dead_code, clippy::unnecessary_cast)]

use origami_ffi::*;
use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[link(name = "X11")]
extern "C" {
    pub fn XOpenDisplay(name: *const c_char) -> *mut c_void;
    pub fn XDefaultScreen(d: *mut c_void) -> c_int;
    pub fn XRootWindow(d: *mut c_void, screen: c_int) -> c_ulong;
    pub fn XCreateSimpleWindow(
        d: *mut c_void,
        parent: c_ulong,
        x: c_int,
        y: c_int,
        w: c_uint,
        h: c_uint,
        border_width: c_uint,
        border: c_ulong,
        background: c_ulong,
    ) -> c_ulong;
    pub fn XMapWindow(d: *mut c_void, w: c_ulong) -> c_int;
    pub fn XSync(d: *mut c_void, discard: c_int) -> c_int;
}

pub fn var(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} is not set; see the header of the test"))
}

pub fn ok(status: origami_status, what: &str) {
    if status != ORIGAMI_OK {
        let mut buf = [0 as c_char; 512];
        // SAFETY: a writable buffer of its stated length.
        unsafe { origami_last_error(buf.as_mut_ptr(), buf.len()) };
        let text = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
        panic!("{what}: status {status}: {}", text.to_string_lossy());
    }
}

/// CPU seconds this thread has used.
pub fn thread_cpu() -> f64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 * 1e-9
}

/// CPU ticks each thread has used, by name.
pub fn thread_ticks() -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    for t in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let Ok(stat) = std::fs::read_to_string(t.path().join("stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let f: Vec<&str> = stat[close + 2..].split(' ').collect();
        let ticks: u64 = f[11].parse::<u64>().unwrap_or(0) + f[12].parse::<u64>().unwrap_or(0);
        let name = format!(
            "{} {}",
            &stat[open + 1..close],
            t.file_name().to_string_lossy()
        );
        out.insert(name, ticks);
    }
    out
}

/// CPU seconds the whole process has used.
pub fn process_cpu() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: a valid rusage.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 * 1e-6;
    tv(u.ru_utime) + tv(u.ru_stime)
}

pub fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v.get(((v.len() as f64 - 1.0) * p).round() as usize)
        .copied()
        .unwrap_or(0.0)
}

/// A picture: width, height and RGB bytes.
pub type Shot = (u32, u32, Vec<u8>);

pub fn ppm(bytes: &[u8]) -> Shot {
    let mut at = 0;
    let mut field = || {
        while bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        String::from_utf8_lossy(&bytes[start..at]).into_owned()
    };
    assert_eq!(field(), "P6");
    let (w, h): (u32, u32) = (field().parse().unwrap(), field().parse().unwrap());
    assert_eq!(field(), "255");
    at += 1;
    (w, h, bytes[at..].to_vec())
}

/// What the X server shows in the window.
pub fn shot(window: c_ulong, to: &Path) -> Shot {
    let st = Command::new("import")
        .args(["-window", &window.to_string(), "-depth", "8"])
        .arg(format!("ppm:{}", to.display()))
        .status()
        .expect("ImageMagick import");
    assert!(st.success(), "import failed");
    let mut bytes = Vec::new();
    std::fs::File::open(to)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    ppm(&bytes)
}

pub fn differing(a: &[u8], b: &[u8]) -> Vec<usize> {
    a.chunks(3)
        .zip(b.chunks(3))
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(i, _)| i)
        .collect()
}

pub static WAKES: AtomicU64 = AtomicU64::new(0);

pub unsafe extern "C" fn wake(_: *mut c_void) {
    WAKES.fetch_add(1, Ordering::Relaxed);
}

pub struct Rig {
    pub machine: *const origami_machine,
    pub view: *mut origami_view,
    pub window: c_ulong,
    pub fd: i32,
    pub presents: u64,
    pub wall_ms: Vec<f64>,
    pub cpu_ms: Vec<f64>,
    /// Per present: our own part (take, upload, draw) in microseconds.
    pub ours_us: Vec<f64>,
    pub ours_last_ns: u64,
    pub last_present: Instant,
}

impl Rig {
    /// Wait up to `ms` for a wake; true when one came.
    pub fn wait(&self, ms: i32) -> bool {
        let mut p = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        unsafe { libc::poll(&mut p, 1, ms) > 0 }
    }

    pub fn present(&mut self) -> u32 {
        // SAFETY: valid handles.
        unsafe { ok(origami_frame_wake_clear(self.machine), "wake_clear") };
        let (w0, c0) = (Instant::now(), thread_cpu());
        let mut drawn = 9;
        // SAFETY: valid view and out pointer.
        unsafe { ok(origami_view_present(self.view, &mut drawn), "present") };
        if drawn == ORIGAMI_DRAWN_PRESENTED {
            self.presents += 1;
            self.wall_ms.push(w0.elapsed().as_secs_f64() * 1e3);
            self.cpu_ms.push((thread_cpu() - c0) * 1e3);
            let vs = self.view_stats();
            let ours = vs.take_ns + vs.draw_ns;
            self.ours_us.push((ours - self.ours_last_ns) as f64 / 1e3);
            self.ours_last_ns = ours;
            self.last_present = Instant::now();
        }
        drawn
    }

    /// Present on every wake until nothing has been drawn for `quiet`.
    pub fn settle(&mut self, quiet: Duration, limit: Duration) {
        let start = Instant::now();
        self.last_present = Instant::now();
        loop {
            if self.wait(100) {
                self.present();
            }
            if self.presents > 0 && self.last_present.elapsed() > quiet {
                return;
            }
            assert!(start.elapsed() < limit, "the picture never settled");
        }
    }

    /// Draw twice more: NVIDIA's presentation to Xvfb shows the previous
    /// image until the next present.
    pub fn flush(&mut self) {
        self.present();
        self.present();
        std::thread::sleep(Duration::from_millis(300));
    }

    /// The window against QEMU's screendump, taken while the guest is quiet:
    /// the pixels that differ and the colours in the picture. A guest that
    /// moves between the two shots (a wake) is measured again.
    pub fn compare(
        &mut self,
        dump: &dyn Fn(&Path) -> Shot,
        work: &Path,
        name: &str,
    ) -> (usize, usize) {
        for _ in 0..6 {
            self.settle(Duration::from_secs(3), Duration::from_secs(120));
            self.flush();
            let wakes = WAKES.load(Ordering::Relaxed);
            let (_, _, got) = shot(self.window, &work.join(format!("{name}-window.ppm")));
            let (_, _, want) = dump(&work.join(format!("{name}.ppm")));
            if WAKES.load(Ordering::Relaxed) == wakes {
                let colours: std::collections::HashSet<&[u8]> = want.chunks(3).collect();
                return (differing(&got, &want).len(), colours.len());
            }
        }
        panic!("the guest never stayed still for a comparison");
    }

    pub fn view_stats(&self) -> origami_view_counters {
        let mut vs = origami_view_counters {
            size: std::mem::size_of::<origami_view_counters>() as u32,
            reserved: 0,
            presents: 0,
            take_ns: 0,
            draw_ns: 0,
            present_ns: 0,
        };
        // SAFETY: a valid view and out pointer.
        unsafe { ok(origami_view_stats(self.view, &mut vs), "view_stats") };
        vs
    }

    pub fn seq(&self) -> u64 {
        let mut heads = [origami_head_info {
            width: 0,
            height: 0,
            seq: 0,
            active: 0,
            reserved: 0,
        }; 1];
        let mut n = 0;
        // SAFETY: valid buffer for one head.
        unsafe {
            ok(
                origami_frame_heads(self.machine, heads.as_mut_ptr(), 1, &mut n),
                "heads",
            )
        };
        heads[0].seq
    }

    pub fn stats(&self) -> origami_frame_counters {
        let mut s = origami_frame_counters {
            size: std::mem::size_of::<origami_frame_counters>() as u32,
            counted: 0,
            frames: 0,
            lines: 0,
            imported_lines: 0,
            copied_bytes: 0,
        };
        // SAFETY: valid machine and out pointer.
        unsafe { ok(origami_frame_stats(self.machine, &mut s), "stats") };
        s
    }

    pub fn cursor(&self) -> origami_cursor {
        let mut c = origami_cursor {
            size: std::mem::size_of::<origami_cursor>() as u32,
            present: 0,
            on: 0,
            x: 0,
            y: 0,
            hot_x: 0,
            hot_y: 0,
            width: 0,
            height: 0,
            gen: 0,
            image_bytes: 0,
        };
        let mut image = vec![0u8; 64 * 64 * 4];
        // SAFETY: valid machine, out pointer and buffer.
        unsafe {
            ok(
                origami_frame_cursor(
                    self.machine,
                    0,
                    u32::MAX,
                    &mut c,
                    image.as_mut_ptr(),
                    image.len(),
                ),
                "cursor",
            )
        };
        c
    }

    pub fn send(&self, events: &[origami_input_event]) {
        // SAFETY: valid events for their count.
        unsafe {
            ok(
                origami_input_send(self.machine, events.as_ptr(), events.len()),
                "input",
            )
        };
    }
}

pub fn key(code: u32, down: bool) -> origami_input_event {
    origami_input_event {
        kind: ORIGAMI_INPUT_KEY,
        code,
        down: u32::from(down),
        head: 0,
        x: 0,
        y: 0,
    }
}

pub fn move_to(x: i32, y: i32) -> origami_input_event {
    origami_input_event {
        kind: ORIGAMI_INPUT_POINTER_AT,
        code: 0,
        down: 0,
        head: 0,
        x,
        y,
    }
}

pub fn move_by(dx: i32, dy: i32) -> origami_input_event {
    origami_input_event {
        kind: ORIGAMI_INPUT_POINTER_MOVE,
        code: 0,
        down: 0,
        head: 0,
        x: dx,
        y: dy,
    }
}

pub fn keys_space() -> u32 {
    origami_key::Space as u32
}
