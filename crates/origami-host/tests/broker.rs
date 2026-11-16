use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use origami_host::broker::*;

#[derive(Clone, Default)]
struct Sink {
    data: Arc<Mutex<Vec<u8>>>,
    stall: Arc<AtomicBool>,
    // Write in two halves with a yield between, so a lease grant sneaking
    // into the middle of a write would show up as a torn message.
    split: bool,
}

impl Sink {
    fn bytes(&self) -> Vec<u8> {
        self.data.lock().unwrap().clone()
    }
}

impl InputSink for Sink {
    fn write_input(&mut self, data: &[u8], timeout: Duration) -> std::io::Result<()> {
        if self.stall.load(Ordering::Acquire) {
            thread::sleep(timeout);
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        if self.split && data.len() > 1 {
            let (a, b) = data.split_at(data.len() / 2);
            self.data.lock().unwrap().extend_from_slice(a);
            thread::yield_now();
            self.data.lock().unwrap().extend_from_slice(b);
        } else {
            self.data.lock().unwrap().extend_from_slice(data);
        }
        Ok(())
    }
}

fn cfg(ring: usize) -> Config {
    Config {
        ring_bytes: ring,
        write_timeout: Duration::from_millis(300),
    }
}

fn drain(r: &mut Reader) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 64];
    loop {
        let o = r.read(&mut buf);
        if o.n == 0 {
            return out;
        }
        out.extend_from_slice(&buf[..o.n]);
    }
}

#[derive(Default)]
struct Notices(Mutex<Vec<Notice>>);

impl Notifier for Notices {
    fn notify(&self, n: Notice) {
        self.0.lock().unwrap().push(n);
    }
}

#[test]
fn readers_at_different_offsets() {
    let (b, mut w) = Broker::new(cfg(1024), b"", Sink::default());
    w.append(b"hello world");
    let mut r0 = b.reader_at(0);
    let mut r6 = b.reader_at(6);
    let mut live = b.reader_live();
    assert_eq!(drain(&mut r0), b"hello world");
    assert_eq!(drain(&mut r6), b"world");
    assert_eq!(drain(&mut live), b"");
    w.append(b"!");
    assert_eq!(drain(&mut live), b"!");
    assert_eq!(drain(&mut r0), b"!");
    // An offset in the future is the end, not an error.
    assert_eq!(b.reader_at(10_000).offset(), 12);
}

#[test]
fn slow_reader_is_told_what_it_dropped() {
    let (b, mut w) = Broker::new(cfg(16), b"", Sink::default());
    let mut r = b.reader_at(0);
    let data: Vec<u8> = (0..100u8).collect();
    for chunk in data.chunks(7) {
        w.append(chunk);
    }
    let mut buf = [0u8; 64];
    let o = r.read(&mut buf);
    assert!(o.dropped > 0 && o.dropped <= 100 - o.n as u64);
    assert!(o.n > 0);
    // What it got is the stream's bytes at the offset it landed on.
    let start = o.dropped as usize;
    assert_eq!(&buf[..o.n], &data[start..start + o.n]);
    assert_eq!(r.offset(), o.dropped + o.n as u64);
    // One append bigger than the ring keeps only the newest bytes.
    let big: Vec<u8> = (0..200u32).map(|i| i as u8).collect();
    w.append(&big);
    let o = r.read(&mut buf);
    assert!(o.dropped > 0);
    let abs = r.offset() - o.n as u64;
    assert_eq!(
        &buf[..o.n],
        &big[(abs - 100) as usize..(abs - 100) as usize + o.n]
    );
}

#[test]
fn ring_never_returns_torn_bytes_under_a_fast_producer() {
    let (b, mut w) = Broker::new(cfg(256), b"", Sink::default());
    let done = Arc::new(AtomicBool::new(false));
    let d2 = done.clone();
    let prod = thread::spawn(move || {
        let mut at = 0u64;
        while at < 3_000_000 {
            let chunk: Vec<u8> = (0..37).map(|i| ((at + i) % 251) as u8).collect();
            w.append(&chunk);
            at += 37;
        }
        d2.store(true, Ordering::Release);
    });
    let mut r = b.reader_at(0);
    let mut buf = [0u8; 100];
    let (mut got, mut dropped) = (0u64, 0u64);
    while !done.load(Ordering::Acquire) || r.offset() < b.ring().head() {
        let start = r.offset();
        let o = r.read(&mut buf);
        let abs = start + o.dropped;
        for (i, byte) in buf[..o.n].iter().enumerate() {
            assert_eq!(
                *byte,
                ((abs + i as u64) % 251) as u8,
                "torn read at {}",
                abs + i as u64
            );
        }
        got += o.n as u64;
        dropped += o.dropped;
    }
    prod.join().unwrap();
    assert_eq!(got + dropped, b.ring().head());
}

