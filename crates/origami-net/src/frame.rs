//! QEMU stream framing and the switch's forwarding rule.

use std::io::{self, BufReader, Read};
use std::net::Ipv4Addr;

/// The longest frame accepted. QEMU's own limit is far lower.
const MAX_FRAME: usize = 65536;

/// Reads one frame, or None at a clean end of stream.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut head = [0u8; 4];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(head) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes"),
        ));
    }
    let mut frame = vec![0u8; len];
    r.read_exact(&mut frame)?;
    Ok(Some(frame))
}

/// Appends a frame and its length to `buf`.
pub fn push_frame(buf: &mut Vec<u8>, frame: &[u8]) {
    buf.extend_from_slice(&(frame.len() as u32).to_be_bytes());
    buf.extend_from_slice(frame);
}

/// Reads a frame, then every further whole frame already buffered, so the
/// caller can pass the lot on with one write instead of one each.
pub fn read_frames<R: Read>(r: &mut BufReader<R>) -> io::Result<Option<Vec<Vec<u8>>>> {
    let Some(first) = read_frame(r)? else {
        return Ok(None);
    };
    let mut frames = vec![first];
    loop {
        let held = r.buffer();
        if held.len() < 4 {
            break;
        }
        let len = u32::from_be_bytes(held[..4].try_into().unwrap()) as usize;
        if len > MAX_FRAME || held.len() < 4 + len {
            break;
        }
        match read_frame(r)? {
            Some(f) => frames.push(f),
            None => break,
        }
    }
    Ok(Some(frames))
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
pub fn route(frame: &[u8], mac: [u8; 6], ip: Ipv4Addr) -> Route {
    if frame.len() < 14 {
        return Route::Network;
    }
    if frame[0] & 1 == 1 {
        return Route::Both;
    }
    let ipv4_to_us = frame[12] == 0x08 && frame[13] == 0x00 && frame.len() >= 34 && {
        let dst = &frame[30..34];
        dst == ip.octets()
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
    const IP: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 4);

    fn ip_frame(dmac: [u8; 6], dst: [u8; 4]) -> Vec<u8> {
        let mut f = vec![0u8; 60];
        f[0..6].copy_from_slice(&dmac);
        f[12] = 8;
        f[30..34].copy_from_slice(&dst);
        f
    }

    #[test]
    fn frames_for_the_stack_stay_off_the_network() {
        assert_eq!(route(&ip_frame(MAC, [192, 0, 2, 4]), MAC, IP), Route::Stack);
        // A stale neighbour entry sends the right IP to another MAC.
        let other = [2, 0, 0, 0, 0, 9];
        assert_eq!(
            route(&ip_frame(other, [192, 0, 2, 4]), MAC, IP),
            Route::Stack
        );
        assert_eq!(route(&ip_frame(MAC, [192, 0, 2, 9]), MAC, IP), Route::Stack);
        assert_eq!(
            route(&ip_frame(other, [192, 0, 2, 2]), MAC, IP),
            Route::Network
        );
        assert_eq!(route(&[0u8; 5], MAC, IP), Route::Network);
    }

    #[test]
    fn broadcasts_reach_both() {
        assert_eq!(route(&ip_frame([0xff; 6], [255; 4]), MAC, IP), Route::Both);
        assert_eq!(
            route(&ip_frame([1, 0, 0x5e, 0, 0, 1], [224, 0, 0, 1]), MAC, IP),
            Route::Both
        );
    }

    #[test]
    fn framing_round_trips() {
        let mut buf = Vec::new();
        push_frame(&mut buf, b"hello");
        push_frame(&mut buf, b"");
        assert_eq!(&buf[..4], &[0, 0, 0, 5]);
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"hello");
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"");
        assert!(read_frame(&mut r).unwrap().is_none());
        // Whole frames already buffered come together; a partial one waits.
        push_frame(&mut buf, b"third");
        let mut r = BufReader::new(&buf[..]);
        let frames = read_frames(&mut r).unwrap().unwrap();
        assert_eq!(frames, [&b"hello"[..], b"", b"third"]);
        assert!(read_frames(&mut r).unwrap().is_none());
        let huge = [0xffu8, 0xff, 0xff, 0xff];
        assert!(read_frame(&mut &huge[..]).is_err());
    }
}
