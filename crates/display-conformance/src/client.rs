//! Test clients: a viewer built on the product's session library, and a raw
//! socket for the checks that break the rules on purpose.

use origami_display::v4::session::{Event, FrameLease, HelloParams, Session, SessionError};
use origami_display::v4::testext::{self, Control, InputEcho, Stats, Stimulus};
use origami_display::v4::transport::{send_all, Fault, Reader};
use origami_display::v4::wire::{self, ErrorMsg, Head, Header, Hello, ToViewer};
use std::collections::VecDeque;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub type R<T> = Result<T, String>;

#[macro_export]
macro_rules! ensure {
    ($c:expr, $($a:tt)+) => { if !($c) { return Err(format!($($a)+)); } };
}

#[derive(Clone)]
pub struct Target {
    pub path: PathBuf,
}

impl Target {
    pub fn stream(&self) -> R<UnixStream> {
        UnixStream::connect(&self.path).map_err(|e| format!("connect {}: {e}", self.path.display()))
    }
}

#[derive(Clone)]
pub struct Opts {
    pub features: u32,
    pub credit: u32,
    pub max_w: u32,
    pub max_h: u32,
    pub test: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            features: wire::feat::INPUT,
            credit: 1,
            max_w: 16384,
            max_h: 16384,
            test: true,
        }
    }
}

impl Opts {
    pub fn credit(mut self, c: u32) -> Self {
        self.credit = c;
        self
    }
    pub fn rw(mut self) -> Self {
        self.features |= wire::feat::RW_PIXELS;
        self
    }
    pub fn no_input(mut self) -> Self {
        self.features &= !wire::feat::INPUT;
        self
    }
}

/// Everything a viewer can receive except frames.
#[derive(Debug, Clone)]
pub enum Item {
    Head(Head),
    CursorNotify { head: u32, seq: u32 },
    CursorImage(wire::CursorImage),
    InputAck(u32),
    InputState { flags: u32 },
    RegionGone(u32),
    Echo(InputEcho),
    ControlDone { op: u32, result: u32 },
    Stats(Stats),
    Other,
}

pub enum Got {
    Frame(FrameLease),
    Item(Item),
}

pub struct V {
    pub s: Session,
    pub ix: u8,
    pub id: u32,
    pub log: Vec<Item>,
    pending: VecDeque<Got>,
}

fn err(e: SessionError) -> String {
    e.to_string()
}

impl V {
    pub fn connect(t: &Target, o: &Opts) -> R<V> {
        let mut features = o.features;
        let mut offers = vec![];
        if o.test {
            features |= wire::feat::TEST;
            offers.push(wire::Offer {
                name: testext::NAME.into(),
                version_min: testext::VERSION,
                version_max: testext::VERSION,
                payload: vec![],
            });
        }
        let p = HelloParams {
            name: "conformance".into(),
            features,
            credit: o.credit,
            max_width: o.max_w,
            max_height: o.max_h,
            offers,
            ..Default::default()
        };
        let s = Session::connect(t.stream()?, &p).map_err(err)?;
        let ix = s
            .welcome
            .accepts
            .iter()
            .find(|a| a.name == testext::NAME)
            .map_or(0, |a| a.index);
        ensure!(!o.test || ix != 0, "the producer did not enable qemu.test");
        let id = s.welcome.viewer_id;
        Ok(V {
            s,
            ix,
            id,
            log: vec![],
            pending: VecDeque::new(),
        })
    }

    pub fn head(&self, h: u32) -> R<Head> {
        self.s
            .head(h)
            .ok_or_else(|| format!("no HEAD for head {h}"))
    }

    fn convert(&mut self, e: Event) -> Got {
        match e {
            Event::Frame(f) => Got::Frame(f),
            Event::Head(h) => Got::Item(Item::Head(h)),
            Event::CursorNotify { head, cursor_seq } => Got::Item(Item::CursorNotify {
                head,
                seq: cursor_seq,
            }),
            Event::CursorImage(c) => Got::Item(Item::CursorImage(c)),
            Event::InputAck(s) => Got::Item(Item::InputAck(s)),
            Event::InputState { flags, .. } => Got::Item(Item::InputState { flags }),
            Event::RegionGone(r) => Got::Item(Item::RegionGone(r)),
            Event::Unknown { .. } => Got::Item(Item::Other),
            Event::Ext { ty, msg } => {
                let local = (ty & 0xFF) as u8;
                let it = match local {
                    testext::INPUT_ECHO => testext::InputEcho::decode(&msg)
                        .map(Item::Echo)
                        .unwrap_or(Item::Other),
                    testext::STATS => Stats::decode(&msg).map(Item::Stats).unwrap_or(Item::Other),
                    testext::CONTROL_DONE => Item::ControlDone {
                        op: u32::from_le_bytes(msg[8..12].try_into().unwrap()),
                        result: u32::from_le_bytes(msg[12..16].try_into().unwrap()),
                    },
                    _ => Item::Other,
                };
                Got::Item(it)
            }
        }
    }