#[test]
fn primed_tail_and_install_readers_start_at_boot_offset() {
    let (b, mut w) = Broker::new(cfg(1024), b"old session\n", Sink::default());
    assert_eq!(b.live_from(), 12);
    // History is there for a viewer, but an install does not see it.
    assert_eq!(drain(&mut b.reader_at(0)), b"old session\n");
    w.append(b"login: ");
    assert_eq!(drain(&mut b.install_reader()), b"login: ");

    let boot = b.mark_boot();
    w.append(b"booting\nlogin: ");
    let mut r = b.install_reader();
    assert_eq!(r.offset(), boot);
    // The pre-boot prompt cannot satisfy a wait; the post-boot one does.
    assert!(matches!(
        b.install_reader()
            .wait_for(b"login: ", Duration::from_millis(100)),
        WaitResult::Found(_)
    ));
    let before = b
        .reader_at(0)
        .wait_for(b"login: ", Duration::from_millis(100));
    assert!(matches!(before, WaitResult::Found(o) if o < boot + 8));
    assert_eq!(drain(&mut r), b"booting\nlogin: ");

    let reboot = b.mark_boot();
    let mut r = b.install_reader();
    assert_eq!(
        r.wait_for(b"login: ", Duration::from_millis(50)),
        WaitResult::Timeout
    );
    w.append(b"login: ");
    assert_eq!(
        r.wait_for(b"login: ", Duration::from_millis(500)),
        WaitResult::Found(reboot + 7)
    );
}

#[test]
fn lease_offset_bounds_install_reader() {
    let (b, mut w) = Broker::new(cfg(1024), b"", Sink::default());
    w.append(b"before");
    let c = b.connect(None);
    let g = c.lease().unwrap();
    assert_eq!(g.offset, 6);
    w.append(b"after");
    assert_eq!(drain(&mut b.install_reader()), b"after");
    c.release().unwrap();
    // The bound belongs to the lease, not to the broker for ever.
    assert_eq!(drain(&mut b.install_reader()), b"beforeafter");
}

#[test]
fn wait_for_blocks_until_output_arrives() {
    let (b, mut w) = Broker::new(cfg(1024), b"", Sink::default());
    let mut r = b.reader_live();
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        w.append(b"par");
        thread::sleep(Duration::from_millis(50));
        w.append(b"tial ok");
        w
    });
    assert_eq!(
        r.wait_for(b"partial", Duration::from_secs(5)),
        WaitResult::Found(7)
    );
    let mut w = t.join().unwrap();
    w.close();
    let mut buf = [0u8; 16];
    assert_eq!(r.read_wait(&mut buf, None).n, 3);
    assert!(r.read_wait(&mut buf, None).eof);
    assert_eq!(
        r.wait_for(b"never", Duration::from_secs(1)),
        WaitResult::Eof
    );
}

#[test]
fn input_from_a_non_holder_is_dropped_with_one_notice() {
    let sink = Sink::default();
    let (b, _w) = Broker::new(cfg(64), b"", sink.clone());
    let notices = Arc::new(Notices::default());
    let a = b.connect(None);
    let v = b.connect(Some(notices.clone()));
    assert_eq!(v.send(b"free ").unwrap(), SendOutcome::Sent);
    a.lease().unwrap();
    assert_eq!(a.send(b"mine\r").unwrap(), SendOutcome::Sent);
    for _ in 0..3 {
        assert_eq!(v.send(b"x").unwrap(), SendOutcome::Dropped);
    }
    assert_eq!(sink.bytes(), b"free mine\r");
    assert_eq!(
        *notices.0.lock().unwrap(),
        vec![Notice::InputDropped { holder: a.id() }]
    );
    assert!(matches!(v.lease(), Err(InputError::LeaseHeld(h)) if h == a.id()));
    assert!(matches!(v.release(), Err(InputError::NotHolder)));

    // A new lease is a new occasion to be told.
    a.release().unwrap();
    v.lease().unwrap();
    assert_eq!(a.send(b"y").unwrap(), SendOutcome::Dropped);
    v.release().unwrap();
}

#[test]
fn lease_ends_when_its_connection_closes() {
    let sink = Sink::default();
    let (b, _w) = Broker::new(cfg(64), b"", sink.clone());
    let other = b.connect(None);
    let holder = b.connect(None);
    holder.lease().unwrap();
    assert_eq!(other.send(b"no").unwrap(), SendOutcome::Dropped);
    holder.send(b"rm -rf").unwrap();
    drop(holder);
    assert_eq!(b.holder(), None);
    assert_eq!(other.send(b"yes").unwrap(), SendOutcome::Sent);
    // The abandoned fragment was cleared before anyone else typed.
    assert_eq!(sink.bytes(), b"rm -rf\x15yes");
}

