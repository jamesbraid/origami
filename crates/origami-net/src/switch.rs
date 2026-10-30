//! The switch: two listening sockets for QEMU's stream netdevs and the
//! rule that moves frames between them and the stack.

use std::io::{self, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use origami_nfs::Server;

use crate::frame::{frames, push_frame, route, Framer, Route};
use crate::stack::{Stack, StackConfig};
use crate::transport::{self, Bind, Conn, Endpoint, Listener};

/// Configures [`Switch::start`].
#[derive(Clone, Debug)]
pub struct SwitchConfig {
    pub stack: StackConfig,
    /// Where QEMU's guest NIC (A) connects. Port 0 takes any free port.
    pub guest: SocketAddr,
    /// Where QEMU's hub port on the machine's network (B) connects.
    pub network: SocketAddr,
    /// Listen on Unix sockets made in this directory instead of on
    /// loopback ports, for any side whose address is left at port 0. The
    /// switch removes the directory when it stops.
    pub socket_dir: Option<PathBuf>,
}

impl SwitchConfig {
    pub fn new(stack: StackConfig) -> SwitchConfig {
        let any = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        SwitchConfig {
            stack,
            guest: any,
            network: any,
            socket_dir: None,
        }
    }

    fn bind(&self, addr: SocketAddr, name: &str) -> Bind {
        match &self.socket_dir {
            Some(dir) if addr.port() == 0 => Bind::Unix(dir.join(name)),
            _ => Bind::Tcp(addr),
        }
    }
}

/// One QEMU connection, replaced when QEMU reconnects.
#[derive(Default)]
struct Slot(Mutex<Option<Conn>>);

impl Slot {
    /// Sends frames already carrying their lengths, in one write.
    fn send_framed(&self, framed: &[u8]) {
        if framed.is_empty() {
            return;
        }
        let mut g = self.0.lock().unwrap();
        if let Some(s) = g.as_mut() {
            if s.write_all(framed).is_err() {
                // The peer went away; its reader notices and clears the
                // slot. Frames sent meanwhile are lost, as on a dead link.
                let _ = s.shutdown(Shutdown::Both);
                *g = None;
            }
        }
    }

    fn replace(&self, s: Conn) {
        if let Some(old) = self.0.lock().unwrap().replace(s) {
            let _ = old.shutdown(Shutdown::Both);
        }
    }

    fn close(&self) {
        if let Some(s) = self.0.lock().unwrap().take() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

struct Shared {
    stack: Mutex<Option<Stack>>,
    guest: Slot,
    network: Slot,
    cfg: StackConfig,
    /// The stack's address and its aliases.
    ips: Vec<Ipv4Addr>,
}

/// A running switch and stack. Dropping it stops both.
pub struct Switch {
    shared: Arc<Shared>,
    guest_endpoint: Endpoint,
    network_endpoint: Endpoint,
    socket_dir: Option<PathBuf>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Switch {
    pub fn start(cfg: SwitchConfig, server: Option<Arc<Server>>) -> io::Result<Switch> {
        let guest_l = Listener::bind(&cfg.bind(cfg.guest, "guest.sock"))?;
        let network_l = Listener::bind(&cfg.bind(cfg.network, "net.sock"))?;
        let guest_endpoint = guest_l.endpoint()?;
        let network_endpoint = network_l.endpoint()?;
        let shared = Arc::new(Shared {
            stack: Mutex::new(None),
            guest: Slot::default(),
            network: Slot::default(),
            cfg: cfg.stack.clone(),
            ips: std::iter::once(cfg.stack.ip)
                .chain(cfg.stack.aliases.iter().copied())
                .collect(),
        });
        let out = shared.clone();
        let stack = Stack::start(cfg.stack, server, move |f| {
            let mut framed = Vec::with_capacity(4 + f.len());
            push_frame(&mut framed, &f);
            out.guest.send_framed(&framed)
        });
        *shared.stack.lock().unwrap() = Some(stack);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let threads = vec![
            spawn_accept(shared.clone(), guest_l, true, stop.clone()),
            spawn_accept(shared.clone(), network_l, false, stop.clone()),
        ];
        Ok(Switch {
            shared,
            guest_endpoint,
            network_endpoint,
            socket_dir: cfg.socket_dir,
            stop,
            threads,
        })
    }

    /// Where the guest NIC's stream netdev connects, for a switch
    /// listening on loopback TCP.
    ///
    /// # Panics
    ///
    /// If the switch listens on a Unix socket; see
    /// [`Switch::guest_endpoint`].
    pub fn guest_addr(&self) -> SocketAddr {
        match &self.guest_endpoint {
            Endpoint::Tcp(a) => *a,
            Endpoint::Unix(_) => panic!("the guest side listens on a Unix socket"),
        }
    }

    /// Where the machine's network joins, for a switch listening on
    /// loopback TCP.
    ///
    /// # Panics
    ///
    /// If the switch listens on a Unix socket; see
    /// [`Switch::network_endpoint`].
    pub fn network_addr(&self) -> SocketAddr {
        match &self.network_endpoint {
            Endpoint::Tcp(a) => *a,
            Endpoint::Unix(_) => panic!("the network side listens on a Unix socket"),
        }
    }

    /// Where the guest NIC's stream netdev connects.
    pub fn guest_endpoint(&self) -> &Endpoint {
        &self.guest_endpoint
    }

    /// Where the machine's network joins.
    pub fn network_endpoint(&self) -> &Endpoint {
        &self.network_endpoint
    }

    pub fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.shared.guest.close();
        self.shared.network.close();
        // Wake the accept loops.
        self.guest_endpoint.poke();
        self.network_endpoint.poke();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        self.shared.stack.lock().unwrap().take();
        if let Some(dir) = self.socket_dir.take() {
            transport::remove_dir(&dir);
        }
    }
}

impl Drop for Switch {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_accept(
    shared: Arc<Shared>,
    listener: Listener,
    guest: bool,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut readers: Vec<JoinHandle<()>> = Vec::new();
        loop {
            let conn = listener.accept();
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let Ok(conn) = conn else { continue };
            let Ok(mut read_half) = conn.try_clone() else {
                continue;
            };
            let slot = if guest {
                &shared.guest
            } else {
                &shared.network
            };
            slot.replace(conn);
            let shared = shared.clone();
            readers.retain(|r| !r.is_finished());
            readers.push(thread::spawn(move || {
                let mut framer = Framer::new();
                let mut out = Vec::new();
                // Each read takes whatever QEMU has sent, many frames
                // during a transfer, and goes on with one write.
                while let Ok(n) = framer.fill(&mut read_half) {
                    if n == 0 {
                        break;
                    }
                    let Ok(whole) = framer.whole() else { break };
                    if guest {
                        from_guest(&shared, whole, &mut out);
                    } else {
                        shared.guest.send_framed(whole);
                    }
                    let len = whole.len();
                    framer.consume(len);
                }
            }));
        }
        for r in readers {
            let _ = r.join();
        }
    })
}

/// Sends on what the guest wrote: `whole` is whole frames with their
/// lengths, `out` a buffer to reuse. A frame for the network is copied once,
/// into the buffer; only one for the stack is allocated.
fn from_guest(shared: &Shared, whole: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let mut to_stack = Vec::new();
    for frame in frames(whole) {
        let r = route(frame, shared.cfg.mac, &shared.ips);
        if matches!(r, Route::Network | Route::Both) {
            push_frame(out, frame);
        }
        if matches!(r, Route::Stack | Route::Both) {
            to_stack.push(frame.to_vec());
        }
    }
    shared.network.send_framed(out);
    if !to_stack.is_empty() {
        if let Some(s) = shared.stack.lock().unwrap().as_ref() {
            for frame in to_stack {
                s.receive(frame);
            }
        }
    }
}
