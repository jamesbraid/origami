//! Drives Ethernet frames through the switch and stack with no QEMU. The
//! test plays QEMU's two stream netdevs over loopback sockets and plays
//! the guest with a second smoltcp stack.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use origami_net::{StackConfig, Switch, SwitchConfig};
use origami_nfs::{Options, Server, Share};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, ChecksumCapabilities, Device, DeviceCapabilities, Medium};
use smoltcp::socket::{icmp, tcp, udp};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{
    EthernetAddress, HardwareAddress, Icmpv4Packet, Icmpv4Repr, IpAddress, IpCidr, IpEndpoint,
};

const STACK_MAC: [u8; 6] = [0x08, 0x00, 0x69, 0x00, 0x00, 0x04];
const STACK_IP: Ipv4Addr = Ipv4Addr::new(10, 98, 0, 4);
const GUEST_MAC: [u8; 6] = [0x08, 0x00, 0x69, 0xff, 0xda, 0xf7];
const GUEST_IP: Ipv4Addr = Ipv4Addr::new(10, 98, 0, 15);

#[derive(Default)]
struct Wire {
    rx: VecDeque<Vec<u8>>,
    tx: VecDeque<Vec<u8>>,
}
struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut VecDeque<Vec<u8>>);
impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}
impl phy::TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut b = vec![0; len];
        let r = f(&mut b);
        self.0.push_back(b);
        r
    }
}
impl Device for Wire {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;
    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ethernet;
        c.max_transmission_unit = 1514;
        c.checksum = ChecksumCapabilities::default();
        c
    }
    fn receive(&mut self, _: SmolInstant) -> Option<(Rx, Tx<'_>)> {
        let f = self.rx.pop_front()?;
        Some((Rx(f), Tx(&mut self.tx)))
    }
    fn transmit(&mut self, _: SmolInstant) -> Option<Tx<'_>> {
        Some(Tx(&mut self.tx))
    }
}

fn write_frame(s: &mut TcpStream, f: &[u8]) {
    let mut b = (f.len() as u32).to_be_bytes().to_vec();
    b.extend_from_slice(f);
    s.write_all(&b).unwrap();
}

fn spawn_reader(mut s: TcpStream) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || loop {
        let mut h = [0u8; 4];
        if s.read_exact(&mut h).is_err() {
            return;
        }
        let mut f = vec![0u8; u32::from_be_bytes(h) as usize];
        if s.read_exact(&mut f).is_err() || tx.send(f).is_err() {
            return;
        }
    });
    rx
}

struct Guest {
    wire: Wire,
    iface: Interface,
    sockets: SocketSet<'static>,
    a: TcpStream,
    a_rx: Receiver<Vec<u8>>,
    start: Instant,
}

