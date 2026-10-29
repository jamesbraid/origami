//! The userspace TCP/IP stack: ARP, ICMP echo, and UDP and TCP sockets
//! feeding an NFS [`Server`], all on one thread, with a worker thread for
//! the UDP calls and one for each TCP connection so that disk I/O never
//! stalls the network.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use origami_nfs::Server;
use smoltcp::iface::{Config, Interface, PollResult, SocketHandle, SocketSet};
use smoltcp::phy::{self, ChecksumCapabilities, Device, DeviceCapabilities, Medium};
use smoltcp::socket::{tcp, udp};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint};

use crate::host::{Addressing, Host, Verdict};
use crate::PORTS;

/// Bytes of a TCP stream queued for a connection's worker before the stack
/// stops reading from the socket and lets the window close.
const WORKER_QUEUE: usize = 1 << 20;
/// Idle listeners kept for each TCP port.
const LISTENERS: usize = 4;
const SOCKET_BUFFER: usize = 256 * 1024;
const MAX_POLL: Duration = Duration::from_millis(50);

/// Where the stack lives on the network.
#[derive(Clone, Debug)]
pub struct StackConfig {
    pub mac: [u8; 6],
    pub ip: Ipv4Addr,
    /// The network's mask length.
    pub prefix: u8,
    /// BOOTP and DHCP for one machine, when the stack is the network's only
    /// host. None leaves addressing to whatever else is on the network.
    pub addressing: Option<Addressing>,
}

enum Event {
    Frame(Vec<u8>),
    UdpReply {
        port: u16,
        to: IpEndpoint,
        data: Vec<u8>,
    },
    TcpData {
        handle: SocketHandle,
        data: Vec<u8>,
    },
    TcpDone {
        handle: SocketHandle,
    },
    Stop,
}

/// The running stack. Dropping it stops it.
pub struct Stack {
    events: SyncSender<Event>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Stack {
    /// Starts the stack, serving `server` when there is one. `out` is
    /// called, on the stack's thread, with each Ethernet frame it sends.
    pub fn start(
        cfg: StackConfig,
        server: Option<Arc<Server>>,
        out: impl FnMut(Vec<u8>) + Send + 'static,
    ) -> Stack {
        let (events, rx) = mpsc::sync_channel(4096);
        let stop = Arc::new(AtomicBool::new(false));
        let inner = Inner::new(cfg, server, events.clone(), stop.clone(), Box::new(out));
        let thread = thread::Builder::new()
            .name("origami-net".into())
            .spawn(move || inner.run(rx))
            .expect("spawn network stack thread");
        Stack {
            events,
            stop,
            thread: Some(thread),
        }
    }

    /// Hands the stack a frame the guest sent. Blocks while the stack is
    /// behind, which pushes back on the sender.
    pub fn receive(&self, frame: Vec<u8>) {
        let _ = self.events.send(Event::Frame(frame));
    }

    /// Stops the stack and its workers.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.events.send(Event::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The frames going into and out of smoltcp.
#[derive(Default)]
struct Link {
    rx: VecDeque<Vec<u8>>,
    tx: VecDeque<Vec<u8>>,
}

struct RxToken(Vec<u8>);
struct TxToken<'a>(&'a mut VecDeque<Vec<u8>>);

impl phy::RxToken for RxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        self.0.push_back(buf);
        r
    }
}

impl Device for Link {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps.checksum = ChecksumCapabilities::default();
        caps
    }

    fn receive(&mut self, _: SmolInstant) -> Option<(RxToken, TxToken<'_>)> {
        let frame = self.rx.pop_front()?;
        Some((RxToken(frame), TxToken(&mut self.tx)))
    }

    fn transmit(&mut self, _: SmolInstant) -> Option<TxToken<'_>> {
        Some(TxToken(&mut self.tx))
    }
}

