//! Follow mode against a real guest. Ignored by default: it needs a running
//! QEMU machine at an IRIX desktop, with `-device display-shm,path=SOCKET`,
//! named by ORIGAMI_FOLLOW_SOCKET. It moves the pointer around a circle and
//! compares the guest's cursor with it.

use origami_display::v4::sys::UnixStream;
use origami_display::v4::viewer::Viewer;
use origami_engine::InputEvent;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
#[ignore = "needs a running IRIX desktop; see the header"]
fn the_guest_cursor_follows_a_circling_pointer() {
    let socket = std::env::var("ORIGAMI_FOLLOW_SOCKET").expect("ORIGAMI_FOLLOW_SOCKET");
    let viewer = Arc::new(Viewer::new());
    let v = viewer.clone();
    let stream = UnixStream::connect(&socket).expect("connect");
    std::thread::spawn(move || v.run(stream, &|| {}));
    let t0 = Instant::now();
    while viewer.followed_cursor().is_none() {
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "the guest has drawn no cursor"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let at = |t: f64| {
        let a = std::f64::consts::PI * t;
        (
            (640.0 + 200.0 * a.cos()).round() as i32,
            (512.0 + 200.0 * a.sin()).round() as i32,
        )
    };
    let send = |p: (i32, i32)| {
        viewer
            .send_input(&[InputEvent::PointerAt {
                head: 0,
                x: p.0,
                y: p.1,
            }])
            .unwrap();
    };
    send(at(0.0));
    std::thread::sleep(Duration::from_secs(1));

    // 4 s at 120 Hz, the guest's cursor read every millisecond.
    let cpu0 = follow_cpu_ms();
    let start = Instant::now();
    let (mut errors, mut draws, mut seq, mut aim) = (vec![], 0u32, None, at(0.0));
    let mut next = start;
    while start.elapsed() < Duration::from_secs(4) {
        let t = start.elapsed().as_secs_f64();
        if Instant::now() >= next {
            aim = at(t);
            send(aim);
            next += Duration::from_micros(8333);
        }
        let (s, x, y) = viewer.followed_cursor().unwrap();
        if seq != Some(s) {
            draws += 1;
            seq = Some(s);
        }
        if t > 0.3 {
            errors.push(((x - aim.0) as f64).hypot((y - aim.1) as f64));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(300));
    let cpu = follow_cpu_ms() - cpu0;
    let end = viewer.followed_cursor().unwrap();
    viewer.close();

    errors.sort_by(f64::total_cmp);
    let mean = errors.iter().sum::<f64>() / errors.len() as f64;
    let p95 = errors[errors.len() * 95 / 100];
    eprintln!(
        "MEASURE follow: {} cursor draws in 4 s, error mean {mean:.1} px, p95 {p95:.1}, max {:.1}; stopped at {},{} for {},{}",
        draws, errors[errors.len() - 1], end.1, end.2, aim.0, aim.1
    );
    eprintln!("MEASURE follow thread CPU while moving: {cpu:.0} ms in 4.3 s");
    // The guest acts on a packet about 10 ms after it is sent, so the cursor
    // can be drawn about 100 times a second and trails by about that long.
    assert!(draws >= 300, "{draws} draws in 4 s");
    assert!(mean < 12.0 && p95 < 20.0, "mean {mean:.1} p95 {p95:.1}");
    assert_eq!((end.1, end.2), aim);
}

/// The follow thread's user and system time so far, from /proc (Linux only).
fn follow_cpu_ms() -> f64 {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return 0.0;
    };
    let tick = 1000.0 / 100.0; // USER_HZ
    for t in tasks.flatten() {
        let comm = std::fs::read_to_string(t.path().join("comm")).unwrap_or_default();
        if comm.trim() == "origami-follow" {
            let stat = std::fs::read_to_string(t.path().join("stat")).unwrap_or_default();
            let f: Vec<&str> = stat
                .rsplit(')')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .collect();
            let ticks: f64 =
                f[11].parse::<f64>().unwrap_or(0.0) + f[12].parse::<f64>().unwrap_or(0.0);
            return ticks * tick;
        }
    }
    0.0
}