impl Guest {
    fn poll(&mut self) {
        while let Ok(f) = self.a_rx.try_recv() {
            self.wire.rx.push_back(f);
        }
        let now = SmolInstant::from_micros(self.start.elapsed().as_micros() as i64);
        self.iface.poll(now, &mut self.wire, &mut self.sockets);
        while let Some(f) = self.wire.tx.pop_front() {
            write_frame(&mut self.a, &f);
        }
    }

    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Guest) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.poll();
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn udp_call(&mut self, port: u16, call: &[u8]) -> Vec<u8> {
        self.udp_call_to(STACK_IP, port, call).0
    }

    /// Calls `ip` and returns the reply with the address it came from.
    fn udp_call_to(&mut self, ip: Ipv4Addr, port: u16, call: &[u8]) -> (Vec<u8>, IpAddress) {
        let h = self.sockets.add(udp::Socket::new(
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 65535]),
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 65535]),
        ));
        self.sockets
            .get_mut::<udp::Socket>(h)
            .bind(0)
            .unwrap_or_else(|_| self.sockets.get_mut::<udp::Socket>(h).bind(40000).unwrap());
        let to = IpEndpoint::new(IpAddress::Ipv4(ip), port);
        self.sockets
            .get_mut::<udp::Socket>(h)
            .send_slice(call, to)
            .unwrap();
        let mut reply = Vec::new();
        let mut from = to.addr;
        self.until("a UDP reply", |g| {
            if let Ok((d, meta)) = g.sockets.get_mut::<udp::Socket>(h).recv() {
                assert_eq!(meta.endpoint.port, port);
                reply = d.to_vec();
                from = meta.endpoint.addr;
                true
            } else {
                false
            }
        });
        self.sockets.remove(h);
        (reply, from)
    }

    fn tcp_connect(&mut self, port: u16, local: u16) -> SocketHandle {
        self.tcp_connect_to(STACK_IP, port, local)
    }

    fn tcp_connect_to(&mut self, ip: Ipv4Addr, port: u16, local: u16) -> SocketHandle {
        let h = self.sockets.add(tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; 65535]),
            tcp::SocketBuffer::new(vec![0; 65535]),
        ));
        let cx = self.iface.context();
        self.sockets
            .get_mut::<tcp::Socket>(h)
            .connect(cx, IpEndpoint::new(IpAddress::Ipv4(ip), port), local)
            .unwrap();
        self.until("the TCP handshake", |g| {
            g.sockets.get_mut::<tcp::Socket>(h).may_send()
        });
        h
    }

    /// Sends one record-marked call and returns the reply record.
    fn tcp_call(&mut self, h: SocketHandle, call: &[u8]) -> Vec<u8> {
        let mut rec = (0x8000_0000u32 | call.len() as u32).to_be_bytes().to_vec();
        rec.extend_from_slice(call);
        self.sockets
            .get_mut::<tcp::Socket>(h)
            .send_slice(&rec)
            .unwrap();
        let mut got = Vec::new();
        self.until("a TCP reply", |g| {
            let s = g.sockets.get_mut::<tcp::Socket>(h);
            let _ = s.recv(|b| {
                got.extend_from_slice(b);
                (b.len(), ())
            });
            got.len() >= 4
                && got.len()
                    >= 4 + (u32::from_be_bytes(got[..4].try_into().unwrap()) & 0x7fff_ffff) as usize
        });
        got.split_off(4)
    }
}

struct Rig {
    switch: Switch,
    guest: Guest,
    b: TcpStream,
    b_rx: Receiver<Vec<u8>>,
    _dir: tempfile::TempDir,
}

fn rig() -> Rig {
    rig_with(vec![])
}

fn rig_with(aliases: Vec<Ipv4Addr>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), b"hi\n").unwrap();
    std::fs::write(dir.path().join("big"), vec![b'x'; 8192]).unwrap();
    let share = Share::open("src", dir.path(), Options::default()).unwrap();
    let server = Arc::new(Server::new(Arc::new(share)));
    let cfg = SwitchConfig::new(StackConfig {
        mac: STACK_MAC,
        ip: STACK_IP,
        prefix: 24,
        aliases,
        addressing: None,
    });
    let switch = Switch::start(cfg, Some(server)).unwrap();
    let a = TcpStream::connect(switch.guest_addr()).unwrap();
    let b = TcpStream::connect(switch.network_addr()).unwrap();
    let a_rx = spawn_reader(a.try_clone().unwrap());
    let b_rx = spawn_reader(b.try_clone().unwrap());
    // The switch takes each connection on its own thread. Wait until it
    // forwards in both directions before the test sends anything.
    let probe = |dst: [u8; 6]| {
        let mut f = vec![0u8; 60];
        f[0..6].copy_from_slice(&dst);
        f[12] = 8;
        f[30..34].copy_from_slice(&[10, 98, 0, 99]);
        f
    };
    let (mut a, mut b) = (a, b);
    let (a_rx, b_rx) = (a_rx, b_rx);
    // A frame sent before the other side is registered is dropped, as on
    // a dead link, so keep probing until one arrives.
    let arrived = |from: &mut TcpStream, to: &Receiver<Vec<u8>>, dst: [u8; 6]| {
        for _ in 0..100 {
            write_frame(from, &probe(dst));
            if to.recv_timeout(Duration::from_millis(100)).is_ok() {
                thread::sleep(Duration::from_millis(100));
                while to.try_recv().is_ok() {}
                return;
            }
        }
        panic!("the switch never forwarded a probe");
    };
    arrived(&mut a, &b_rx, [0x52, 0x55, 0xc0, 0, 2, 2]);
    arrived(&mut b, &a_rx, [0x52, 0x55, 0xc0, 0, 2, 3]);
    let mut wire = Wire::default();
    let mut icfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(GUEST_MAC)));
    icfg.random_seed = 7;
    let mut iface = Interface::new(icfg, &mut wire, SmolInstant::from_micros(0));
    iface.update_ip_addrs(|a| {
        a.push(IpCidr::new(IpAddress::Ipv4(GUEST_IP), 24)).unwrap();
    });
    let guest = Guest {
        wire,
        iface,
        sockets: SocketSet::new(Vec::new()),
        a,
        a_rx,
        start: Instant::now(),
    };
    Rig {
        switch,
        guest,
        b,
        b_rx,
        _dir: dir,
    }
}