    /// The next event of any kind. `Ok(None)` on timeout.
    pub fn next(&mut self, timeout: Duration) -> R<Option<Got>> {
        if let Some(g) = self.pending.pop_front() {
            return Ok(Some(g));
        }
        match self.s.recv(Some(timeout)).map_err(err)? {
            Some(e) => Ok(Some(self.convert(e))),
            None => Ok(None),
        }
    }

    /// Wait for a frame; other items go to the log.
    pub fn frame(&mut self, timeout: Duration) -> R<Option<FrameLease>> {
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.next(left)? {
                Some(Got::Frame(f)) => return Ok(Some(f)),
                Some(Got::Item(i)) => self.log.push(i),
                None => return Ok(None),
            }
            if Instant::now() >= end {
                return Ok(None);
            }
        }
    }

    pub fn frame_or_fail(&mut self, what: &str) -> R<FrameLease> {
        self.frame(Duration::from_secs(5))?
            .ok_or_else(|| format!("no FRAME arrived: {what}"))
    }

    /// Expect no frame for `d`.
    pub fn quiet(&mut self, d: Duration, what: &str) -> R<()> {
        match self.frame(d)? {
            None => Ok(()),
            Some(f) => Err(format!(
                "unexpected FRAME serial {} ({what})",
                f.frame.serial
            )),
        }
    }

    /// Wait for an item matching `pick`, logging the rest and keeping frames queued.
    pub fn item<T>(
        &mut self,
        timeout: Duration,
        mut pick: impl FnMut(&Item) -> Option<T>,
    ) -> R<Option<T>> {
        let end = Instant::now() + timeout;
        let mut frames = vec![];
        let out = loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.next(left)? {
                Some(Got::Item(i)) => {
                    if let Some(t) = pick(&i) {
                        break Some(t);
                    }
                    self.log.push(i);
                }
                Some(Got::Frame(f)) => frames.push(Got::Frame(f)),
                None => break None,
            }
            if Instant::now() >= end {
                break None;
            }
        };
        for f in frames.into_iter().rev() {
            self.pending.push_front(f);
        }
        Ok(out)
    }

    /// Read whatever arrives within `d`, logging items; frames are returned.
    pub fn collect(&mut self, d: Duration) -> R<Vec<FrameLease>> {
        let end = Instant::now() + d;
        let mut fr = vec![];
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.next(left)? {
                Some(Got::Frame(f)) => fr.push(f),
                Some(Got::Item(i)) => self.log.push(i),
                None => return Ok(fr),
            }
            if Instant::now() >= end {
                return Ok(fr);
            }
        }
    }

    pub fn send(&self, m: &[u8]) -> R<()> {
        self.s.send(m).map_err(|e| e.to_string())
    }

    pub fn stim(&self, head: u32, kind: u32, seed: u64, step: u32, arg: u32) -> R<()> {
        self.send(
            &Stimulus {
                head,
                kind,
                seed,
                step,
                arg,
            }
            .encode(self.ix),
        )
    }

    pub fn pat(&self, head: u32, pat: u32, seed: u64, step: u32) -> R<()> {
        self.stim(head, testext::kind::PATTERN, seed, step, pat)
    }

    /// Send a `CONTROL` and wait for its answer.
    pub fn control(&mut self, op: u32, head: u32, target: u32, arg: u32) -> R<u32> {
        self.send(
            &Control {
                op,
                head,
                target_viewer: target,
                arg,
            }
            .encode(self.ix),
        )?;
        self.item(Duration::from_secs(5), |i| match i {
            Item::ControlDone { op: o, result } if *o == op => Some(*result),
            _ => None,
        })?
        .ok_or_else(|| format!("no CONTROL_DONE for op {op}"))
    }

    pub fn stats(&mut self) -> R<Stats> {
        self.send(&testext::stats_request(self.ix))?;
        self.item(Duration::from_secs(5), |i| {
            if let Item::Stats(s) = i {
                Some(*s)
            } else {
                None
            }
        })?
        .ok_or_else(|| "no STATS".to_string())
    }

    /// Request a frame and wait for it.
    pub fn get(&mut self, head: u32, sample_now: bool) -> R<FrameLease> {
        self.s
            .request(head, sample_now)
            .map_err(|e| e.to_string())?;
        self.frame_or_fail("after a request")
    }

    /// Wait for the producer to have acted on everything sent so far:
    /// a round trip through `STATS`.
    pub fn sync(&mut self) -> R<()> {
        self.stats().map(|_| ())
    }
}

