//! The switch: two listening sockets for QEMU's stream netdevs and the
//! rule that moves frames between them and the stack.

use std::io::{self, BufReader};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use origami_nfs::Server;

use crate::frame::{read_frame, route, write_frame, Route};
use crate::stack::{Stack, StackConfig};

/// Configures [`Switch::start`].
#[derive(Clone, Copy, Debug)]
pub struct SwitchConfig {
    pub stack: StackConfig,
    /// Where QEMU's guest NIC (A) connects. Port 0 takes any free port.
    pub guest: SocketAddr,
    /// Where QEMU's hub port on the machine's network (B) connects.
    pub network: SocketAddr,
}

impl SwitchConfig {
    pub fn new(stack: StackConfig) -> SwitchConfig {
        let any = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        SwitchConfig {
            stack,
            guest: any,
            network: any,
        }
    }
}

/// One QEMU connection, replaced when QEMU reconnects.
#[derive(Default)]
struct Slot(Mutex<Option<TcpStream>>);

impl Slot {
    fn send(&self, frame: &[u8]) {
        let mut g = self.0.lock().unwrap();
        if let Some(s) = g.as_mut() {
            if write_frame(s, frame).is_err() {
                // The peer went away; its reader notices and clears the
                // slot. Frames sent meanwhile are lost, as on a dead link.
                let _ = s.shutdown(Shutdown::Both);
                *g = None;
            }
        }
    }

    fn replace(&self, s: TcpStream) {
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
}

/// A running switch and stack. Dropping it stops both.
pub struct Switch {
    shared: Arc<Shared>,
    guest_addr: SocketAddr,
    network_addr: SocketAddr,
    stop: Arc<std::sync::atomic::AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Switch {
    pub fn start(cfg: SwitchConfig, server: Arc<Server>) -> io::Result<Switch> {
        let guest_l = TcpListener::bind(cfg.guest)?;
        let network_l = TcpListener::bind(cfg.network)?;
        let guest_addr = guest_l.local_addr()?;
        let network_addr = network_l.local_addr()?;
        let shared = Arc::new(Shared {
            stack: Mutex::new(None),
            guest: Slot::default(),
            network: Slot::default(),
            cfg: cfg.stack,
        });
        let out = shared.clone();
        let stack = Stack::start(cfg.stack, server, move |f| out.guest.send(&f));
        *shared.stack.lock().unwrap() = Some(stack);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let threads = vec![
            spawn_accept(shared.clone(), guest_l, true, stop.clone()),
            spawn_accept(shared.clone(), network_l, false, stop.clone()),
        ];
        Ok(Switch {
            shared,
            guest_addr,
            network_addr,
            stop,
            threads,
        })
    }

    /// Where the guest NIC's stream netdev connects.
    pub fn guest_addr(&self) -> SocketAddr {
        self.guest_addr
    }

    /// Where the machine's network joins.
    pub fn network_addr(&self) -> SocketAddr {
        self.network_addr
    }

    pub fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.shared.guest.close();
        self.shared.network.close();
        // Wake the accept loops.
        let _ = TcpStream::connect(self.guest_addr);
        let _ = TcpStream::connect(self.network_addr);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        self.shared.stack.lock().unwrap().take();
    }
}

impl Drop for Switch {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_accept(
    shared: Arc<Shared>,
    listener: TcpListener,
    guest: bool,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut readers: Vec<JoinHandle<()>> = Vec::new();
        for conn in listener.incoming() {
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let Ok(conn) = conn else { continue };
            let _ = conn.set_nodelay(true);
            let Ok(read_half) = conn.try_clone() else {
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
                let mut r = BufReader::with_capacity(1 << 16, read_half);
                while let Ok(Some(frame)) = read_frame(&mut r) {
                    if guest {
                        from_guest(&shared, frame);
                    } else {
                        shared.guest.send(&frame);
                    }
                }
            }));
        }
        for r in readers {
            let _ = r.join();
        }
    })
}

fn from_guest(shared: &Shared, frame: Vec<u8>) {
    let r = route(&frame, shared.cfg.mac, shared.cfg.ip);
    if matches!(r, Route::Network | Route::Both) {
        shared.network.send(&frame);
    }
    if matches!(r, Route::Stack | Route::Both) {
        if let Some(s) = shared.stack.lock().unwrap().as_ref() {
            s.receive(frame);
        }
    }
}
