//! What a viewer pays per frame (receive FRAME, map the slot, release), with
//! an in-process producer: `cargo bench -p display-conformance`.

use display_conformance::client::{Opts, Target, V};
use origami_display::v4::testext::pattern;
use origami_display::v4::wire::{Damage, Frame};
use origami_display_producer::reference;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

/// A cheap timestamp: `Instant::now()` costs over a microsecond on some
/// virtual machines, which would swamp the steps measured here.
#[cfg(target_arch = "x86_64")]
fn ticks() -> u64 {
    // SAFETY: rdtsc has no preconditions.
    unsafe { core::arch::x86_64::_rdtsc() }
}
#[cfg(not(target_arch = "x86_64"))]
fn ticks() -> u64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// Ticks per nanosecond, measured against the monotonic clock.
fn tick_rate() -> f64 {
    let (t0, c0) = (Instant::now(), ticks());
    std::thread::sleep(Duration::from_millis(100));
    (ticks() - c0) as f64 / t0.elapsed().as_nanos() as f64
}

fn thread_cpu_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn pct(v: &mut [u64], p: f64, rate: f64) -> f64 {
    v.sort_unstable();
    v[((v.len() - 1) as f64 * p) as usize] as f64 / rate
}

fn run(name: &str, pat: u32, w: u32, h: u32, n: u32, rate: f64) {
    let dir = std::env::temp_dir().join(format!("vf-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("d.sock");
    let p = reference::start(UnixListener::bind(&path).unwrap(), 1, w, h, false).unwrap();
    let t = Target { path };
    let mut v = V::connect(&t, &Opts::default()).unwrap();
    let (mut recv, mut map, mut rel) = (vec![], vec![], vec![]);
    let mut sink = 0u64;
    let cpu0 = thread_cpu_ns();
    let wall0 = Instant::now();
    for i in 0..n {
        v.pat(0, pat, 1, i + 1).unwrap();
        let t0 = ticks();
        v.s.request(0, false).unwrap();
        let f = v.frame(Duration::from_secs(5)).unwrap().expect("frame");
        let t1 = ticks();
        // "Map": reach the slot's pixels the way an upload would, first and last damaged line.
        let px = f.pixels();
        sink += px[0] as u64 + px[px.len() - 4] as u64 + f.frame.serial;
        let t2 = ticks();
        f.release();
        let t3 = ticks();
        recv.push(t1 - t0);
        map.push(t2 - t1);
        rel.push(t3 - t2);
    }
    let cpu = (thread_cpu_ns() - cpu0) as f64 / n as f64 / 1000.0;
    let wall = wall0.elapsed().as_secs_f64() * 1e6 / n as f64;
    println!(
        "{name:<22} {w}x{h}  request->FRAME us p50 {:7.1} p99 {:7.1} | map us p50 {:5.3} | release us p50 {:5.3} p99 {:6.3} | viewer thread CPU {cpu:6.1} us/frame, wall {wall:7.1} us/frame",
        pct(&mut recv, 0.5, rate * 1000.0),
        pct(&mut recv, 0.99, rate * 1000.0),
        pct(&mut map, 0.5, rate * 1000.0),
        pct(&mut rel, 0.5, rate * 1000.0),
        pct(&mut rel, 0.99, rate * 1000.0),
    );
    std::hint::black_box(sink);
    drop(v);
    p.stop();
    let _ = std::fs::remove_dir_all(dir);
}

/// The wire and lease work with no socket and no producer.
fn micro() {
    let f = Frame {
        head: 0,
        region: 3,
        serial: 9,
        mode_serial: 1,
        sample_ns: 1,
        publish_ns: 2,
        guest_ns: 0,
        lease: 0,
        frame_flags: 0,
        lease_seq: 5,
        damage: Damage::Lines(vec![0xFFFF; 16]),
    };
    let bytes = f.encode();
    const N: u32 = 2_000_000;
    let t = Instant::now();
    let mut acc = 0u64;
    for _ in 0..N {
        let d = Frame::decode(std::hint::black_box(&bytes)).unwrap();
        acc += d.serial;
    }
    let ns = t.elapsed().as_nanos() as f64 / N as f64;
    println!(
        "Frame::decode ({} bytes, 16 damage words)      {ns:6.1} ns",
        bytes.len()
    );
    std::hint::black_box(acc);
}

fn main() {
    micro();
    let rate = tick_rate();
    run(
        "sparse (1 in 7 lines)",
        pattern::LINES,
        1280,
        1024,
        3000,
        rate,
    );
    run("full motion", pattern::SOLID, 1280, 1024, 1500, rate);
    run("small head", pattern::LINES, 256, 128, 3000, rate);
}
