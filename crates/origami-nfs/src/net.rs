//! Host sockets for the server: a thin adapter over the transport-agnostic
//! core in [`Server`], and [`ShareServer`], which serves one host
//! directory on them.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::rpc::{LogLevel, Logger, Ports, Server};
use crate::sharefs::{Options, Share};

/// How often a blocked thread looks at the stop flag.
const POLL: Duration = Duration::from_millis(100);

/// Send and receive buffer for a UDP socket. The server's largest reply is
/// a 32 KiB READ plus headers, so a datagram needs well over 32 KiB of
/// buffer.
const UDP_BUFFER: usize = 256 * 1024;

/// Makes `sock` big enough for the largest datagram the server sends, 64 KiB
/// at most. Hosts refuse a datagram larger than the send buffer: macOS
/// defaults it to `net.inet.udp.maxdgram`, 9216 bytes, and answers
/// `EMSGSIZE`, so without this a READ or READDIR reply over 9 KiB is never
/// sent. Linux and Windows defaults already suffice. A host may clamp the
/// size (Linux to `net.core.wmem_max`); the limit that matters stays 64 KiB.
pub fn raise_udp_buffers(sock: &UdpSocket) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
            let size = UDP_BUFFER as libc::c_int;
            // SAFETY: the descriptor is open for the call and the option
            // value is a c_int of the stated length.
            let rc = unsafe {
                libc::setsockopt(
                    sock.as_raw_fd(),
                    libc::SOL_SOCKET,
                    opt,
                    &size as *const libc::c_int as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = sock;
    Ok(())
}

/// A [`Server`] answering on host sockets, until stopped.
pub struct Serving {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Serving {
    /// Starts a thread for each socket given. UDP calls are answered one
    /// at a time; each TCP connection has a thread of its own.
    pub fn start(
        server: Arc<Server>,
        udp: Option<UdpSocket>,
        tcp: Option<TcpListener>,
    ) -> io::Result<Serving> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        if let Some(sock) = udp {
            sock.set_read_timeout(Some(POLL))?;
            if let Err(e) = raise_udp_buffers(&sock) {
                server.log(LogLevel::Warn, || {
                    format!("nfs: raising the UDP buffers: {e}; large replies may be dropped")
                });
            }
            let (server, stop) = (server.clone(), stop.clone());
            threads.push(thread::spawn(move || serve_udp(&server, &sock, &stop)));
        }
        if let Some(l) = tcp {
            l.set_nonblocking(true)?;
            let (server, stop) = (server.clone(), stop.clone());
            threads.push(thread::spawn(move || serve_tcp(server, l, stop)));
        }
        Ok(Serving { stop, threads })
    }

    /// Stops answering and waits for every thread, closing the sockets.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve_udp(server: &Server, sock: &UdpSocket, stop: &AtomicBool) {
    let mut buf = vec![0u8; 65536];
    while !stop.load(Ordering::Relaxed) {
        let (n, from) = match sock.recv_from(&mut buf) {
            Ok(r) => r,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // An ICMP port-unreachable from an earlier reply surfaces as
            // an error on some hosts; it isn't the socket failing.
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => continue,
            Err(_) => return,
        };
        if let Some(reply) = server.handle_datagram(from, &buf[..n]) {
            if let Err(e) = sock.send_to(&reply, from) {
                server.log(LogLevel::Warn, || {
                    format!("nfs: {from}: dropped a {}-byte reply: {e}", reply.len())
                });
            }
        }
    }
}

fn serve_tcp(server: Arc<Server>, l: TcpListener, stop: Arc<AtomicBool>) {
    let mut conns: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        conns.retain(|c| !c.is_finished());
        match l.accept() {
            Ok((stream, peer)) => {
                if !server.admits(peer) {
                    continue;
                }
                let (server, stop) = (server.clone(), stop.clone());
                conns.push(thread::spawn(move || {
                    serve_conn(&server, stream, peer, &stop)
                }));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    for c in conns {
        let _ = c.join();
    }
}

fn serve_conn(server: &Server, mut stream: TcpStream, peer: SocketAddr, stop: &AtomicBool) {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(POLL)).is_err()
        || stream.set_nodelay(true).is_err()
    {
        return;
    }
    let _ = server.serve_stream(peer, &mut stream, Some(stop));
}

/// Configures [`ShareServer::start`].
pub struct ShareConfig {
    /// The share's name; guests mount it as `/name`.
    pub name: String,
    /// The host directory to export.
    pub dir: PathBuf,
    /// Where the path table lives. Required on Windows, which has no
    /// inode numbers; elsewhere setting it chooses handles from paths
    /// instead of inodes.
    pub path_table: Option<PathBuf>,
    /// The address to listen on.
    pub bind: IpAddr,
    /// The port to listen on, for both transports, such as 2049 for NFS.
    /// When it can't be bound, because it is privileged or taken, the
    /// server takes any free port instead and [`BoundPorts::fell_back`]
    /// says so. None takes any free port.
    pub preferred_port: Option<u16>,
    pub udp: bool,
    pub tcp: bool,
    /// Admits only clients this accepts. None admits everyone.
    pub allow: Option<Arc<dyn Fn(IpAddr) -> bool + Send + Sync>>,
    pub logger: Option<Logger>,
}

impl ShareConfig {
    /// A share on loopback, any free port, over UDP and TCP.
    pub fn new(name: impl Into<String>, dir: impl Into<PathBuf>) -> ShareConfig {
        ShareConfig {
            name: name.into(),
            dir: dir.into(),
            path_table: None,
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            preferred_port: None,
            udp: true,
            tcp: true,
            allow: None,
            logger: None,
        }
    }
}

/// The addresses a [`ShareServer`] listens on.
#[derive(Clone, Copy, Debug)]
pub struct BoundPorts {
    pub udp: Option<SocketAddr>,
    pub tcp: Option<SocketAddr>,
    /// The preferred port couldn't be bound.
    pub fell_back: bool,
}

/// A running server for one share. Dropping it stops the server.
pub struct ShareServer {
    ports: BoundPorts,
    serving: Serving,
}

impl ShareServer {
    /// Opens the share and starts serving it. Portmap, MOUNT and NFS all
    /// answer on the one port of each transport.
    pub fn start(cfg: ShareConfig) -> io::Result<ShareServer> {
        if !cfg.udp && !cfg.tcp {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a share needs UDP or TCP",
            ));
        }
        let share = Share::open(
            &cfg.name,
            &cfg.dir,
            Options {
                path_table: cfg.path_table,
                attr_filter: None,
            },
        )?;
        let (udp, tcp, fell_back) = bind(cfg.bind, cfg.preferred_port, cfg.udp, cfg.tcp)?;
        let ports = BoundPorts {
            udp: udp.as_ref().map(|s| s.local_addr()).transpose()?,
            tcp: tcp.as_ref().map(|s| s.local_addr()).transpose()?,
            fell_back,
        };
        let mut server = Server::new(Arc::new(share));
        if let Some(a) = cfg.allow {
            server = server.with_allow(move |ip| a(ip));
        }
        if let Some(l) = cfg.logger {
            server = server.with_logger(l);
        }
        let of = |a: Option<SocketAddr>| {
            let p = a.map_or(0, |a| a.port() as u32);
            Ports {
                portmap: p,
                mount: p,
                nfs: p,
            }
        };
        server.set_ports(of(ports.udp), of(ports.tcp));
        let serving = Serving::start(Arc::new(server), udp, tcp)?;
        Ok(ShareServer { ports, serving })
    }

    /// Where the server listens.
    pub fn ports(&self) -> BoundPorts {
        self.ports
    }

    /// Stops the server and waits for it.
    pub fn stop(mut self) {
        self.serving.stop();
    }
}

