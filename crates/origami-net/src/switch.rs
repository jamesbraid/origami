//! The switch: two listening sockets for QEMU's stream netdevs and the
//! rule that moves frames between them and the stack.

use std::io::{self, BufReader, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use origami_nfs::Server;

use crate::frame::{push_frame, read_frames, route, Route};
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
                while let Ok(Some(frames)) = read_frames(&mut r) {
                    if guest {
                        from_guest(&shared, frames);
                    } else {
                        let mut framed = Vec::new();
                        for f in &frames {
                            push_frame(&mut framed, f);
                        }
                        shared.guest.send_framed(&framed);
                    }
                }
            }));
        }
        for r in readers {
            let _ = r.join();
        }
    })
}

fn from_guest(shared: &Shared, frames: Vec<Vec<u8>>) {
    let mut to_network = Vec::new();
    let mut to_stack = Vec::new();
    for frame in frames {
        let r = route(&frame, shared.cfg.mac, shared.cfg.ip);
        if matches!(r, Route::Network | Route::Both) {
            push_frame(&mut to_network, &frame);
        }
        if matches!(r, Route::Stack | Route::Both) {
            to_stack.push(frame);
        }
    }
    shared.network.send_framed(&to_network);
    if let Some(s) = shared.stack.lock().unwrap().as_ref() {
        for frame in to_stack {
            s.receive(frame);
        }
    }
}