fn rpc_call(xid: u32, prog: u32, vers: u32, proc_: u32, args: &[u32]) -> Vec<u8> {
    let mut v = Vec::new();
    for w in [xid, 0, 2, prog, vers, proc_, 0, 0, 0, 0] {
        v.extend_from_slice(&w.to_be_bytes());
    }
    for w in args {
        v.extend_from_slice(&w.to_be_bytes());
    }
    v
}

fn words(b: &[u8]) -> Vec<u32> {
    b.chunks(4)
        .map(|c| u32::from_be_bytes(c.try_into().unwrap()))
        .collect()
}

#[test]
fn arp_and_ping_reach_the_stack() {
    let mut r = rig();
    let id = 0x1234;
    let h = r.guest.sockets.add(icmp::Socket::new(
        icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]),
        icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]),
    ));
    r.guest
        .sockets
        .get_mut::<icmp::Socket>(h)
        .bind(icmp::Endpoint::Ident(id))
        .unwrap();
    let repr = Icmpv4Repr::EchoRequest {
        ident: id,
        seq_no: 1,
        data: b"ping",
    };
    let buf = r
        .guest
        .sockets
        .get_mut::<icmp::Socket>(h)
        .send(repr.buffer_len(), IpAddress::Ipv4(STACK_IP))
        .unwrap();
    repr.emit(
        &mut Icmpv4Packet::new_unchecked(buf),
        &ChecksumCapabilities::default(),
    );
    let mut pong = false;
    r.guest.until("an echo reply", |g| {
        if let Ok((payload, _)) = g.sockets.get_mut::<icmp::Socket>(h).recv() {
            let p = Icmpv4Packet::new_checked(payload).unwrap();
            let rep = Icmpv4Repr::parse(&p, &ChecksumCapabilities::default()).unwrap();
            pong = matches!(
                rep,
                Icmpv4Repr::EchoReply {
                    ident: 0x1234,
                    seq_no: 1,
                    ..
                }
            );
            true
        } else {
            false
        }
    });
    assert!(pong);
    // The guest's ARP request was a broadcast, so the network saw it; the
    // echo request, addressed to the stack, never reached the network.
    let mut seen = Vec::new();
    while let Ok(f) = r.b_rx.recv_timeout(Duration::from_millis(200)) {
        seen.push(f);
    }
    assert!(
        seen.iter()
            .all(|f| f[0..6] == [0xff; 6] && f[12..14] == [8, 6]),
        "{seen:?}"
    );
    assert!(!seen.is_empty());
}

#[test]
fn portmap_answers_over_udp() {
    let mut r = rig();
    // GETPORT for NFS version 3 over UDP.
    let call = rpc_call(0x11, 100000, 2, 3, &[100003, 3, 17, 0]);
    let w = words(&r.guest.udp_call(111, &call));
    // xid, REPLY, MSG_ACCEPTED, verifier flavor and length, SUCCESS, port.
    assert_eq!(w, [0x11, 1, 0, 0, 0, 0, 2049]);
}

#[test]
fn nfs_null_answers_over_tcp() {
    let mut r = rig();
    let h = r.guest.tcp_connect(2049, 49152);
    let w = words(&r.guest.tcp_call(h, &rpc_call(0x22, 100003, 3, 0, &[])));
    assert_eq!(w, [0x22, 1, 0, 0, 0, 0]);
    // A second call on the same connection, then the mount protocol.
    let w = words(&r.guest.tcp_call(h, &rpc_call(0x23, 100003, 3, 0, &[])));
    assert_eq!(w, [0x23, 1, 0, 0, 0, 0]);
    let h2 = r.guest.tcp_connect(635, 49153);
    let w = words(&r.guest.tcp_call(h2, &rpc_call(0x24, 100005, 3, 0, &[])));
    assert_eq!(w, [0x24, 1, 0, 0, 0, 0]);
}

