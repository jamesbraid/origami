//! The connection to QEMU's display socket: say HELLO, receive the control
//! block and the regions as passed handles, keep the [`Display`] current from
//! `REGION`, `REGION_GONE` and `WAKE`, send input on the same socket, and
//! connect again when the connection drops while the machine is alive.

use crate::control::Control;
use crate::display::Display;
use crate::proto::*;
use crate::region::Handle;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Where input goes: the write half of the current connection, if there is
/// one. The transport fills it after HELLO and empties it when the connection
/// ends.
pub type Uplink = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

/// What a socket gives the reader: bytes, and the handles that arrived with
/// them (`SCM_RIGHTS` on Linux and macOS; Windows passes values in the
/// messages instead).
pub trait MsgSource {
    /// Read some bytes into `buf`; any handles that came with them are pushed
    /// on `handles`. A read timeout is `WouldBlock` or `TimedOut`; zero is the
    /// end of the stream.
    fn read_some(&mut self, buf: &mut [u8], handles: &mut Vec<Handle>) -> io::Result<usize>;
}

pub struct Conn {
    pub source: Box<dyn MsgSource + Send>,
    pub sink: Box<dyn Write + Send>,
}

/// How the thread reaches the machine. The real one connects to the Unix
/// socket; the tests supply scripted connections.
pub trait Link: Send {
    fn connect(&mut self) -> io::Result<Conn>;
    /// Whether the machine still exists.
    fn alive(&mut self) -> bool;
}

#[derive(Clone, Copy)]
pub struct Timing {
    /// How long to wait for the display to appear the first time.
    pub first_connect: Duration,
    /// Between attempts to reach a display that is not there.
    pub retry: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            first_connect: Duration::from_secs(300),
            retry: Duration::from_millis(200),
        }
    }
}

/// Why `run` returned.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    Stopped,
    /// The machine is gone.
    Gone,
    /// The display never appeared.
    NeverCame,
}

