//! QEMU stream framing and the switch's forwarding rule.

use std::io::{self, Read};
use std::net::Ipv4Addr;

/// The longest frame accepted. QEMU's own limit is far lower.
const MAX_FRAME: usize = 65536;

/// Appends a frame and its length to `buf`.
pub fn push_frame(buf: &mut Vec<u8>, frame: &[u8]) {
    buf.extend_from_slice(&(frame.len() as u32).to_be_bytes());
    buf.extend_from_slice(frame);
}

/// The frames in `framed`, which holds only whole ones (see [`Framer::whole`]).
pub fn frames(mut framed: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        if framed.len() < 4 {
            return None;
        }
        let len = u32::from_be_bytes(framed[..4].try_into().unwrap()) as usize;
        let (frame, rest) = framed[4..].split_at(len.min(framed.len() - 4));
        framed = rest;
        Some(frame)
    })
}

/// Reads a stream of length-prefixed frames into one reused buffer, so a
/// burst costs one read and no allocation per frame.
pub struct Framer {
    buf: Vec<u8>,
    /// Bytes before `start` have been handed out.
    start: usize,
    end: usize,
}

impl Framer {
    pub fn new() -> Framer {
        // Room for a maximal frame whatever is already buffered.
        Framer {
            buf: vec![0; 4 * (MAX_FRAME + 4)],
            start: 0,
            end: 0,
        }
    }