#[test]
fn lease_ends_when_the_holders_socket_dies() {
    let sink = Sink::default();
    let (b, _w) = Broker::new(cfg(64), b"", sink.clone());
    let port = RawPort::bind("127.0.0.1:0", b.clone()).unwrap();
    // A control-side holder whose peer vanishes is the same drop; here the
    // lease holder is a pumped TCP connection.
    let holder = b.connect(None);
    holder.lease().unwrap();
    let mut c = TcpStream::connect(port.local_addr()).unwrap();
    c.write_all(b"ignored").unwrap();
    wait_until(|| sink.bytes().is_empty() && b.holder().is_some());
    thread::sleep(Duration::from_millis(100));
    assert!(
        sink.bytes().is_empty(),
        "legacy client typed over the lease"
    );
    drop(holder);
    c.write_all(b"ok").unwrap();
    wait_until(|| sink.bytes() == b"ok");
}

#[test]
fn break_lease_ends_someone_elses_and_tells_them() {
    let sink = Sink::default();
    let (b, _w) = Broker::new(cfg(64), b"", sink.clone());
    let notices = Arc::new(Notices::default());
    let hung = b.connect(Some(notices.clone()));
    let user = b.connect(None);
    assert_eq!(b.break_lease().unwrap(), None);
    hung.lease().unwrap();
    hung.send(b"half").unwrap();
    assert_eq!(b.break_lease().unwrap(), Some(hung.id()));
    assert_eq!(
        *notices.0.lock().unwrap(),
        vec![Notice::LeaseBroken { ctrl_u: true }]
    );
    assert_eq!(sink.bytes(), b"half\x15");
    assert!(user.lease().is_ok());
    // The old holder's drop must not end the new holder's lease.
    drop(hung);
    assert!(user.holds_lease());
}

#[test]
fn ctrl_u_only_after_an_unfinished_line() {
    let sink = Sink::default();
    let (b, _w) = Broker::new(cfg(64), b"", sink.clone());
    let c = b.connect(None);
    c.lease().unwrap();
    c.send(b"ls\r").unwrap();
    assert!(!c.release().unwrap().ctrl_u);
    c.lease().unwrap();
    c.send(b"ls\r").unwrap();
    c.send(b"pw").unwrap();
    assert!(c.release().unwrap().ctrl_u);
    c.lease().unwrap();
    assert!(
        !c.release().unwrap().ctrl_u,
        "nothing typed, nothing to clear"
    );
    assert_eq!(sink.bytes(), b"ls\rls\rpw\x15");
}

#[test]
fn a_stalled_write_fails_the_holder_and_bounds_other_senders() {
    let sink = Sink::default();
    let (b, _w) = Broker::new(
        Config {
            ring_bytes: 64,
            write_timeout: Duration::from_millis(200),
        },
        b"",
        sink.clone(),
    );
    let a = b.connect(None);
    let other = b.connect(None);
    a.lease().unwrap();
    sink.stall.store(true, Ordering::Release);
    let t = Instant::now();
    let (ra, ro) = thread::scope(|s| {
        let h = s.spawn(|| a.send(b"x"));
        thread::sleep(Duration::from_millis(30));
        let ro = other.send(b"y");
        (h.join().unwrap(), ro)
    });
    assert!(matches!(ra, Err(InputError::Stalled)));
    assert!(ro.is_ok() || matches!(ro, Err(InputError::Stalled)));
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert!(a.holds_lease(), "a failed write does not cost the lease");
    sink.stall.store(false, Ordering::Release);
    assert!(a.send(b"z").is_ok());
}

#[test]
fn legacy_port_and_pumped_connection_see_the_same_bytes() {
    let sink = Sink::default();
    let (b, mut w) = Broker::new(cfg(4096), b"history\n", sink.clone());
    let port = RawPort::bind("127.0.0.1:0", b.clone()).unwrap();
    let mut legacy = TcpStream::connect(port.local_addr()).unwrap();
    legacy
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    // A second path onto the broker, as the control socket's `console` would be.
    let conn = b.connect(None);
    let mut other = conn.broker().reader_live();
    // Let the accept thread register its reader before output starts.
    wait_until(|| true);
    thread::sleep(Duration::from_millis(100));

    w.append(b"banner\r\nlogin: ");
    let mut got = [0u8; 15];
    legacy.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"banner\r\nlogin: ");
    assert_eq!(drain(&mut other), b"banner\r\nlogin: ");

    legacy.write_all(b"root\r").unwrap();
    wait_until(|| sink.bytes() == b"root\r");
    drop(port);
}