fn bad(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

/// Fill `buf` from `source`, collecting handles. None when `stop` was set.
fn read_full(
    source: &mut dyn MsgSource,
    buf: &mut [u8],
    handles: &mut Vec<Handle>,
    stop: &AtomicBool,
) -> io::Result<Option<()>> {
    let mut filled = 0;
    while filled < buf.len() {
        if stop.load(Ordering::Acquire) {
            return Ok(None);
        }
        match source.read_some(&mut buf[filled..], handles) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => filled += n,
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(Some(()))
}

/// One message and the handles that came with it: the 8-byte header first
/// (a handle rides on a message's first byte), then the rest by its type's
/// fixed size. None when `stop` was set.
fn read_message(
    source: &mut dyn MsgSource,
    stop: &AtomicBool,
) -> io::Result<Option<(Vec<u8>, Vec<Handle>)>> {
    let mut handles = Vec::new();
    let mut msg = vec![0u8; HEADER_SIZE];
    if read_full(source, &mut msg, &mut handles, stop)?.is_none() {
        return Ok(None);
    }
    let kind = message_type(&msg);
    let size = incoming_size(kind).ok_or_else(|| bad(format!("unknown message type {kind:#x}")))?;
    msg.resize(size, 0);
    if read_full(source, &mut msg[HEADER_SIZE..], &mut handles, stop)?.is_none() {
        return Ok(None);
    }
    Ok(Some((msg, handles)))
}

/// The handle value a WELCOME or REGION carries, owned from the moment the
/// message is parsed. On Windows the value is a handle QEMU duplicated into
/// this process, so a message that is then rejected must close it: dropping
/// this does, unless [`take_handle`] took it first. On Linux and macOS the
/// descriptor rides in the `handles` list, which closes on drop, and this
/// holds nothing.
struct Passed {
    #[cfg(windows)]
    value: u64,
}

impl Passed {
    #[cfg_attr(unix, allow(unused_variables))]
    fn new(value: u64) -> Self {
        Passed {
            #[cfg(windows)]
            value,
        }
    }
}

#[cfg(windows)]
impl Drop for Passed {
    fn drop(&mut self) {
        if self.value != 0 {
            // SAFETY: a handle QEMU duplicated into this process for us to
            // close, not yet handed to anything that owns it.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.value as usize as _) };
        }
    }
}

/// The handle for a control block or region: the one that was attached
/// (Linux, macOS) or the value in the message (Windows).
#[cfg_attr(windows, allow(unused_variables))]
fn take_handle(passed: &mut Passed, handles: &mut Vec<Handle>) -> io::Result<Handle> {
    #[cfg(unix)]
    {
        let _ = passed;
        if handles.is_empty() {
            Err(bad("a message that should carry a handle carried none"))
        } else {
            Ok(handles.remove(0))
        }
    }
    #[cfg(windows)]
    {
        let _ = handles;
        match std::mem::take(&mut passed.value) {
            0 => Err(bad("a message that should carry a handle carried none")),
            value => Ok(Handle::Value(value)),
        }
    }
}

/// Handle messages until the stream ends or `stop` is set. `wake` is called
/// when something the UI should look at changed.
pub fn pump(
    display: &Display,
    source: &mut dyn MsgSource,
    stop: &AtomicBool,
    wake: &dyn Fn(),
) -> io::Result<()> {
    let mut welcomed = false;
    loop {
        let Some((msg, mut handles)) = read_message(source, stop)? else {
            return Ok(());
        };
        display.count_message();
        match message_type(&msg) {
            MSG_WELCOME => {
                if welcomed {
                    return Err(bad("a second WELCOME"));
                }
                let w = parse_welcome(&msg);
                let mut passed = Passed::new(w.control_handle);
                if w.version != VERSION {
                    return Err(bad(format!("QEMU speaks version {}", w.version)));
                }
                let shm = |b: u32| b == BACKEND_SHM_FD || b == BACKEND_SHM_WIN32;
                let gpu = |b: u32| b == BACKEND_VK_FD || b == BACKEND_VK_WIN32;
                if !shm(w.control_backend) || !(shm(w.backend) || gpu(w.backend)) {
                    return Err(bad(format!(
                        "QEMU says backend {} (control {}), which this viewer does not know",
                        w.backend, w.control_backend
                    )));
                }
                if gpu(w.backend) {
                    // Memory exported on another adapter or driver cannot be
                    // imported, and there is no shared-memory copy to fall
                    // back to: say so, rather than show a picture that is not.
                    let importer = display.importer().ok_or_else(|| {
                        bad(format!(
                            "QEMU exports GPU memory (backend {}) and this viewer cannot import it",
                            w.backend
                        ))
                    })?;
                    importer
                        .accepts(&w.device_uuid, &w.driver_uuid)
                        .map_err(|why| bad(format!("cannot import QEMU's GPU memory: {why}")))?;
                }
                let mem = take_handle(&mut passed, &mut handles)?.map()?;
                let control = Control::new(mem).map_err(|e| bad(e.to_string()))?;
                display.attach(control);
                welcomed = true;
                if display.visible() {
                    display.arm_all();
                }
                wake();
            }
            MSG_REGION if welcomed => {
                let r = parse_region(&msg);
                let mut passed = Passed::new(r.handle);
                if r.id == 0 || !r.size.is_multiple_of(REGION_ALIGN) {
                    return Err(bad(format!(
                        "region {} has {} bytes, not a multiple of {REGION_ALIGN}",
                        r.id, r.size
                    )));
                }
                match r.backend {
                    BACKEND_VK_FD | BACKEND_VK_WIN32 => {
                        let importer = display.importer().ok_or_else(|| {
                            bad(format!(
                                "region {} is GPU memory and this viewer cannot import it",
                                r.id
                            ))
                        })?;
                        if r.alloc_size < r.size {
                            return Err(bad(format!(
                                "region {} allocates {} bytes for {}",
                                r.id, r.alloc_size, r.size
                            )));
                        }
                        let handle = take_handle(&mut passed, &mut handles)?;
                        let memory = importer
                            .import(&r, handle)
                            .map_err(|e| bad(format!("cannot import region {}: {e}", r.id)))?;
                        display.add_gpu_region(r.id, r.size, None, memory);
                        display.note_backend(importer.name().to_string());
                    }
                    BACKEND_SHM_FD | BACKEND_SHM_WIN32 => {
                        let handle = take_handle(&mut passed, &mut handles)?;
                        let opened = display
                            .importer()
                            .filter(|_| r.kind != KIND_CURSOR_IMAGE)
                            .and_then(|i| i.open_section(&r, &handle).ok());
                        let mem = handle.map()?;
                        if (mem.len() as u64) < r.size {
                            return Err(bad(format!(
                                "region {} is {} bytes, said {}",
                                r.id,
                                mem.len(),
                                r.size
                            )));
                        }
                        // Where the API can read the mapping in place (Metal),
                        // do; else the reader copies the changed lines.
                        let wrapped = opened.or_else(|| {
                            display
                                .importer()
                                .filter(|_| r.kind != KIND_CURSOR_IMAGE)
                                .and_then(|i| i.wrap(&r, &mem))
                        });
                        match wrapped {
                            Some(gpu) => {
                                display.add_gpu_region(r.id, r.size, Some(mem), gpu);
                                if let Some(i) = display.importer() {
                                    display.note_backend(i.name().to_string());
                                }
                            }
                            None => {
                                display.add_region(r.id, mem);
                                if r.kind != KIND_CURSOR_IMAGE {
                                    display
                                        .note_backend("shared memory, changed lines copied".into());
                                }
                            }
                        }
                    }
                    other => {
                        return Err(bad(format!("region {} has backend {other}", r.id)));
                    }
                }
                wake();
            }
            MSG_REGION_GONE if welcomed => {
                display.remove_region(parse_id(&msg));
                wake();
            }
            MSG_WAKE if welcomed => {
                let head = parse_id(&msg) as usize;
                // Armed again for the next change, then look.
                if display.visible() {
                    display.arm(head);
                }
                wake();
            }
            MSG_INPUT_ACK => {}
            t => return Err(bad(format!("message {t:#x} before the WELCOME"))),
        }
        // Handles nobody took (extra ones) close here.
        drop(handles);
    }
}

/// Keep the display connected until `stop` or the machine's end.
pub fn run<L: Link>(
    link: &mut L,
    display: &Display,
    uplink: &Uplink,
    stop: &AtomicBool,
    timing: Timing,
    wake: &dyn Fn(),
) -> Exit {
    let started = Instant::now();
    let mut ever = false;
    let mut misses = 0u32;
    let mut last_error: Option<String> = None;
    while !stop.load(Ordering::Acquire) {
        let connected = match link.connect() {
            Ok(mut conn) => {
                display.link_up();
                let (caps, uuid) = display
                    .importer()
                    .map_or((0, [0; 16]), |i| (i.caps(), i.device_uuid()));
                let hello = hello(std::process::id(), caps, uuid);
                if conn
                    .sink
                    .write_all(&hello)
                    .and_then(|_| conn.sink.flush())
                    .is_ok()
                {
                    ever = true;
                    *lock(uplink) = Some(conn.sink);
                    if let Err(e) = pump(display, &mut *conn.source, stop, wake) {
                        // The same failure at every reconnect is said once.
                        let said = e.to_string();
                        if e.kind() != io::ErrorKind::UnexpectedEof {
                            display.link_failed(said.clone());
                        }
                        if e.kind() != io::ErrorKind::UnexpectedEof
                            && last_error.as_deref() != Some(&said)
                        {
                            eprintln!("origami-gui: display: {said}");
                        }
                        last_error = Some(said);
                    }
                    *lock(uplink) = None;
                    display.detach();
                    wake();
                    true
                } else {
                    false
                }
            }
            Err(e) => {
                display.link_connect_failed(e.to_string());
                false
            }
        };
        if stop.load(Ordering::Acquire) {
            break;
        }
        // Asking whether the machine lives takes its QMP port, which other
        // clients want too, so a display that is merely not up yet is asked
        // about only now and then.
        misses = if connected { 0 } else { misses + 1 };
        if (connected || misses % 10 == 1) && !link.alive() {
            return Exit::Gone;
        }
        if !connected {
            if !ever && started.elapsed() > timing.first_connect {
                return Exit::NeverCame;
            }
            std::thread::sleep(timing.retry);
        }
    }
    Exit::Stopped
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---- The real socket ------------------------------------------------------

/// Connect to the socket the product's runtime gave QEMU
/// (`display-shm-socket`).
pub fn connect_socket(path: &std::path::Path) -> io::Result<Conn> {
    sock::connect(path, Some(Duration::from_millis(200))).map(|(conn, _)| conn)
}

/// Ends a blocked read from another thread: the read then fails as if the
/// peer had closed.
pub type Interrupt = Box<dyn Fn() + Send + Sync>;

/// [`connect_socket`] with reads that block, so a still screen costs no
/// wake-ups. Set `stop`, then call the [`Interrupt`] to end the read.
pub fn connect_socket_blocking(path: &std::path::Path) -> io::Result<(Conn, Interrupt)> {
    sock::connect(path, None)
}

#[cfg(unix)]
mod sock {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;

    struct Source(UnixStream);

    impl MsgSource for Source {
        fn read_some(&mut self, buf: &mut [u8], handles: &mut Vec<Handle>) -> io::Result<usize> {
            let mut iov = libc::iovec {
                iov_base: buf.as_mut_ptr().cast(),
                iov_len: buf.len(),
            };
            // Room for several descriptors, aligned for cmsghdr.
            let mut control = [0u64; 16];
            // SAFETY: a zeroed msghdr is valid; the fields set below point at
            // buffers that outlive the call.
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = std::mem::size_of_val(&control) as _;
            // SAFETY: `msg` is fully set up for one iovec and a control buffer.
            // Linux marks the descriptors close-on-exec as they arrive, so a
            // child started on another thread cannot inherit one; macOS has
            // no such flag, and the fcntl below is the best it can do.
            #[cfg(target_os = "linux")]
            let flags = libc::MSG_CMSG_CLOEXEC;
            #[cfg(not(target_os = "linux"))]
            let flags = 0;
            let n = unsafe { libc::recvmsg(self.0.as_raw_fd(), &mut msg, flags) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: walking the control messages the kernel wrote into
            // `control`, which `msg` still describes.
            unsafe {
                let mut c = libc::CMSG_FIRSTHDR(&msg);
                while !c.is_null() {
                    if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                        let count = ((*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                            / std::mem::size_of::<libc::c_int>();
                        let data = libc::CMSG_DATA(c) as *const libc::c_int;
                        for i in 0..count {
                            let fd = std::ptr::read_unaligned(data.add(i));
                            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                            handles.push(Handle::Fd(OwnedFd::from_raw_fd(fd)));
                        }
                    }
                    c = libc::CMSG_NXTHDR(&msg, c);
                }
            }
            if msg.msg_flags & libc::MSG_CTRUNC != 0 {
                return Err(bad("a passed handle did not fit"));
            }
            Ok(n as usize)
        }
    }

    pub fn connect(
        path: &std::path::Path,
        timeout: Option<Duration>,
    ) -> io::Result<(Conn, Interrupt)> {
        let s = UnixStream::connect(path)?;
        s.set_read_timeout(timeout)?;
        let sink = s.try_clone()?;
        let closer = s.try_clone()?;
        Ok((
            Conn {
                source: Box::new(Source(s)),
                sink: Box::new(sink),
            },
            Box::new(move || {
                let _ = closer.shutdown(std::net::Shutdown::Both);
            }),
        ))
    }
}

#[cfg(windows)]
mod sock {
    use super::*;
    use std::io::Read;

    struct Source(uds_windows::UnixStream);

    impl MsgSource for Source {
        fn read_some(&mut self, buf: &mut [u8], _handles: &mut Vec<Handle>) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    pub fn connect(
        path: &std::path::Path,
        timeout: Option<Duration>,
    ) -> io::Result<(Conn, Interrupt)> {
        let s = uds_windows::UnixStream::connect(path)?;
        s.set_read_timeout(timeout)?;
        let sink = s.try_clone()?;
        let closer = s.try_clone()?;
        Ok((
            Conn {
                source: Box::new(Source(s)),
                sink: Box::new(sink),
            },
            Box::new(move || {
                let _ = closer.shutdown(std::net::Shutdown::Both);
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::Taken;
    use crate::region::testing::{fill_lines, Block, Heap};
    use crate::region::Memory;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicU32;

    enum Step {
        /// Bytes, with the handles that ride on the first of them.
        Data(Vec<u8>, Vec<Handle>),
        Timeout,
    }

    struct Script(VecDeque<Step>);

    impl MsgSource for Script {
        fn read_some(&mut self, buf: &mut [u8], handles: &mut Vec<Handle>) -> io::Result<usize> {
            match self.0.pop_front() {
                None => Ok(0),
                Some(Step::Timeout) => Err(io::ErrorKind::TimedOut.into()),
                Some(Step::Data(mut bytes, hs)) => {
                    handles.extend(hs);
                    let n = bytes.len().min(buf.len());
                    buf[..n].copy_from_slice(&bytes[..n]);
                    if n < bytes.len() {
                        self.0
                            .push_front(Step::Data(bytes.split_off(n), Vec::new()));
                    }
                    Ok(n)
                }
            }
        }
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            lock(&self.0).extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn header(kind: u16, len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[0..2].copy_from_slice(&kind.to_le_bytes());
        b
    }

    fn put32(b: &mut [u8], o: usize, v: u32) {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn welcome(backend: u32, control_backend: u32) -> Vec<u8> {
        let mut b = header(MSG_WELCOME, 64);
        put32(&mut b, 8, 3);
        put32(&mut b, 12, backend);
        put32(&mut b, 16, 1);
        put32(&mut b, 20, control_backend);
        b
    }

    fn region(id: u32, backend: u32, size: u64) -> Vec<u8> {
        let mut b = header(MSG_REGION, 56);
        put32(&mut b, 8, id);
        put32(&mut b, 12, 1);
        put32(&mut b, 16, backend);
        b[24..32].copy_from_slice(&size.to_le_bytes());
        b[32..40].copy_from_slice(&size.to_le_bytes());
        b
    }

    fn id_msg(kind: u16, id: u32) -> Vec<u8> {
        let mut b = header(kind, 16);
        put32(&mut b, 8, id);
        b
    }

    fn mem(h: &Arc<Heap>) -> Vec<Handle> {
        vec![Handle::Memory(h.clone() as Arc<dyn Memory>)]
    }

    /// A machine's side of a session: a control block with one xrgb head and
    /// its plane.
    struct Machine {
        block: Block,
        plane: Arc<Heap>,
    }

    fn machine(w: u32, h: u32) -> Machine {
        let block = Block::new(1);
        let plane = Heap::new(1 << 18);
        block.set_layout(0, w, h, 1, 0, w * 4, HEAD_DIRTY_EXACT);
        fill_lines(&plane, 0, (w * 4) as usize, w as usize, 0..h, &|y| {
            0x500 + y
        });
        Machine { block, plane }
    }

    /// The messages of a session start: WELCOME with the control block, then
    /// the plane's REGION.
    fn session(m: &Machine) -> Vec<Step> {
        vec![
            Step::Data(welcome(3, 3), mem(&m.block.mem)),
            Step::Data(region(1, 3, 1 << 18), mem(&m.plane)),
        ]
    }

    fn run_pump(display: &Display, steps: Vec<Step>) -> io::Result<u32> {
        let woke = AtomicU32::new(0);
        let r = pump(
            display,
            &mut Script(steps.into()),
            &AtomicBool::new(false),
            &|| {
                woke.fetch_add(1, Ordering::SeqCst);
            },
        );
        r.map(|_| woke.load(Ordering::SeqCst))
    }

    fn err_of(steps: Vec<Step>) -> String {
        run_pump(&Display::default(), steps)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn a_welcome_and_a_region_make_a_head_that_can_be_taken() {
        let m = machine(16, 8);
        let d = Display::default();
        let e = run_pump(&d, session(&m)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "the stream ended");
        let h = d.heads();
        assert_eq!(h.len(), 1);
        assert!(h[0].active);
        let mut px = Vec::new();
        match d.take(0, &mut px).unwrap() {
            Taken::Frame(f) => assert_eq!((f.width, f.height, f.runs), (16, 8, vec![(0, 8)])),
            other => panic!("{other:?}"),
        }
        assert_eq!(u32::from_le_bytes(px[..4].try_into().unwrap()), 0x500);
    }

    #[test]
    fn messages_split_across_reads_with_timeouts_keep_their_handles() {
        let m = machine(16, 8);
        let d = Display::default();
        let w = welcome(3, 3);
        let steps = vec![
            // The header in two pieces, the handle on the first byte.
            Step::Data(w[..3].to_vec(), mem(&m.block.mem)),
            Step::Timeout,
            Step::Data(w[3..8].to_vec(), Vec::new()),
            Step::Data(w[8..30].to_vec(), Vec::new()),
            Step::Timeout,
            Step::Data(w[30..].to_vec(), Vec::new()),
            Step::Data(region(1, 3, 1 << 18), mem(&m.plane)),
        ];
        let _ = run_pump(&d, steps);
        assert!(d.heads()[0].active);
    }

    /// An importer that imports nothing real: it records what it was asked.
    struct FakeImporter {
        accepts: Result<(), String>,
        import: Result<(), String>,
        wrap: bool,
        /// The API opens a plain section from its handle (Direct3D 12).
        open: bool,
        imported: Mutex<Vec<(u32, u32, u64)>>,
    }

    struct Held;

    impl origami_engine::GpuMemory for Held {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    impl crate::import::Importer for FakeImporter {
        fn caps(&self) -> u32 {
            CAP_VULKAN
        }
        fn device_uuid(&self) -> [u8; 16] {
            [7; 16]
        }
        fn export_uuid(&self) -> Option<[u8; 16]> {
            Some([7; 16])
        }
        fn accepts(&self, device: &[u8; 16], driver: &[u8; 16]) -> Result<(), String> {
            assert_eq!((device, driver), (&[7; 16], &[8; 16]));
            self.accepts.clone()
        }
        fn import(
            &self,
            r: &Region,
            _handle: Handle,
        ) -> io::Result<Arc<dyn origami_engine::GpuMemory>> {
            lock(&self.imported).push((r.id, r.memory_type, r.alloc_size));
            self.import
                .clone()
                .map(|_| Arc::new(Held) as Arc<dyn origami_engine::GpuMemory>)
                .map_err(io::Error::other)
        }
        fn wrap(
            &self,
            _r: &Region,
            _m: &Arc<dyn Memory>,
        ) -> Option<Arc<dyn origami_engine::GpuMemory>> {
            self.wrap
                .then(|| Arc::new(Held) as Arc<dyn origami_engine::GpuMemory>)
        }
        fn open_section(
            &self,
            _r: &Region,
            _h: &Handle,
        ) -> Result<Arc<dyn origami_engine::GpuMemory>, String> {
            self.open
                .then(|| Arc::new(Held) as Arc<dyn origami_engine::GpuMemory>)
                .ok_or_else(|| "off".into())
        }
        fn name(&self) -> &'static str {
            "fake import"
        }
    }

    fn importer(
        accepts: Result<(), String>,
        import: Result<(), String>,
        wrap: bool,
    ) -> Arc<FakeImporter> {
        Arc::new(FakeImporter {
            accepts,
            import,
            wrap,
            open: false,
            imported: Mutex::new(Vec::new()),
        })
    }

    /// A WELCOME for the GPU backend, naming the adapter and driver the
    /// fake importer expects.
    fn gpu_welcome() -> Vec<u8> {
        let mut b = welcome(BACKEND_VK_FD, 3);
        b[24..40].copy_from_slice(&[7; 16]);
        b[40..56].copy_from_slice(&[8; 16]);
        b
    }

    /// A REGION of exported GPU memory: Vulkan's memory type and allocation
    /// size ride in it.
    fn gpu_region(id: u32, size: u64) -> Vec<u8> {
        let mut b = region(id, BACKEND_VK_FD, size);
        put32(&mut b, 20, 5);
        b[32..40].copy_from_slice(&(size + 4096).to_le_bytes());
        b
    }

    #[test]
    fn exported_gpu_memory_is_imported_and_the_head_is_named_not_copied() {
        let m = machine(16, 8);
        let d = Display::default();
        let fake = importer(Ok(()), Ok(()), false);
        d.set_importer(fake.clone());
        // The handle that comes with the region is whatever the OS passed; the
        // plane heap stands in for it, and is not mapped.
        let steps = vec![
            Step::Data(gpu_welcome(), mem(&m.block.mem)),
            Step::Data(gpu_region(1, 1 << 18), mem(&m.plane)),
        ];
        let _ = run_pump(&d, steps);
        assert_eq!(*lock(&fake.imported), vec![(1, 5, (1 << 18) + 4096)]);
        assert_eq!(d.backend().as_deref(), Some("fake import"));
        assert!(d.heads()[0].active);
        let mut px = Vec::new();
        match d.take(0, &mut px).unwrap() {
            Taken::Imported(f) => assert_eq!(f.update.runs, vec![(0, 8)]),
            other => panic!("{other:?}"),
        }
        assert!(px.is_empty());
    }

    #[test]
    fn gpu_memory_that_cannot_be_imported_is_an_error_and_never_a_picture() {
        let m = machine(16, 8);
        let session = |gpu_region: Vec<u8>| {
            vec![
                Step::Data(gpu_welcome(), mem(&m.block.mem)),
                Step::Data(gpu_region, mem(&m.plane)),
            ]
        };
        // No importer at all.
        assert!(err_of(session(gpu_region(1, 1 << 18))).contains("cannot import"));
        // Another adapter or driver.
        let d = Display::default();
        d.set_importer(importer(
            Err("QEMU exported on adapter X".into()),
            Ok(()),
            false,
        ));
        let e = run_pump(&d, session(gpu_region(1, 1 << 18))).unwrap_err();
        assert!(e.to_string().contains("adapter X"), "{e}");
        assert!(!d.is_attached(), "nothing was shown");
        // The driver refuses the import.
        let d = Display::default();
        d.set_importer(importer(
            Ok(()),
            Err("vkAllocateMemory failed".into()),
            false,
        ));
        let e = run_pump(&d, session(gpu_region(1, 1 << 18))).unwrap_err();
        assert!(e.to_string().contains("cannot import region 1"), "{e}");
        assert!(
            !d.heads()[0].active,
            "a region that was not imported draws nothing"
        );
        // A region smaller than its allocation.
        let d = Display::default();
        d.set_importer(importer(Ok(()), Ok(()), false));
        let mut r = gpu_region(1, 1 << 18);
        r[32..40].copy_from_slice(&(1u64 << 10).to_le_bytes());
        assert!(run_pump(&d, session(r))
            .unwrap_err()
            .to_string()
            .contains("allocates"));
    }

    #[test]
    fn shared_memory_the_gpu_can_wrap_is_wrapped_and_otherwise_copied() {
        let m = machine(16, 8);
        for (wrap, want_import) in [(true, true), (false, false)] {
            let d = Display::default();
            d.set_importer(importer(Ok(()), Ok(()), wrap));
            let _ = run_pump(&d, session(&m));
            let mut px = Vec::new();
            match d.take(0, &mut px).unwrap() {
                Taken::Imported(_) => assert!(want_import && px.is_empty()),
                Taken::Frame(_) => assert!(!want_import && !px.is_empty()),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_section_the_gpu_opens_by_handle_is_drawn_in_place() {
        let m = machine(16, 8);
        let d = Display::default();
        d.set_importer(Arc::new(FakeImporter {
            accepts: Ok(()),
            import: Ok(()),
            wrap: false,
            open: true,
            imported: Mutex::new(Vec::new()),
        }));
        let _ = run_pump(&d, session(&m));
        let mut px = Vec::new();
        assert!(matches!(d.take(0, &mut px).unwrap(), Taken::Imported(_)));
        assert!(px.is_empty());
    }

    #[test]
    fn a_cursor_image_region_is_never_wrapped_for_the_gpu() {
        let m = machine(16, 8);
        let d = Display::default();
        d.set_importer(importer(Ok(()), Ok(()), true));
        let mut cursor = region(2, BACKEND_SHM_FD, 1 << 16);
        put32(&mut cursor, 12, KIND_CURSOR_IMAGE);
        let mut steps = session(&m);
        steps.push(Step::Data(cursor, mem(&Heap::new(1 << 16))));
        let _ = run_pump(&d, steps);
        assert_eq!(
            d.backend().as_deref(),
            Some("fake import"),
            "the plane's, not the cursor's"
        );
        let mut px = Vec::new();
        assert!(matches!(d.take(0, &mut px).unwrap(), Taken::Imported(_)));
    }

    #[test]
    fn the_hello_says_what_the_importer_can_do() {
        let m = machine(16, 8);
        let first = Capture::default();
        let mut link = Scripted {
            conns: VecDeque::from([(session(&m), first.clone())]),
            lives_for: 1,
            refuse: 0,
            connects: 0,
        };
        let d = Display::default();
        d.set_importer(importer(Ok(()), Ok(()), false));
        run(
            &mut link,
            &d,
            &new_uplink(),
            &AtomicBool::new(false),
            quick(),
            &|| {},
        );
        let sent = lock(&first.0).clone();
        assert_eq!(
            u32::from_le_bytes(sent[12..16].try_into().unwrap()),
            CAP_VULKAN | CAP_WAKE
        );
        assert_eq!(&sent[24..40], &[7; 16]);
    }

    #[test]
    fn a_region_gone_drops_the_mapping_and_the_head_with_it() {
        let m = machine(16, 8);
        let d = Display::default();
        let mut steps = session(&m);
        steps.push(Step::Data(id_msg(MSG_REGION_GONE, 1), Vec::new()));
        let _ = run_pump(&d, steps);
        assert!(!d.heads()[0].active);
    }

    #[test]
    fn a_wake_arms_the_head_again_and_wakes_the_ui() {
        let m = machine(16, 8);
        let d = Display::default();
        let mut steps = session(&m);
        steps.push(Step::Data(id_msg(MSG_WAKE, 0), Vec::new()));
        let before = m.block.get(0, R_ARMED);
        assert_eq!(before, 0);
        let woke = run_pump(&d, steps).map(|_| 0).unwrap_or_else(|_| 0);
        let _ = woke;
        assert_eq!(m.block.get(0, R_ARMED), 1);
        // A viewer nobody is looking at does not arm.
        m.block.put(0, R_ARMED, 0);
        let d = Display::default();
        d.set_visible(false);
        let mut steps = session(&m);
        steps.push(Step::Data(id_msg(MSG_WAKE, 0), Vec::new()));
        let _ = run_pump(&d, steps);
        assert_eq!(m.block.get(0, R_ARMED), 0);
    }

    #[test]
    fn the_protocol_errors_close_the_connection() {
        let m = machine(8, 8);
        // An unknown type.
        assert!(err_of(vec![Step::Data(header(0x00ee, 8), Vec::new())])
            .contains("unknown message type"));
        // Our own types coming back.
        assert!(err_of(vec![Step::Data(header(MSG_HELLO, 48), Vec::new())]).contains("unknown"));
        // A region, a wake or a removal before the WELCOME.
        for msg in [
            region(1, 3, 1 << 16),
            id_msg(MSG_WAKE, 0),
            id_msg(MSG_REGION_GONE, 1),
        ] {
            assert!(err_of(vec![Step::Data(msg, Vec::new())]).contains("before the WELCOME"));
        }
        // A second WELCOME.
        let mut steps = session(&m);
        steps.push(Step::Data(welcome(3, 3), mem(&m.block.mem)));
        assert!(err_of(steps).contains("second WELCOME"));
        // A version we do not speak.
        let mut w = welcome(3, 3);
        put32(&mut w, 8, 2);
        assert!(err_of(vec![Step::Data(w, mem(&m.block.mem))]).contains("version 2"));
        // A GPU backend, which this viewer did not ask for.
        assert!(
            err_of(vec![Step::Data(welcome(1, 3), mem(&m.block.mem))]).contains("cannot import")
        );
        // A control block that is not one.
        let junk = Heap::new(CONTROL_BYTES);
        assert!(err_of(vec![Step::Data(welcome(3, 3), mem(&junk))]).contains("magic"));
        // A welcome with no handle.
        #[cfg(unix)]
        assert!(err_of(vec![Step::Data(welcome(3, 3), Vec::new())]).contains("carried none"));
        // A region smaller than it says, or with id 0, or the wrong backend.
        let small = Heap::new(1 << 16);
        for (id, backend, size) in [
            (1, 3, 1u64 << 20),
            (0, 3, 1 << 16),
            (1, 1, 1 << 16),
            (1, 3, 1000),
        ] {
            let mut steps = vec![Step::Data(welcome(3, 3), mem(&m.block.mem))];
            steps.push(Step::Data(region(id, backend, size), mem(&small)));
            assert!(err_of(steps).contains("region"), "{id} {backend} {size}");
        }
    }

    #[test]
    fn a_stream_that_ends_mid_message_is_an_error_not_a_message() {
        let w = welcome(3, 3);
        let e = run_pump(
            &Display::default(),
            vec![Step::Data(w[..20].to_vec(), Vec::new())],
        )
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }

    /// A machine that appears, goes and comes back: each `connect` takes the
    /// next prepared connection.
    struct Scripted {
        conns: VecDeque<(Vec<Step>, Capture)>,
        /// The machine lives until this many connections have been made.
        lives_for: u32,
        refuse: u32,
        connects: u32,
    }

    impl Link for Scripted {
        fn connect(&mut self) -> io::Result<Conn> {
            if self.refuse > 0 {
                self.refuse -= 1;
                return Err(io::ErrorKind::ConnectionRefused.into());
            }
            let (steps, cap) = self
                .conns
                .pop_front()
                .ok_or(io::Error::from(io::ErrorKind::ConnectionRefused))?;
            self.connects += 1;
            Ok(Conn {
                source: Box::new(Script(steps.into())),
                sink: Box::new(cap),
            })
        }
        fn alive(&mut self) -> bool {
            self.connects < self.lives_for
        }
    }

    fn quick() -> Timing {
        Timing {
            first_connect: Duration::from_millis(300),
            retry: Duration::from_millis(5),
        }
    }

    fn new_uplink() -> Uplink {
        Arc::new(Mutex::new(None))
    }

    #[test]
    fn the_display_says_hello_then_reconnects_to_a_restarted_machine_and_ends_with_it() {
        let first = machine(16, 8);
        first.block.bump(0);
        first.block.bump(0);
        let second = machine(12, 6);
        let (c1, c2) = (Capture::default(), Capture::default());
        let mut link = Scripted {
            conns: VecDeque::from([
                (session(&first), c1.clone()),
                (session(&second), c2.clone()),
            ]),
            lives_for: 2,
            refuse: 2,
            connects: 0,
        };
        let d = Display::default();
        let uplink = new_uplink();
        let stop = AtomicBool::new(false);
        let wakes = AtomicU32::new(0);
        let exit = run(&mut link, &d, &uplink, &stop, quick(), &|| {
            wakes.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(exit, Exit::Gone);
        assert_eq!(link.connects, 2, "two connections after two refusals");
        for c in [&c1, &c2] {
            let sent = lock(&c.0).clone();
            assert_eq!(sent.len(), 48, "HELLO and nothing else");
            assert_eq!(message_type(&sent), MSG_HELLO);
            assert_eq!(u32::from_le_bytes(sent[8..12].try_into().unwrap()), 3);
            assert_eq!(
                u32::from_le_bytes(sent[16..20].try_into().unwrap()),
                std::process::id()
            );
        }
        assert!(
            lock(&uplink).is_none(),
            "no input channel once the connection ended"
        );
        assert!(!d.is_attached(), "detached when the machine is gone");
        assert!(wakes.load(Ordering::SeqCst) >= 4);
    }

    #[test]
    fn sequence_numbers_climb_across_a_reconnect() {
        let first = machine(16, 8);
        for _ in 0..5 {
            first.block.bump(0);
        }
        let second = machine(12, 6);
        let d = Display::default();
        let mut seen = Vec::new();
        for m in [&first, &second] {
            let _ = run_pump(&d, session(m));
            seen.push(d.heads()[0].seq);
            d.detach();
        }
        assert!(seen[1] > seen[0], "{seen:?}");
    }

    #[test]
    fn input_goes_out_on_the_connection_while_it_lives() {
        // A link whose connection stays up until told: the input channel is
        // filled after HELLO and the same Capture sees both.
        struct Held {
            cap: Capture,
            release: Arc<AtomicBool>,
        }
        struct Block1(Arc<AtomicBool>, Vec<Step>);
        impl MsgSource for Block1 {
            fn read_some(&mut self, buf: &mut [u8], h: &mut Vec<Handle>) -> io::Result<usize> {
                if let Some(Step::Data(..)) = self.1.first() {
                    return Script(std::mem::take(&mut self.1).into()).read_some(buf, h);
                }
                if self.0.load(Ordering::SeqCst) {
                    Ok(0)
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                    Err(io::ErrorKind::TimedOut.into())
                }
            }
        }
        impl Link for Held {
            fn connect(&mut self) -> io::Result<Conn> {
                let m = machine(8, 8);
                Ok(Conn {
                    source: Box::new(Block1(self.release.clone(), session(&m))),
                    sink: Box::new(self.cap.clone()),
                })
            }
            fn alive(&mut self) -> bool {
                false
            }
        }
        let cap = Capture::default();
        let release = Arc::new(AtomicBool::new(false));
        let uplink = new_uplink();
        let d = Arc::new(Display::default());
        let t = {
            let (cap, release, uplink, d) =
                (cap.clone(), release.clone(), uplink.clone(), d.clone());
            std::thread::spawn(move || {
                let mut link = Held { cap, release };
                run(
                    &mut link,
                    &d,
                    &uplink,
                    &AtomicBool::new(false),
                    quick(),
                    &|| {},
                )
            })
        };
        for _ in 0..200 {
            if lock(&uplink).is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        {
            let mut up = lock(&uplink);
            let w = up.as_mut().expect("an input channel while connected");
            w.write_all(
                &Input {
                    kind: InputKind::Key,
                    head: 0,
                    code: 30,
                    value: 1,
                    x: 0,
                    y: 0,
                }
                .encode(false, 1),
            )
            .unwrap();
        }
        release.store(true, Ordering::SeqCst);
        assert_eq!(t.join().unwrap(), Exit::Gone);
        let sent = lock(&cap.0).clone();
        assert_eq!(sent.len(), 48 + 32);
        assert_eq!(message_type(&sent[48..]), MSG_INPUT);
    }

    #[test]
    fn a_display_that_never_comes_gives_up_and_a_stop_ends_the_wait() {
        let mut link = Scripted {
            conns: VecDeque::new(),
            lives_for: u32::MAX,
            refuse: 0,
            connects: 0,
        };
        let d = Display::default();
        let stop = AtomicBool::new(false);
        assert_eq!(
            run(&mut link, &d, &new_uplink(), &stop, quick(), &|| {}),
            Exit::NeverCame
        );
        stop.store(true, Ordering::SeqCst);
        assert_eq!(
            run(&mut link, &d, &new_uplink(), &stop, quick(), &|| {}),
            Exit::Stopped
        );
    }

    #[test]
    fn the_link_facts_say_which_stage_the_display_reached() {
        use origami_engine::status::{stage, Facts, Reason};
        let facts_of = |d: &Display| Facts {
            running: true,
            socket_exists: true,
            link: d.link_facts(),
            ..Default::default()
        };
        // Nothing yet: not connected.
        let d = Display::default();
        assert_eq!(stage(&facts_of(&d)), Reason::NotConnected);
        // Connected, no WELCOME.
        d.link_up();
        assert_eq!(stage(&facts_of(&d)), Reason::NoWelcome);
        // A WELCOME and no REGION.
        let m = machine(16, 8);
        let steps = vec![Step::Data(welcome(3, 3), mem(&m.block.mem))];
        let _ = run_pump(&d, steps);
        let f = d.link_facts();
        assert!(f.welcomed);
        assert_eq!((f.regions, f.messages), (0, 1));
        assert_eq!(stage(&facts_of(&d)), Reason::NoRegion);
        // A region, and a head with a picture.
        let d = Display::default();
        d.link_up();
        let _ = run_pump(&d, session(&m));
        let f = d.link_facts();
        assert_eq!((f.regions, f.active_heads, f.messages), (1, 1, 2));
        // A head with a plane but no mode: regions without a picture.
        let idle = machine(16, 8);
        idle.block.set_layout(0, 0, 0, 0, 0, 0, HEAD_DIRTY_EXACT);
        let d = Display::default();
        d.link_up();
        let _ = run_pump(&d, session(&idle));
        let f = d.link_facts();
        assert_eq!((f.regions, f.active_heads), (1, 0));
        assert_eq!(stage(&facts_of(&d)), Reason::NoPicture);
    }

    #[test]
    fn a_refused_socket_and_a_protocol_failure_are_remembered_for_the_status() {
        let mut link = Scripted {
            conns: VecDeque::new(),
            lives_for: u32::MAX,
            refuse: u32::MAX,
            connects: 0,
        };
        let d = Display::default();
        run(
            &mut link,
            &d,
            &new_uplink(),
            &AtomicBool::new(false),
            quick(),
            &|| {},
        );
        let f = d.link_facts();
        assert!(f.connect_failures >= 2, "{f:?}");
        assert!(
            f.connect_error
                .as_deref()
                .is_some_and(|e| e.contains("refused")),
            "{f:?}"
        );
        assert!(!f.connected);

        // A connection that fails the protocol: the reason stays after it drops.
        let mut link = Scripted {
            conns: VecDeque::from([(
                vec![Step::Data(header(0x00ee, 8), Vec::new())],
                Capture::default(),
            )]),
            lives_for: 1,
            refuse: 0,
            connects: 0,
        };
        let d = Display::default();
        let exit = run(
            &mut link,
            &d,
            &new_uplink(),
            &AtomicBool::new(false),
            quick(),
            &|| {},
        );
        assert_eq!(exit, Exit::Gone);
        let f = d.link_facts();
        assert_eq!(f.connects, 1);
        assert!(!f.connected && !f.welcomed);
        assert!(
            f.protocol_error
                .as_deref()
                .is_some_and(|e| e.contains("unknown message type")),
            "{f:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_interrupt_ends_a_read_that_would_block_forever() {
        let path = std::env::temp_dir().join(format!("od-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let (mut conn, interrupt) = connect_socket_blocking(&path).unwrap();
        let (_peer, _) = listener.accept().unwrap();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 8];
            conn.source.read_some(&mut buf, &mut Vec::new())
        });
        std::thread::sleep(Duration::from_millis(300));
        assert!(!reader.is_finished(), "the read returned with nothing sent");
        interrupt();
        assert_eq!(reader.join().unwrap().unwrap(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_machine_that_is_gone_before_its_display_appears_ends_the_wait() {
        let mut link = Scripted {
            conns: VecDeque::new(),
            lives_for: 0,
            refuse: 0,
            connects: 0,
        };
        let d = Display::default();
        assert_eq!(
            run(
                &mut link,
                &d,
                &new_uplink(),
                &AtomicBool::new(false),
                quick(),
                &|| {}
            ),
            Exit::Gone
        );
    }
}
