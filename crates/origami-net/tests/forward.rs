//! Bulk frames between QEMU's two stream netdevs cross the switch intact
//! and in order, over loopback TCP and over Unix sockets, with the stack
//! left out of it. `cargo test -p origami-net --release -- --ignored
//! --nocapture` prints how fast.

use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use origami_net::{Endpoint, StackConfig, Switch, SwitchConfig};

const STACK_MAC: [u8; 6] = [0x08, 0x00, 0x69, 0x00, 0x00, 0x04];
const STACK_IP: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 4);

trait Stream: Read + Write + Send + 'static {
    fn clone_stream(&self) -> Box<dyn Stream>;
}
impl Stream for std::net::TcpStream {
    fn clone_stream(&self) -> Box<dyn Stream> {
        Box::new(self.try_clone().unwrap())
    }
}
#[cfg(unix)]
impl Stream for std::os::unix::net::UnixStream {
    fn clone_stream(&self) -> Box<dyn Stream> {
        Box::new(self.try_clone().unwrap())
    }
}

fn connect(e: &Endpoint) -> Box<dyn Stream> {
    match e {
        Endpoint::Tcp(a) => Box::new(std::net::TcpStream::connect(a).unwrap()),
        #[cfg(unix)]
        Endpoint::Unix(p) => Box::new(std::os::unix::net::UnixStream::connect(p).unwrap()),
        #[cfg(not(unix))]
        Endpoint::Unix(_) => unreachable!(),
    }
}

/// A frame for a host that is neither the stack's MAC nor its IP. The
/// first four payload bytes number it.
fn frame(n: u32, len: usize) -> Vec<u8> {
    let mut f = vec![n as u8; len];
    f[0..6].copy_from_slice(&[0x52, 0x55, 0xc0, 0, 2, 2]);
    f[12] = 8;
    f[13] = 0;
    f[30..34].copy_from_slice(&[192, 0, 2, 2]);
    f[34..38].copy_from_slice(&n.to_be_bytes());
    f
}

fn size(n: u32) -> usize {
    60 + (n as usize * 37) % 1440
}

fn send_all(s: &mut Box<dyn Stream>, count: u32) {
    let mut batch = Vec::new();
    for n in 0..count {
        let f = frame(n, size(n));
        batch.extend_from_slice(&(f.len() as u32).to_be_bytes());
        batch.extend_from_slice(&f);
        if batch.len() > 60_000 || n + 1 == count {
            s.write_all(&batch).unwrap();
            batch.clear();
        }
    }
}

/// Reads one frame, which carries `PROBE` if it is a readiness probe.
fn read_frame(s: &mut Box<dyn Stream>) -> Vec<u8> {
    let mut head = [0u8; 4];
    s.read_exact(&mut head).unwrap();
    let mut f = vec![0u8; u32::from_be_bytes(head) as usize];
    s.read_exact(&mut f).unwrap();
    f
}

/// Reads `count` numbered frames, skipping probes, and returns their bytes.
fn receive_all(s: &mut Box<dyn Stream>, count: u32) -> usize {
    let (mut n, mut bytes) = (0, 0);
    while n < count {
        let f = read_frame(s);
        if f == frame(PROBE, 60) {
            continue;
        }
        assert_eq!(f, frame(n, size(n)), "frame {n}");
        bytes += f.len();
        n += 1;
    }
    bytes
}

const PROBE: u32 = u32::MAX;

/// Sends `probe` from `from` until `to` has read one, the sign that the
/// switch has both connections: it registers each on its own thread and
/// drops a frame sent before then, as a dead link does.
fn wait_ready(from: &mut Box<dyn Stream>, to: &mut Box<dyn Stream>) {
    let mut sender = from.clone_stream();
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let t = thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            sender
                .write_all(&[&60u32.to_be_bytes()[..], &frame(PROBE, 60)].concat())
                .unwrap();
            thread::sleep(Duration::from_millis(20));
        }
    });
    assert_eq!(read_frame(to), frame(PROBE, 60));
    done.store(true, Ordering::Relaxed);
    t.join().unwrap();
}

/// Moves `count` frames each way and returns MB/s.
fn run(socket_dir: Option<PathBuf>, count: u32) -> f64 {
    let mut cfg = SwitchConfig::new(StackConfig {
        mac: STACK_MAC,
        ip: STACK_IP,
        prefix: 24,
        addressing: None,
    });
    cfg.socket_dir = socket_dir;
    let switch = Switch::start(cfg, None).unwrap();
    let (mut a, mut b) = (
        connect(switch.guest_endpoint()),
        connect(switch.network_endpoint()),
    );
    let (mut a_rx, mut b_rx) = (a.clone_stream(), b.clone_stream());
    wait_ready(&mut a, &mut b_rx);
    wait_ready(&mut b, &mut a_rx);
    let started = Instant::now();
    let send_a = thread::spawn(move || send_all(&mut a, count));
    let to_network = receive_all(&mut b_rx, count);
    send_a.join().unwrap();
    let send_b = thread::spawn(move || send_all(&mut b, count));
    let to_guest = receive_all(&mut a_rx, count);
    send_b.join().unwrap();
    (to_network + to_guest) as f64 / started.elapsed().as_secs_f64() / 1e6
}

#[test]
fn bulk_frames_cross_over_tcp() {
    run(None, 20_000);
}

#[cfg(unix)]
#[test]
fn bulk_frames_cross_over_unix_sockets() {
    let dir = origami_net::socket_dir().expect("a socket directory");
    run(Some(dir.clone()), 20_000);
    assert!(!dir.exists(), "the switch removes its socket directory");
}

#[test]
#[ignore = "a benchmark"]
fn throughput() {
    println!("tcp  {:.0} MB/s", run(None, 400_000));
    #[cfg(unix)]
    {
        let dir = origami_net::socket_dir().unwrap();
        println!("unix {:.0} MB/s", run(Some(dir), 400_000));
    }
}