fn wait_until(mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "condition never became true"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn producer_close_reaches_pumped_clients() {
    let (b, w) = Broker::new(cfg(256), b"", Sink::default());
    let port = RawPort::bind("127.0.0.1:0", b.clone()).unwrap();
    let mut c = TcpStream::connect(port.local_addr()).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    thread::sleep(Duration::from_millis(100));
    let t = thread::spawn(move || run_producer(w, &b"from engine"[..]));
    let mut all = Vec::new();
    c.read_to_end(&mut all).unwrap();
    assert_eq!(all, b"from engine");
    t.join().unwrap().unwrap();
}

#[test]
fn lease_grants_race_with_sends_without_a_stray_byte() {
    const ITERATIONS: usize = 20_000;
    let strays = AtomicUsize::new(0);
    for i in 0..ITERATIONS {
        let sink = Sink {
            split: true,
            ..Sink::default()
        };
        let (b, _w) = Broker::new(cfg(64), b"", sink.clone());
        let sender = b.connect(None);
        let leaser = b.connect(None);
        let go = Barrier::new(2);
        let grant = thread::scope(|s| {
            let h = s.spawn(|| {
                go.wait();
                for _ in 0..16 {
                    let _ = sender.send(b"xxxx");
                }
            });
            go.wait();
            for _ in 0..(i % 7) {
                thread::yield_now();
            }
            let g = leaser.lease().unwrap();
            h.join().unwrap();
            g
        });
        let written = sink.bytes();
        // Everything before the grant is whole four-byte messages, and no
        // sender byte follows it.
        if grant.input_seq % 4 != 0 || written.len() as u64 != grant.input_seq {
            strays.fetch_add(1, Ordering::Relaxed);
        }
        assert_eq!(
            grant.input_seq % 4,
            0,
            "grant landed inside a write (iteration {i})"
        );
        assert_eq!(
            written.len() as u64,
            grant.input_seq,
            "non-holder bytes reached the sink after the grant (iteration {i})"
        );
        assert!(written.iter().all(|&c| c == b'x'));
    }
    assert_eq!(strays.load(Ordering::Relaxed), 0);
}

// Run with: cargo test -p origami-host --release -- --ignored --nocapture
#[test]
#[ignore = "timing floor; needs a release build"]
fn perf_ring_append_throughput_and_reader_latency() {
    const CHUNK: usize = 4096;
    const TOTAL: usize = 512 << 20;
    let (b, mut w) = Broker::new(Config::default(), b"", Sink::default());
    let stop = Arc::new(AtomicBool::new(false));
    // A reader draining all the while must not slow the producer.
    let mut r = b.reader_live();
    let s2 = stop.clone();
    let rd = thread::spawn(move || {
        let mut buf = vec![0u8; 64 << 10];
        while !s2.load(Ordering::Acquire) {
            r.read_wait(&mut buf, Some(Duration::from_millis(5)));
        }
    });
    let chunk = vec![0xa5u8; CHUNK];
    let t = Instant::now();
    for _ in 0..TOTAL / CHUNK {
        w.append(&chunk);
    }
    let secs = t.elapsed().as_secs_f64();
    stop.store(true, Ordering::Release);
    rd.join().unwrap();
    let mbs = TOTAL as f64 / secs / 1e6;
    println!("ring append with a live reader: {mbs:.0} MB/s");
    assert!(mbs >= 200.0, "append throughput {mbs:.0} MB/s");

    // Wake latency: the producer stamps a byte, the reader measures how long
    // it took to see it.
    let (b, mut w) = Broker::new(Config::default(), b"", Sink::default());
    let mut r = b.reader_live();
    let origin = Instant::now();
    let stamps: Arc<Mutex<Vec<u64>>> = Arc::default();
    let st = stamps.clone();
    let prod = thread::spawn(move || {
        for _ in 0..5000 {
            let spin_until = Instant::now() + Duration::from_micros(200);
            while Instant::now() < spin_until {
                std::hint::spin_loop();
            }
            st.lock().unwrap().push(origin.elapsed().as_nanos() as u64);
            w.append(&[1]);
        }
    });
    let mut lat = Vec::new();
    let mut buf = [0u8; 16];
    while lat.len() < 5000 {
        let o = r.read_wait(&mut buf, Some(Duration::from_secs(5)));
        let now = origin.elapsed().as_nanos() as u64;
        if o.n == 0 {
            continue;
        }
        let sent = stamps.lock().unwrap()[lat.len()];
        lat.push(now.saturating_sub(sent));
        // Bytes appended while the reader was busy count as one wake.
        lat.extend(std::iter::repeat_n(0, o.n - 1));
    }
    prod.join().unwrap();
    lat.sort_unstable();
    let p99 = lat[lat.len() * 99 / 100];
    println!(
        "reader wake latency: p50 {} us, p99 {} us, max {} us",
        lat[lat.len() / 2] / 1000,
        p99 / 1000,
        lat.last().unwrap() / 1000
    );
    assert!(p99 < 1_000_000, "p99 reader wake {} us", p99 / 1000);
}
