//! The server's own behavior: the UDP reply cache, the transport-agnostic
//! core, and `ShareServer` on host sockets.

use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use origami_nfs::sharefs::{Options, Share};
use origami_nfs::{Ports, Server, ShareConfig, ShareServer};

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        let p = std::env::temp_dir().join(format!("origami-nfs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn u32s(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_be_bytes()).collect()
}

fn opaque(s: &[u8]) -> Vec<u8> {
    let mut v = (s.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(s);
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
    v
}

/// An RPC call with AUTH_NULL credentials.
fn call(xid: u32, prog: u32, vers: u32, proc_: u32, args: &[u8]) -> Vec<u8> {
    let mut v = u32s(&[xid, 0, 2, prog, vers, proc_, 0, 0, 0, 0]);
    v.extend_from_slice(args);
    v
}

fn word(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap())
}

const NFS: u32 = 100003;
const MOUNT: u32 = 100005;
const PORTMAP: u32 = 100000;

fn share_server(dir: &std::path::Path, table: &std::path::Path) -> Server {
    let share = Share::open(
        "s",
        dir,
        Options {
            path_table: Some(table.to_path_buf()),
            ..Options::default()
        },
    )
    .unwrap();
    Server::new(Arc::new(share))
}

// A UDP client that hears no reply sends the same call again. A second
// REMOVE run for real would be NOENT although the first worked, so the
// server must answer it from the first reply.
#[test]
fn udp_retransmission_is_answered_from_cache() {
    let d = Dir::new("drc");
    let share = d.0.join("share");
    fs::create_dir_all(&share).unwrap();
    fs::write(share.join("f"), "").unwrap();
    let server = share_server(&share, &d.0.join("handles"));
    let from: SocketAddr = "127.0.0.1:4000".parse().unwrap();

    let mnt = server
        .handle_datagram(from, &call(1, MOUNT, 1, 1, &opaque(b"/s")))
        .unwrap();
    let root = &mnt[28..60];
    let mut args = root.to_vec();
    args.extend(opaque(b"f"));
    let remove = call(2, NFS, 2, 10, &args);

    let first = server.handle_datagram(from, &remove).unwrap();
    assert_eq!(word(&first, 24), 0, "REMOVE status");
    assert_eq!(
        server.handle_datagram(from, &remove).unwrap(),
        first,
        "retransmitted REMOVE"
    );

    // The same bytes from another address are another client's call.
    let other: SocketAddr = "127.0.0.1:4001".parse().unwrap();
    assert_eq!(
        word(&server.handle_datagram(other, &remove).unwrap(), 24),
        2,
        "other client sees NOENT"
    );

    // A new call, a new xid, runs for real.
    let mut again = remove.clone();
    again[3] = 3;
    assert_eq!(
        word(&server.handle_datagram(from, &again).unwrap(), 24),
        2,
        "second REMOVE is NOENT"
    );
}

#[test]
fn reads_are_not_cached() {
    let d = Dir::new("drc-read");
    let share = d.0.join("share");
    fs::create_dir_all(&share).unwrap();
    fs::write(share.join("f"), "one").unwrap();
    let server = share_server(&share, &d.0.join("handles"));
    let from: SocketAddr = "127.0.0.1:4000".parse().unwrap();
    let mnt = server
        .handle_datagram(from, &call(1, MOUNT, 1, 1, &opaque(b"/s")))
        .unwrap();
    let mut args = mnt[28..60].to_vec();
    args.extend(opaque(b"f"));
    let lookup = server
        .handle_datagram(from, &call(2, NFS, 2, 4, &args))
        .unwrap();
    let fh = &lookup[28..60];
    let mut rd = fh.to_vec();
    rd.extend(u32s(&[0, 100, 0]));
    let read = call(3, NFS, 2, 6, &rd);
    let a = server.handle_datagram(from, &read).unwrap();
    fs::write(share.join("f"), "two").unwrap();
    let b = server.handle_datagram(from, &read).unwrap();
    assert_ne!(a, b, "a repeated READ must run again");
}

/// An in-memory stream: reads come from `input`, writes collect in `out`.
struct Duplex {
    input: Cursor<Vec<u8>>,
    out: Vec<u8>,
}

impl Read for Duplex {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.input.read(b)
    }
}