#[test]
fn other_traffic_goes_to_the_network_and_back() {
    let mut r = rig();
    let other = [0x52, 0x55, 0xc0, 0, 2, 2];
    let mut f = vec![0u8; 60];
    f[0..6].copy_from_slice(&other);
    f[6..12].copy_from_slice(&GUEST_MAC);
    f[12] = 8;
    f[30..34].copy_from_slice(&[10, 98, 0, 2]);
    write_frame(&mut r.guest.a, &f);
    let got = r.b_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(got, f);
    // And back from the network to the guest only.
    let mut g = f.clone();
    g[0..6].copy_from_slice(&GUEST_MAC);
    g[6..12].copy_from_slice(&other);
    write_frame(&mut r.b, &g);
    let back = r.guest.a_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(back, g);
    assert!(r.b_rx.recv_timeout(Duration::from_millis(200)).is_err());
    let _: SocketAddr = r.switch.guest_addr();
}

#[test]
fn a_reconnecting_guest_link_is_picked_up() {
    let mut r = rig();
    let first = r.guest.udp_call(111, &rpc_call(1, 100000, 2, 0, &[]));
    assert_eq!(words(&first), [1, 1, 0, 0, 0, 0]);
    // QEMU reconnects after a restart of `run`.
    r.guest.a.shutdown(std::net::Shutdown::Both).unwrap();
    r.guest.a = TcpStream::connect(r.switch.guest_addr()).unwrap();
    r.guest.a_rx = spawn_reader(r.guest.a.try_clone().unwrap());
    let again = r.guest.udp_call(111, &rpc_call(2, 100000, 2, 0, &[]));
    assert_eq!(words(&again), [2, 1, 0, 0, 0, 0]);
}

#[test]
fn an_nfs2_read_reply_crosses_the_stack_as_fragments() {
    let mut r = rig();
    // MOUNT version 1 of "/src" gives a status and a 32-byte handle.
    let mut call = rpc_call(0x31, 100005, 1, 1, &[4]);
    call.extend_from_slice(b"/src");
    let reply = r.guest.udp_call(635, &call);
    assert_eq!(words(&reply[..28])[6], 0, "mount status");
    let fh = reply[28..60].to_vec();
    // LOOKUP "big" in the root: fh, then the name.
    let mut call = rpc_call(0x32, 100003, 2, 4, &[]);
    call.extend_from_slice(&fh);
    call.extend_from_slice(&3u32.to_be_bytes());
    call.extend_from_slice(b"big\0");
    let reply = r.guest.udp_call(2049, &call);
    assert_eq!(words(&reply[..28])[6], 0, "lookup status");
    let big = reply[28..60].to_vec();
    // READ 8192 bytes: more than a frame, so the datagram is fragmented.
    let mut call = rpc_call(0x33, 100003, 2, 6, &[]);
    call.extend_from_slice(&big);
    for w in [0u32, 8192, 8192] {
        call.extend_from_slice(&w.to_be_bytes());
    }
    let reply = r.guest.udp_call(2049, &call);
    assert!(reply.len() > 8192, "{}", reply.len());
    assert_eq!(words(&reply[..28])[6], 0, "read status");
    // 24 header bytes, status, 17 attribute words, the length, the data.
    let data = &reply[24 + 4 + 68 + 4..];
    assert_eq!(&data[..8192], &vec![b'x'; 8192][..]);
}

#[test]
fn an_alias_answers_as_itself() {
    let alias = Ipv4Addr::new(10, 98, 0, 6);
    let mut r = rig_with(vec![alias]);
    // UDP: the reply comes from the address that was called, not the
    // stack's primary one.
    let call = rpc_call(0x41, 100000, 2, 3, &[100003, 3, 17, 0]);
    let (reply, from) = r.guest.udp_call_to(alias, 111, &call);
    assert_eq!(words(&reply), [0x41, 1, 0, 0, 0, 0, 2049]);
    assert_eq!(from, IpAddress::Ipv4(alias));
    let (_, from) = r.guest.udp_call_to(STACK_IP, 111, &call);
    assert_eq!(from, IpAddress::Ipv4(STACK_IP));
    // TCP connects to either.
    let h = r.guest.tcp_connect_to(alias, 2049, 49160);
    let w = words(&r.guest.tcp_call(h, &rpc_call(0x42, 100003, 3, 0, &[])));
    assert_eq!(w, [0x42, 1, 0, 0, 0, 0]);
}