/// A bare socket.
pub struct Raw {
    pub sock: UnixStream,
    pub reader: Reader,
    pub received: Vec<(ToViewer, Header, Vec<u8>, Option<OwnedFd>)>,
    pub closed: bool,
}

impl Raw {
    pub fn connect(t: &Target) -> R<Raw> {
        Ok(Raw {
            sock: t.stream()?,
            reader: Reader::new(),
            received: vec![],
            closed: false,
        })
    }

    pub fn hello(credit: u32) -> Hello {
        Hello {
            major: wire::MAJOR,
            minor: wire::MINOR,
            features: wire::feat::INPUT,
            name: "raw".into(),
            storage: [1, 0, 0, 0, 0, 0, 0, 0],
            credit,
            max_width: 16384,
            max_height: 16384,
            prev_viewer_id: 0,
            max_slot_bytes: 0,
            prev_handles_taken: 0,
            prev_session_id: 0,
            offers: vec![],
        }
    }

    pub fn send(&self, b: &[u8]) -> R<()> {
        send_all(&self.sock, b, None).map_err(|e| e.to_string())
    }

    pub fn send_with_fd(&self, b: &[u8], fd: &impl AsRawFd) -> R<()> {
        send_all(&self.sock, b, Some(fd.as_raw_fd())).map_err(|e| e.to_string())
    }

    /// Read until `d` passes or the stream ends, parsing what arrives.
    pub fn read(&mut self, d: Duration) -> R<()> {
        let end = Instant::now() + d;
        loop {
            loop {
                let got = self
                    .reader
                    .next_with(|h, m, fd| (ToViewer::decode(h, m), h, m.to_vec(), fd))
                    .map_err(|f| format!("{f:?}"))?;
                match got {
                    Some((m, h, raw, fd)) => {
                        self.received
                            .push((m.map_err(|e| e.0.to_string())?, h, raw, fd))
                    }
                    None => break,
                }
            }
            if self.closed || Instant::now() >= end {
                return Ok(());
            }
            let left = end.saturating_duration_since(Instant::now());
            if !origami_display::v4::session::wait_readable(&self.sock, Some(left))
                .map_err(|e| e.to_string())?
            {
                return Ok(());
            }
            match self.reader.fill(self.sock.as_raw_fd()) {
                Ok(0) => self.closed = true,
                Ok(_) => {}
                Err(Fault::Io(e)) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                    self.closed = true
                }
                Err(e) => return Err(format!("{e:?}")),
            }
        }
    }

    /// The first `ERROR` the producer sends, reading up to `d`.
    pub fn error(&mut self, d: Duration) -> R<ErrorMsg> {
        self.read(d)?;
        self.received
            .iter()
            .find_map(|(m, ..)| {
                if let ToViewer::Error(e) = m {
                    Some(e.clone())
                } else {
                    None
                }
            })
            .ok_or_else(|| "no ERROR arrived".to_string())
    }

    /// Say `HELLO` and read through `READY`.
    pub fn handshake(&mut self, credit: u32) -> R<()> {
        self.send(&Raw::hello(credit).encode())?;
        let end = Instant::now() + Duration::from_secs(5);
        while !self
            .received
            .iter()
            .any(|(m, ..)| matches!(m, ToViewer::Ready))
        {
            ensure!(Instant::now() < end && !self.closed, "no READY");
            self.read(Duration::from_millis(100))?;
        }
        Ok(())
    }

    pub fn count(&self, f: impl Fn(&ToViewer) -> bool) -> usize {
        self.received.iter().filter(|(m, ..)| f(m)).count()
    }
}