/// Binds the sockets, on the preferred port or else any.
fn bind(
    ip: IpAddr,
    preferred: Option<u16>,
    want_udp: bool,
    want_tcp: bool,
) -> io::Result<(Option<UdpSocket>, Option<TcpListener>, bool)> {
    let attempt = |port: u16| -> io::Result<(Option<UdpSocket>, Option<TcpListener>)> {
        let udp = if want_udp {
            Some(UdpSocket::bind((ip, port))?)
        } else {
            None
        };
        let tcp = if want_tcp {
            Some(TcpListener::bind((ip, port))?)
        } else {
            None
        };
        Ok((udp, tcp))
    };
    if let Some(p) = preferred {
        if let Ok((u, t)) = attempt(p) {
            return Ok((u, t, false));
        }
    }
    let (u, t) = attempt(0)?;
    Ok((u, t, preferred.is_some()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    fn buffer(sock: &UdpSocket, opt: libc::c_int) -> libc::c_int {
        let mut v: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: v and len are valid for the call and len is v's size.
        let rc = unsafe {
            libc::getsockopt(
                sock.as_raw_fd(),
                libc::SOL_SOCKET,
                opt,
                &mut v as *mut libc::c_int as *mut libc::c_void,
                &mut len,
            )
        };
        assert_eq!(rc, 0, "{}", io::Error::last_os_error());
        v
    }

    fn set(sock: &UdpSocket, opt: libc::c_int, v: libc::c_int) {
        // SAFETY: as in raise_udp_buffers.
        let rc = unsafe {
            libc::setsockopt(
                sock.as_raw_fd(),
                libc::SOL_SOCKET,
                opt,
                &v as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        assert_eq!(rc, 0, "{}", io::Error::last_os_error());
    }

    /// A socket with macOS's 9216-byte default send buffer refuses the
    /// 32 KiB reply a READ makes; after raising it the datagram goes out.
    #[test]
    fn raised_buffers_carry_a_32k_datagram() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        raise_udp_buffers(&rx).unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        set(&tx, libc::SO_SNDBUF, 4096);
        assert!(buffer(&tx, libc::SO_SNDBUF) < 32768 + 512);
        raise_udp_buffers(&tx).unwrap();
        assert!(buffer(&tx, libc::SO_SNDBUF) >= 32768 + 512);
        assert!(buffer(&tx, libc::SO_RCVBUF) >= 32768 + 512);
        let reply = vec![7u8; 32768 + 128];
        tx.send_to(&reply, rx.local_addr().unwrap()).unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = vec![0u8; 65536];
        let (n, _) = rx.recv_from(&mut buf).unwrap();
        assert_eq!(n, reply.len());
    }
}