/// What the stack keeps for an accepted TCP connection.
struct Conn {
    /// Bytes for the worker; dropping it ends the worker's input.
    to_worker: Option<Sender<Vec<u8>>>,
    queued: Arc<AtomicUsize>,
    /// Reply bytes the socket hasn't taken yet.
    out: VecDeque<Vec<u8>>,
    out_off: usize,
    worker_done: bool,
}

struct Inner {
    server: Option<Arc<Server>>,
    host: Host,
    events: SyncSender<Event>,
    stop: Arc<AtomicBool>,
    out: Box<dyn FnMut(Vec<u8>) + Send>,
    link: Link,
    iface: Interface,
    sockets: SocketSet<'static>,
    started: Instant,
    udp: HashMap<u16, SocketHandle>,
    /// Listeners and accepted connections, by port.
    tcp: Vec<(u16, SocketHandle)>,
    conns: HashMap<SocketHandle, Conn>,
    udp_calls: Sender<(u16, SocketAddr, IpEndpoint, Vec<u8>)>,
    workers: Vec<JoinHandle<()>>,
}

impl Inner {
    fn new(
        cfg: StackConfig,
        server: Option<Arc<Server>>,
        events: SyncSender<Event>,
        stop: Arc<AtomicBool>,
        out: Box<dyn FnMut(Vec<u8>) + Send>,
    ) -> Inner {
        let ports = origami_nfs::Ports {
            portmap: PORTS[0] as u32,
            mount: PORTS[1] as u32,
            nfs: PORTS[2] as u32,
        };
        if let Some(server) = &server {
            server.set_ports(ports, ports);
        }
        let served: Vec<u16> = if server.is_some() {
            PORTS.to_vec()
        } else {
            vec![]
        };
        let host = Host::new(
            cfg.mac,
            cfg.ip,
            cfg.prefix,
            served.clone(),
            cfg.addressing.clone(),
        );
        let mut link = Link::default();
        let mut icfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(cfg.mac)));
        icfg.random_seed = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let started = Instant::now();
        let mut iface = Interface::new(icfg, &mut link, SmolInstant::from_micros(0));
        iface.update_ip_addrs(|a| {
            let _ = a.push(IpCidr::new(IpAddress::Ipv4(cfg.ip), cfg.prefix));
        });
        let mut sockets = SocketSet::new(Vec::new());
        let mut udp_sockets = HashMap::new();
        for &port in &served {
            let mut s = udp::Socket::new(
                udp::PacketBuffer::new(
                    vec![udp::PacketMetadata::EMPTY; 32],
                    vec![0; SOCKET_BUFFER],
                ),
                udp::PacketBuffer::new(
                    vec![udp::PacketMetadata::EMPTY; 32],
                    vec![0; SOCKET_BUFFER],
                ),
            );
            s.bind(port).expect("bind UDP port");
            udp_sockets.insert(port, sockets.add(s));
        }

        // One thread answers UDP calls in arrival order, as a single
        // socket would.
        let (udp_calls, udp_rx) = mpsc::channel::<(u16, SocketAddr, IpEndpoint, Vec<u8>)>();
        let mut workers = Vec::new();
        if let Some(server) = server.clone() {
            let events = events.clone();
            workers.push(thread::spawn(move || {
                while let Ok((port, from, to, data)) = udp_rx.recv() {
                    if let Some(reply) = server.handle_datagram(from, &data) {
                        if events
                            .send(Event::UdpReply {
                                port,
                                to,
                                data: reply,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }));
        }
        let mut inner = Inner {
            server,
            host,
            events,
            stop,
            out,
            link,
            iface,
            sockets,
            started,
            udp: udp_sockets,
            tcp: Vec::new(),
            conns: HashMap::new(),
            udp_calls,
            workers,
        };
        if inner.server.is_some() {
            for port in PORTS {
                inner.top_up_listeners(port);
            }
        }
        inner
    }

    fn now(&self) -> SmolInstant {
        SmolInstant::from_micros(self.started.elapsed().as_micros() as i64)
    }

    fn top_up_listeners(&mut self, port: u16) {
        let idle = self
            .tcp
            .iter()
            .filter(|(p, h)| *p == port && self.sockets.get::<tcp::Socket>(*h).is_listening())
            .count();
        for _ in idle..LISTENERS {
            let mut s = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; SOCKET_BUFFER]),
                tcp::SocketBuffer::new(vec![0; SOCKET_BUFFER]),
            );
            s.set_nagle_enabled(false);
            s.set_ack_delay(Some(smoltcp::time::Duration::from_millis(1)));
            s.listen(port).expect("listen on TCP port");
            let h = self.sockets.add(s);
            self.tcp.push((port, h));
        }
    }

    fn run(mut self, rx: Receiver<Event>) {
        loop {
            let wait = self
                .iface
                .poll_delay(self.now(), &self.sockets)
                .map_or(MAX_POLL, |d| Duration::from_micros(d.total_micros()))
                .min(MAX_POLL);
            let first = match rx.recv_timeout(wait) {
                Ok(e) => Some(e),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let mut next = first;
            let mut budget = 256;
            while let Some(ev) = next {
                if !self.handle(ev) {
                    self.shutdown();
                    return;
                }
                budget -= 1;
                next = if budget > 0 { rx.try_recv().ok() } else { None };
            }
            self.pump();
        }
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Closing every channel lets each worker's read end.
        self.conns.clear();
        let (dead, _) = mpsc::channel();
        self.udp_calls = dead;
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }

    fn handle(&mut self, ev: Event) -> bool {
        match ev {
            Event::Frame(f) => match self.host.process(&f) {
                Verdict::Pass => self.link.rx.push_back(f),
                Verdict::Done(frames) => {
                    for frame in frames {
                        (self.out)(frame);
                    }
                }
            },
            Event::UdpReply { port, to, data } => {
                if let Some(h) = self.udp.get(&port) {
                    // A full buffer drops the reply; the client retries.
                    let _ = self
                        .sockets
                        .get_mut::<udp::Socket>(*h)
                        .send_slice(&data, to);
                }
            }
            Event::TcpData { handle, data } => {
                if let Some(c) = self.conns.get_mut(&handle) {
                    c.out.push_back(data);
                }
            }
            Event::TcpDone { handle } => {
                if let Some(c) = self.conns.get_mut(&handle) {
                    c.worker_done = true;
                }
            }
            Event::Stop => return false,
        }
        true
    }

    /// Runs smoltcp and our sockets until neither has more to do.
    fn pump(&mut self) {
        for _ in 0..64 {
            let now = self.now();
            let polled = self.iface.poll(now, &mut self.link, &mut self.sockets);
            let moved = self.service();
            while let Some(mut f) = self.link.tx.pop_front() {
                // The Ethernet minimum, as the host's own frames are.
                if f.len() < 60 {
                    f.resize(60, 0);
                }
                (self.out)(f);
            }
            if polled == PollResult::None && !moved {
                break;
            }
        }
    }

    /// Moves data between sockets and workers. Returns whether anything
    /// moved, so the caller polls again.
    fn service(&mut self) -> bool {
        let mut moved = false;
        for port in PORTS {
            let Some(&h) = self.udp.get(&port) else {
                break;
            };
            let s = self.sockets.get_mut::<udp::Socket>(h);
            while let Ok((data, meta)) = s.recv() {
                let ep = meta.endpoint;
                let IpAddress::Ipv4(a) = ep.addr;
                let from = SocketAddr::new(IpAddr::V4(a), ep.port);
                let _ = self.udp_calls.send((port, from, ep, data.to_vec()));
                moved = true;
            }
        }
        let handles: Vec<(u16, SocketHandle)> = self.tcp.clone();
        let mut gone = Vec::new();
        for (port, h) in handles {
            let s = self.sockets.get_mut::<tcp::Socket>(h);
            if !self.conns.contains_key(&h) {
                if s.is_listening() {
                    continue;
                }
                if s.state() == tcp::State::Closed {
                    gone.push((port, h));
                    continue;
                }
                if !s.may_recv() && !s.may_send() {
                    continue; // still handshaking, or already closing
                }
                let ep = s.remote_endpoint();
                let Some(ep) = ep else { continue };
                let IpAddress::Ipv4(a) = ep.addr;
                let peer = SocketAddr::new(IpAddr::V4(a), ep.port);
                if !self.server.as_ref().is_some_and(|s| s.admits(peer)) {
                    s.abort();
                    continue;
                }
                self.accept(h, peer);
                moved = true;
            }
            let Some(c) = self.conns.get_mut(&h) else {
                continue;
            };
            let s = self.sockets.get_mut::<tcp::Socket>(h);
            // Socket to worker.
            if let Some(tx) = &c.to_worker {
                if s.can_recv() && c.queued.load(Ordering::Relaxed) < WORKER_QUEUE {
                    let mut buf = Vec::new();
                    let _ = s.recv(|b| {
                        buf.extend_from_slice(b);
                        (b.len(), ())
                    });
                    if !buf.is_empty() {
                        c.queued.fetch_add(buf.len(), Ordering::Relaxed);
                        let _ = tx.send(buf);
                        moved = true;
                    }
                } else if !s.may_recv() && !s.can_recv() {
                    c.to_worker = None; // the peer closed its end
                    moved = true;
                }
            }
            // Worker to socket.
            while s.can_send() {
                let Some(front) = c.out.front() else { break };
                let n = s.send_slice(&front[c.out_off..]).unwrap_or(0);
                if n == 0 {
                    break;
                }
                moved = true;
                c.out_off += n;
                if c.out_off == front.len() {
                    c.out.pop_front();
                    c.out_off = 0;
                }
            }
            if c.worker_done && c.out.is_empty() && s.may_send() {
                s.close();
                moved = true;
            }
            if s.state() == tcp::State::Closed {
                gone.push((port, h));
            }
        }
        for (port, h) in gone {
            self.conns.remove(&h);
            self.tcp.retain(|(_, x)| *x != h);
            self.sockets.remove(h);
            self.top_up_listeners(port);
            moved = true;
        }
        if self.server.is_some() {
            for port in PORTS {
                self.top_up_listeners(port);
            }
        }
        moved
    }

    fn accept(&mut self, handle: SocketHandle, peer: SocketAddr) {
        let (to_worker, from_stack) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let mut stream = ConnStream {
            handle,
            input: from_stack,
            pending: Vec::new(),
            pos: 0,
            queued: queued.clone(),
            events: self.events.clone(),
        };
        let Some(server) = self.server.clone() else {
            return;
        };
        let (stop, events) = (self.stop.clone(), self.events.clone());
        self.workers.retain(|w| !w.is_finished());
        self.workers.push(thread::spawn(move || {
            let _ = server.serve_stream(peer, &mut stream, Some(&stop));
            let _ = events.send(Event::TcpDone { handle });
        }));
        self.conns.insert(
            handle,
            Conn {
                to_worker: Some(to_worker),
                queued,
                out: VecDeque::new(),
                out_off: 0,
                worker_done: false,
            },
        );
        // Replenish the listener this connection used.
        let port = self.tcp.iter().find(|(_, h)| *h == handle).unwrap().0;
        self.top_up_listeners(port);
    }
}

/// A worker's end of a TCP connection.
struct ConnStream {
    handle: SocketHandle,
    input: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    pos: usize,
    queued: Arc<AtomicUsize>,
    events: SyncSender<Event>,
}

impl Read for ConnStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.pending.len() {
            match self.input.recv_timeout(Duration::from_millis(100)) {
                Ok(b) => {
                    self.queued.fetch_sub(b.len(), Ordering::Relaxed);
                    self.pending = b;
                    self.pos = 0;
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(io::Error::from(io::ErrorKind::TimedOut))
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len() - self.pos);
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for ConnStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.events
            .send(Event::TcpData {
                handle: self.handle,
                data: buf.to_vec(),
            })
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
