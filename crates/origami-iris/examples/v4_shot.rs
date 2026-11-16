//! A bare display protocol 4 viewer: `v4_shot SOCKET OUT.ppm [SECONDS]` writes the
//! last frame of head 0 as a PPM. `V4_SHOT_TYPE` types letters; `V4_SHOT_DUMP=DIR` compares with the machine's screendump.

#[cfg(target_os = "linux")]
use origami_display::v4::session::{Event, HelloParams, Session};
#[cfg(target_os = "linux")]
use origami_display::v4::wire;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (sock, out) = (&args[0], &args[1]);
    let secs: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let stream = UnixStream::connect(sock).expect("connect");
    let mut s = Session::connect(stream, &HelloParams::default()).expect("hello");
    s.request(0, false).expect("request");
    let end = Instant::now() + Duration::from_secs_f64(secs);
    let (mut frames, mut last) = (0u32, None);
    for c in std::env::var("V4_SHOT_TYPE").unwrap_or_default().bytes() {
        let code = match "qwertyuiop asdfghjkl  zxcvbnm".find(c as char) {
            Some(i) if i < 10 => 16 + i as u32,
            Some(i) if i < 20 => 30 + (i - 11) as u32,
            Some(i) => 44 + (i - 21) as u32,
            None => continue,
        };
        for down in [1, 0] {
            s.send(&wire::key(0, code, down)).expect("key");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    while Instant::now() < end {
        if let Some(Event::Frame(f)) = s.recv(Some(Duration::from_millis(200))).expect("recv") {
            frames += 1;
            s.request(0, false).expect("request");
            let (w, h, st) = (f.width as usize, f.height as usize, f.stride as usize);
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            for y in 0..h {
                for px in f.pixels()[y * st..y * st + w * 4].chunks_exact(4) {
                    ppm.extend_from_slice(&px[..3]);
                }
            }
            last = Some((ppm, f.frame.serial, f.frame.damage.clone()));
        }
    }
    println!("frames {frames} in {secs} s");
    if let Some((ppm, serial, damage)) = last {
        std::fs::write(out, &ppm).expect("write");
        println!("last serial {serial} damage {damage:?}");
        if let Some(dir) = std::env::var_os("V4_SHOT_DUMP") {
            let record = origami_machine::control::read(std::path::Path::new(&dir)).expect("run");
            let mut c =
                origami_protocol::Client::connect(record.control_port, Duration::from_secs(2))
                    .expect("control");
            let own = format!("{out}.dump.ppm");
            c.call_data_timeout(
                &origami_protocol::Request::Screenshot {
                    head: 0,
                    path: own.clone(),
                },
                Duration::from_secs(40),
            )
            .expect("screenshot");
            let theirs = std::fs::read(&own).expect("dump");
            let differ = ppm.iter().zip(&theirs).filter(|(a, b)| a != b).count();
            println!(
                "screendump {} bytes, v4 {} bytes, {} bytes differ",
                theirs.len(),
                ppm.len(),
                differ
            );
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