    /// Reads what the socket has, up to the buffer's room. Returns 0 at
    /// the end of the stream.
    pub fn fill<R: Read>(&mut self, r: &mut R) -> io::Result<usize> {
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
        } else if self.buf.len() - self.end < MAX_FRAME + 4 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        loop {
            match r.read(&mut self.buf[self.end..]) {
                Ok(n) => {
                    self.end += n;
                    return Ok(n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Every whole frame read so far, still carrying its length. A partial
    /// one stays behind for the next read. Fails on a frame longer than
    /// QEMU would send.
    pub fn whole(&self) -> io::Result<&[u8]> {
        let held = &self.buf[self.start..self.end];
        let mut pos = 0;
        while held.len() - pos >= 4 {
            let len = u32::from_be_bytes(held[pos..pos + 4].try_into().unwrap()) as usize;
            if len > MAX_FRAME {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("frame of {len} bytes"),
                ));
            }
            if held.len() - pos < 4 + len {
                break;
            }
            pos += 4 + len;
        }
        Ok(&held[..pos])
    }

    /// Drops the first `n` bytes, once handled.
    pub fn consume(&mut self, n: usize) {
        self.start += n;
    }
}

/// Where a frame from the guest goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// To the stack alone.
    Stack,
    /// To the machine's network alone.
    Network,
    /// To both: broadcasts and multicasts.
    Both,
}

/// Decides where a frame from the guest goes. A frame addressed to the
/// stack's MAC, or an IPv4 packet addressed to the stack's IP, never
/// reaches the network, so slirp or a real LAN can't answer for the
/// stack's address. Broadcasts reach both, so ARP finds the stack and
/// DHCP still finds the network.
pub fn route(frame: &[u8], mac: [u8; 6], ips: &[Ipv4Addr]) -> Route {
    if frame.len() < 14 {
        return Route::Network;
    }
    if frame[0] & 1 == 1 {
        return Route::Both;
    }
    let ipv4_to_us = frame[12] == 0x08 && frame[13] == 0x00 && frame.len() >= 34 && {
        let dst = &frame[30..34];
        ips.iter().any(|ip| dst == ip.octets())
    };
    if frame[0..6] == mac || ipv4_to_us {
        Route::Stack
    } else {
        Route::Network
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [8, 0, 0x69, 0, 0, 4];
    const IP: Ipv4Addr = Ipv4Addr::new(10, 98, 0, 4);

    fn ip_frame(dmac: [u8; 6], dst: [u8; 4]) -> Vec<u8> {
        let mut f = vec![0u8; 60];
        f[0..6].copy_from_slice(&dmac);
        f[12] = 8;
        f[30..34].copy_from_slice(&dst);
        f
    }

    #[test]
    fn frames_for_the_stack_stay_off_the_network() {
        assert_eq!(
            route(&ip_frame(MAC, [10, 98, 0, 4]), MAC, &[IP]),
            Route::Stack
        );
        // A stale neighbour entry sends the right IP to another MAC.
        let other = [2, 0, 0, 0, 0, 9];
        assert_eq!(
            route(&ip_frame(other, [10, 98, 0, 4]), MAC, &[IP]),
            Route::Stack
        );
        assert_eq!(
            route(&ip_frame(MAC, [10, 98, 0, 9]), MAC, &[IP]),
            Route::Stack
        );
        assert_eq!(
            route(&ip_frame(other, [10, 98, 0, 2]), MAC, &[IP]),
            Route::Network
        );
        assert_eq!(route(&[0u8; 5], MAC, &[IP]), Route::Network);
    }

    #[test]
    fn broadcasts_reach_both() {
        assert_eq!(
            route(&ip_frame([0xff; 6], [255; 4]), MAC, &[IP]),
            Route::Both
        );
        assert_eq!(
            route(&ip_frame([1, 0, 0x5e, 0, 0, 1], [224, 0, 0, 1]), MAC, &[IP]),
            Route::Both
        );
    }

    #[test]
    fn framing_round_trips() {
        let mut buf = Vec::new();
        push_frame(&mut buf, b"hello");
        push_frame(&mut buf, b"");
        assert_eq!(&buf[..4], &[0, 0, 0, 5]);
        assert_eq!(frames(&buf).collect::<Vec<_>>(), [&b"hello"[..], b""]);
    }

    #[test]
    fn a_partial_frame_waits_for_the_rest() {
        let mut stream = Vec::new();
        push_frame(&mut stream, b"hello");
        push_frame(&mut stream, b"");
        push_frame(&mut stream, b"third");
        let (head, tail) = stream.split_at(stream.len() - 2);
        let mut framer = Framer::new();
        assert_eq!(framer.fill(&mut &head[..]).unwrap(), head.len());
        let whole = framer.whole().unwrap();
        assert_eq!(frames(whole).collect::<Vec<_>>(), [&b"hello"[..], b""]);
        let n = whole.len();
        framer.consume(n);
        assert!(framer.whole().unwrap().is_empty());
        framer.fill(&mut &tail[..]).unwrap();
        let whole = framer.whole().unwrap();
        assert_eq!(frames(whole).collect::<Vec<_>>(), [&b"third"[..]]);
        let n = whole.len();
        framer.consume(n);
        assert_eq!(framer.fill(&mut &[][..]).unwrap(), 0);
    }

    #[test]
    fn a_frame_longer_than_qemu_sends_is_refused() {
        let mut framer = Framer::new();
        framer.fill(&mut &[0xffu8, 0xff, 0xff, 0xff][..]).unwrap();
        assert!(framer.whole().is_err());
    }

    #[test]
    fn a_long_stream_compacts_in_place() {
        // More than the buffer holds, in odd sizes, so frames straddle
        // reads and the tail moves back to the front.
        let mut stream = Vec::new();
        let mut want = 0;
        for i in 0..2000usize {
            push_frame(&mut stream, &vec![i as u8; 60 + i % 1400]);
            want += 1;
        }
        let mut src = &stream[..];
        let mut framer = Framer::new();
        let mut got = 0;
        loop {
            let mut chunk = &src[..src.len().min(100_003)];
            let n = framer.fill(&mut chunk).unwrap();
            src = &src[n..];
            let whole = framer.whole().unwrap();
            for f in frames(whole) {
                assert_eq!(f.len(), 60 + got % 1400);
                assert!(f.iter().all(|&b| b == got as u8));
                got += 1;
            }
            let n = whole.len();
            framer.consume(n);
            if src.is_empty() && framer.whole().unwrap().is_empty() {
                break;
            }
        }
        assert_eq!(got, want);
    }
}