impl Write for Duplex {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.out.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn marked(msg: &[u8]) -> Vec<u8> {
    let mut v = (0x8000_0000u32 | msg.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(msg);
    v
}

// The core takes any byte stream, so a userspace network stack can carry
// it with no host socket involved.
#[test]
fn serve_stream_answers_marked_records_from_any_stream() {
    let d = Dir::new("stream");
    let share = d.0.join("share");
    fs::create_dir_all(&share).unwrap();
    let server = share_server(&share, &d.0.join("handles"));
    server.set_ports(
        Ports::default(),
        Ports {
            portmap: 111,
            mount: 635,
            nfs: 2049,
        },
    );

    let getport = call(5, PORTMAP, 2, 3, &u32s(&[NFS, 3, 6, 0]));
    let mut input = marked(&getport);
    input.extend(marked(&call(6, NFS, 3, 0, &[])));
    let mut s = Duplex {
        input: Cursor::new(input),
        out: Vec::new(),
    };
    server
        .serve_stream("127.0.0.1:1".parse().unwrap(), &mut s, None)
        .unwrap();

    let out = s.out;
    let first = word(&out, 0);
    assert_eq!(first & 0x8000_0000, 0x8000_0000);
    let len = (first & 0x7fff_ffff) as usize;
    assert_eq!(word(&out, 4), 5, "xid");
    assert_eq!(
        word(&out, 4 + 24),
        2049,
        "NFS v3 over TCP is advertised at 2049"
    );
    let second = &out[4 + len..];
    assert_eq!(word(second, 4), 6);
    assert_eq!(second.len(), 4 + 24, "NULL answers with an empty body");
}

#[test]
fn a_denied_client_gets_no_answer() {
    let d = Dir::new("deny");
    let share = d.0.join("share");
    fs::create_dir_all(&share).unwrap();
    let server = share_server(&share, &d.0.join("handles")).with_allow(|ip| ip.is_loopback());
    let ping = call(1, NFS, 3, 0, &[]);
    assert!(server
        .handle_datagram("127.0.0.1:9".parse().unwrap(), &ping)
        .is_some());
    assert!(server
        .handle_datagram("192.0.2.1:9".parse().unwrap(), &ping)
        .is_none());
}

#[test]
fn garbage_is_ignored_or_refused() {
    let d = Dir::new("garbage");
    let share = d.0.join("share");
    fs::create_dir_all(&share).unwrap();
    let server = share_server(&share, &d.0.join("handles"));
    let from: SocketAddr = "127.0.0.1:9".parse().unwrap();
    assert!(server.handle_datagram(from, &[]).is_none());
    assert!(
        server.handle_datagram(from, &u32s(&[1, 1, 0])).is_none(),
        "a reply is not a call"
    );
    // A call too short to hold its credential is GARBAGE_ARGS.
    let r = server
        .handle_datagram(from, &u32s(&[7, 0, 2, NFS, 3, 0, 1, 400]))
        .unwrap();
    assert_eq!(word(&r, 20), 4);
}

#[test]
fn share_server_serves_both_transports_and_stops() {
    let d = Dir::new("shareserver");
    let share = d.0.join("share");
    fs::create_dir_all(&share).unwrap();
    fs::write(share.join("hello"), "hi").unwrap();

    let mut cfg = ShareConfig::new("src", &share);
    cfg.path_table = Some(d.0.join("handles"));
    let server = ShareServer::start(cfg).unwrap();
    let ports = server.ports();
    let (udp, tcp) = (ports.udp.unwrap(), ports.tcp.unwrap());
    assert!(!ports.fell_back);
    assert_ne!(udp.port(), 0);

    // Portmap over UDP names the port it was asked on.
    let c = UdpSocket::bind("127.0.0.1:0").unwrap();
    c.connect(udp).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.send(&call(1, PORTMAP, 2, 3, &u32s(&[NFS, 2, 17, 0])))
        .unwrap();
    let mut buf = [0u8; 512];
    let n = c.recv(&mut buf).unwrap();
    assert_eq!(word(&buf[..n], 24), udp.port() as u32);

    // MOUNT over TCP, with no portmap call first.
    let mut t = TcpStream::connect(tcp).unwrap();
    t.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    t.write_all(&marked(&call(2, MOUNT, 3, 1, &opaque(b"/src"))))
        .unwrap();
    let mut hdr = [0u8; 4];
    t.read_exact(&mut hdr).unwrap();
    let mut reply = vec![0u8; (u32::from_be_bytes(hdr) & 0x7fff_ffff) as usize];
    t.read_exact(&mut reply).unwrap();
    assert_eq!(word(&reply, 24), 0, "mountstat3 OK");
    assert_eq!(word(&reply, 28), 32, "a 32-byte handle");

    server.stop();
    // Stopped: the client's connection ends, and the ports are free again.
    let mut rest = Vec::new();
    let _ = t.read_to_end(&mut rest);
    assert!(rest.is_empty());
    UdpSocket::bind(udp).expect("udp port released");
    std::net::TcpListener::bind(tcp).expect("tcp port released");
}

#[test]
fn a_taken_preferred_port_falls_back() {
    let d = Dir::new("fallback");
    let share = d.0.join("share");
    fs::create_dir_all(&share).unwrap();
    let held = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut cfg = ShareConfig::new("src", &share);
    cfg.path_table = Some(d.0.join("handles"));
    cfg.preferred_port = Some(held.local_addr().unwrap().port());
    let server = ShareServer::start(cfg).unwrap();
    assert!(server.ports().fell_back);
    assert_ne!(
        server.ports().udp.unwrap().port(),
        held.local_addr().unwrap().port()
    );
}
