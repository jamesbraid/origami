//! How fast console output goes through the broker compared with a direct
//! loopback connection. Run by hand: `cargo test --release -p origami-host
//! --test throughput -- --ignored --nocapture`.

use origami_host::broker::{run_producer, Broker, Config, InputSink, RawPort};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

const TOTAL: usize = 256 << 20;

struct Null;

impl InputSink for Null {
    fn write_input(&mut self, _: &[u8], _: Duration) -> std::io::Result<()> {
        Ok(())
    }
}

fn guest(listener: TcpListener) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let chunk = vec![b'x'; 16 * 1024];
        let mut sent = 0;
        while sent < TOTAL {
            s.write_all(&chunk).unwrap();
            sent += chunk.len();
        }
    })
}

fn drain(mut s: TcpStream) -> (usize, Duration) {
    let started = Instant::now();
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = 0;
    while let Ok(n) = s.read(&mut buf) {
        if n == 0 {
            break;
        }
        got += n;
    }
    (got, started.elapsed())
}

#[test]
#[ignore = "a measurement, run by hand"]
fn broker_against_direct() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let g = guest(l.try_clone().unwrap());
    let (got, took) = drain(TcpStream::connect(l.local_addr().unwrap()).unwrap());
    g.join().unwrap();
    let direct = got as f64 / took.as_secs_f64() / 1e6;
    println!("direct: {got} bytes in {took:?} = {direct:.0} MB/s");

    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let (broker, writer) = Broker::new(Config::default(), b"", Null);
    let g = guest(l.try_clone().unwrap());
    let engine = TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let p = thread::spawn(move || run_producer(writer, engine));
    let port = RawPort::bind("127.0.0.1:0", broker).unwrap();
    let (got, took) = drain(TcpStream::connect(port.local_addr()).unwrap());
    g.join().unwrap();
    p.join().unwrap().unwrap();
    let brokered = got as f64 / took.as_secs_f64() / 1e6;
    println!("through the broker: {got} bytes in {took:?} = {brokered:.0} MB/s");
}
