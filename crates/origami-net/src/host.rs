//! The host's answers at the frame level: ARP, ICMP echo, ICMP port
//! unreachable, and BOOTP and DHCP for one configured machine.
//!
//! These are done by hand, not by the TCP/IP stack, so the bytes match what
//! Instigator's private network sends: the frame transcripts in its
//! `internal/qemunet/testdata/transcripts` are the contract, and
//! `docs/nfs-shares.md` there ("Private network") says what they leave out.
//! Anything not answered here and addressed to the host goes on to the
//! stack for UDP and TCP.

use std::net::Ipv4Addr;

const ETH_ARP: u16 = 0x0806;
const ETH_IPV4: u16 = 0x0800;
const BROADCAST_MAC: [u8; 6] = [0xff; 6];
/// A frame the host sends is padded with zeros to the Ethernet minimum.
const MIN_FRAME: usize = 60;
/// An ICMP error quotes the offending datagram up to this much of it
/// (RFC 1812: the whole error message fits in 576 bytes).
const QUOTE_MAX: usize = 576 - 20 - 8;
const LEASE_SECONDS: u32 = 365 * 24 * 60 * 60;
const BOOTP_LEN: usize = 300;
const COOKIE: [u8; 4] = [99, 130, 83, 99];

/// The one machine BOOTP and DHCP answer.
#[derive(Clone, Debug)]
pub struct Client {
    pub mac: [u8; 6],
    pub ip: Ipv4Addr,
    /// Sent as option 12 when set.
    pub hostname: Option<String>,
}

/// BOOTP and DHCP for one machine.
#[derive(Clone, Debug)]
pub struct Addressing {
    pub client: Client,
    /// Answers DHCP too; without it a message with a DHCP type option
    /// is left for whatever else owns UDP 67.
    pub dhcp: bool,
}

/// What to do with a frame.
pub enum Verdict {
    /// The host handled it: send these frames, possibly none.
    Done(Vec<Vec<u8>>),
    /// Hand it to the TCP/IP stack.
    Pass,
}

pub struct Host {
    pub mac: [u8; 6],
    pub ip: Ipv4Addr,
    pub prefix: u8,
    /// UDP ports the stack serves, so a datagram to any other draws an
    /// ICMP port unreachable.
    pub udp_ports: Vec<u16>,
    pub addressing: Option<Addressing>,
    ident: u16,
}

impl Host {
    pub fn new(
        mac: [u8; 6],
        ip: Ipv4Addr,
        prefix: u8,
        udp_ports: Vec<u16>,
        addressing: Option<Addressing>,
    ) -> Host {
        Host {
            mac,
            ip,
            prefix,
            udp_ports,
            addressing,
            ident: 0,
        }
    }

    fn broadcast(&self) -> Ipv4Addr {
        let mask = u32::MAX.checked_shl(32 - self.prefix as u32).unwrap_or(0);
        Ipv4Addr::from(u32::from(self.ip) | !mask)
    }

    /// Looks at one frame the guest sent.
    pub fn process(&mut self, f: &[u8]) -> Verdict {
        if f.len() < 14 {
            return Verdict::Done(vec![]);
        }
        let src_mac: [u8; 6] = f[6..12].try_into().unwrap();
        match u16::from_be_bytes([f[12], f[13]]) {
            ETH_ARP => self.arp(f, src_mac),
            ETH_IPV4 => self.ipv4(f, src_mac),
            _ => Verdict::Done(vec![]),
        }
    }

    fn arp(&self, f: &[u8], src_mac: [u8; 6]) -> Verdict {
        let a = &f[14..];
        let ok = a.len() >= 28 && a[0..6] == [0, 1, 8, 0, 6, 4];
        if !ok {
            return Verdict::Done(vec![]);
        }
        match u16::from_be_bytes([a[6], a[7]]) {
            // A reply answers a request the stack made.
            2 => Verdict::Pass,
            // Whoever asks, only for our own address, and a probe from
            // 0.0.0.0 included.
            1 if a[24..28] == self.ip.octets() => {
                let mut r = Vec::with_capacity(MIN_FRAME);
                r.extend_from_slice(&src_mac);
                r.extend_from_slice(&self.mac);
                r.extend_from_slice(&ETH_ARP.to_be_bytes());
                r.extend_from_slice(&[0, 1, 8, 0, 6, 4, 0, 2]);
                r.extend_from_slice(&self.mac);
                r.extend_from_slice(&self.ip.octets());
                r.extend_from_slice(&a[8..14]);
                r.extend_from_slice(&a[14..18]);
                Verdict::Done(vec![pad(r)])
            }
            _ => Verdict::Done(vec![]),
        }
    }

    fn ipv4(&mut self, f: &[u8], src_mac: [u8; 6]) -> Verdict {
        let p = &f[14..];
        if p.len() < 20 || p[0] >> 4 != 4 {
            return Verdict::Done(vec![]);
        }
        let ihl = (p[0] & 15) as usize * 4;
        let total = u16::from_be_bytes([p[2], p[3]]) as usize;
        if ihl < 20 || total < ihl || total > p.len() {
            return Verdict::Done(vec![]);
        }
        let p = &p[..total];
        let dst = Ipv4Addr::new(p[16], p[17], p[18], p[19]);
        let src = Ipv4Addr::new(p[12], p[13], p[14], p[15]);
        let to_us = dst == self.ip;
        let broadcast = dst == Ipv4Addr::BROADCAST || dst == self.broadcast();
        if !to_us && !broadcast {
            return Verdict::Done(vec![]);
        }
        let fragment = u16::from_be_bytes([p[6], p[7]]) & 0x3fff != 0;
        let body = &p[ihl..];
        match p[9] {
            1 if !fragment => self.icmp(p, body, src, dst, src_mac),
            17 if !fragment => self.udp(p, body, src, dst, src_mac, to_us),
            // TCP, fragments and other ICMP go to the stack when meant
            // for us.
            _ if to_us => Verdict::Pass,
            _ => Verdict::Done(vec![]),
        }
    }

    fn icmp(
        &mut self,
        _p: &[u8],
        body: &[u8],
        src: Ipv4Addr,
        dst: Ipv4Addr,
        src_mac: [u8; 6],
    ) -> Verdict {
        // An echo request to the host or the subnet broadcast address;
        // the limited broadcast draws nothing.
        if body.len() < 8 || body[0] != 8 || body[1] != 0 || dst == Ipv4Addr::BROADCAST {
            return if dst == self.ip {
                Verdict::Pass
            } else {
                Verdict::Done(vec![])
            };
        }
        let mut icmp = body.to_vec();
        icmp[0] = 0;
        icmp[2] = 0;
        icmp[3] = 0;
        let c = checksum(&[&icmp]);
        icmp[2..4].copy_from_slice(&c.to_be_bytes());
        Verdict::Done(vec![self.reply_frame(src_mac, src, 1, &icmp)])
    }

    fn udp(
        &mut self,
        p: &[u8],
        body: &[u8],
        src: Ipv4Addr,
        dst: Ipv4Addr,
        src_mac: [u8; 6],
        to_us: bool,
    ) -> Verdict {
        if body.len() < 8 {
            return Verdict::Done(vec![]);
        }
        let sport = u16::from_be_bytes([body[0], body[1]]);
        let dport = u16::from_be_bytes([body[2], body[3]]);
        let ulen = u16::from_be_bytes([body[4], body[5]]) as usize;
        if ulen < 8 || ulen > body.len() {
            return Verdict::Done(vec![]);
        }
        let payload = &body[8..ulen];
        if dport == 67 && self.addressing.is_some() {
            return Verdict::Done(self.addressing_reply(payload, src, sport, src_mac));
        }
        if self.udp_ports.contains(&dport) {
            return if to_us || dst == self.broadcast() {
                Verdict::Pass
            } else {
                Verdict::Done(vec![])
            };
        }
        if !to_us {
            return Verdict::Done(vec![]);
        }
        // Nothing listens: quote the datagram in a port unreachable.
        let quote = &p[..p.len().min(20 + 8 + QUOTE_MAX)];
        let mut icmp = vec![3, 3, 0, 0, 0, 0, 0, 0];
        icmp.extend_from_slice(quote);
        let c = checksum(&[&icmp]);
        icmp[2..4].copy_from_slice(&c.to_be_bytes());
        Verdict::Done(vec![self.reply_frame(src_mac, src, 1, &icmp)])
    }

    /// One IPv4 frame from the host: TTL 64, no DF.
    fn reply_frame(&mut self, dst_mac: [u8; 6], dst: Ipv4Addr, proto: u8, body: &[u8]) -> Vec<u8> {
        self.ident = self.ident.wrapping_add(1);
        let mut f = Vec::with_capacity(14 + 20 + body.len());
        f.extend_from_slice(&dst_mac);
        f.extend_from_slice(&self.mac);
        f.extend_from_slice(&ETH_IPV4.to_be_bytes());
        let mut h = [0u8; 20];
        h[0] = 0x45;
        h[2..4].copy_from_slice(&((20 + body.len()) as u16).to_be_bytes());
        h[4..6].copy_from_slice(&self.ident.to_be_bytes());
        h[8] = 64;
        h[9] = proto;
        h[12..16].copy_from_slice(&self.ip.octets());
        h[16..20].copy_from_slice(&dst.octets());
        let c = checksum(&[&h]);
        h[10..12].copy_from_slice(&c.to_be_bytes());
        f.extend_from_slice(&h);
        f.extend_from_slice(body);
        pad(f)
    }

    fn udp_frame(
        &mut self,
        dst_mac: [u8; 6],
        dst: Ipv4Addr,
        sport: u16,
        dport: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut u = Vec::with_capacity(8 + payload.len());
        u.extend_from_slice(&sport.to_be_bytes());
        u.extend_from_slice(&dport.to_be_bytes());
        u.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        u.extend_from_slice(&[0, 0]);
        u.extend_from_slice(payload);
        let mut pseudo = Vec::with_capacity(12);
        pseudo.extend_from_slice(&self.ip.octets());
        pseudo.extend_from_slice(&dst.octets());
        pseudo.extend_from_slice(&[0, 17]);
        pseudo.extend_from_slice(&(u.len() as u16).to_be_bytes());
        let c = checksum(&[&pseudo, &u]);
        // A computed zero goes out as all ones (RFC 768).
        let c = if c == 0 { 0xffff } else { c };
        u[6..8].copy_from_slice(&c.to_be_bytes());
        self.reply_frame(dst_mac, dst, 17, &u)
    }

    /// Answers a datagram to UDP 67 for the configured machine, as DHCP
    /// when that is on and the message is one, otherwise as BOOTP.
    fn addressing_reply(
        &mut self,
        req: &[u8],
        src: Ipv4Addr,
        sport: u16,
        src_mac: [u8; 6],
    ) -> Vec<Vec<u8>> {
        let a = self.addressing.clone().expect("addressing is on");
        if a.dhcp {
            if let Some(m) = parse_dhcp(req) {
                if m.mac != a.client.mac {
                    return vec![];
                }
                let Some(typ) = dhcp_answer(&m, &a.client, self.ip) else {
                    return vec![];
                };
                let reply = build_dhcp(&m, &a.client, typ, self.ip, self.prefix);
                // A renewing client that owns its address gets a unicast
                // ACK; everything else is broadcast, and always a NAK.
                let (mac, dst) = if typ == 5 && m.ciaddr != Ipv4Addr::UNSPECIFIED {
                    (src_mac, m.ciaddr)
                } else {
                    (BROADCAST_MAC, Ipv4Addr::BROADCAST)
                };
                return vec![self.udp_frame(mac, dst, 67, 68, &reply)];
            }
        }
        if req.len() < BOOTP_LEN || req[0] != 1 || req[1] != 1 || req[2] != 6 || is_dhcp(req) {
            return vec![];
        }
        if req[28..34] != a.client.mac {
            return vec![];
        }
        let reply = build_bootp(req, &a.client, self.ip, self.prefix);
        // A client that already owns an address gets a unicast reply, and
        // one that doesn't, a broadcast, to the port the request came from.
        let (mac, dst) = if src != Ipv4Addr::UNSPECIFIED {
            (src_mac, src)
        } else {
            (BROADCAST_MAC, Ipv4Addr::BROADCAST)
        };
        vec![self.udp_frame(mac, dst, 67, sport, &reply)]
    }
}

fn pad(mut f: Vec<u8>) -> Vec<u8> {
    if f.len() < MIN_FRAME {
        f.resize(MIN_FRAME, 0);
    }
    f
}

/// The Internet checksum of the concatenated parts.
pub fn checksum(parts: &[&[u8]]) -> u16 {
    let mut sum = 0u32;
    let mut odd: Option<u8> = None;
    for part in parts {
        for &b in *part {
            match odd.take() {
                Some(hi) => sum += u32::from(u16::from_be_bytes([hi, b])),
                None => odd = Some(b),
            }
        }
    }
    if let Some(hi) = odd {
        sum += u32::from(hi) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

struct Dhcp {
    typ: u8,
    mac: [u8; 6],
    xid: [u8; 4],
    flags: [u8; 2],
    ciaddr: Ipv4Addr,
    giaddr: [u8; 4],
    requested: Option<Ipv4Addr>,
    server_id: Option<Ipv4Addr>,
    chaddr: [u8; 16],
}

/// Reads an Ethernet BOOTREQUEST that carries the magic cookie and a
/// message type option. Anything else, a truncated option included, is not
/// DHCP.
fn parse_dhcp(req: &[u8]) -> Option<Dhcp> {
    if req.len() < 241 || req[0] != 1 || req[1] != 1 || req[2] != 6 || req[236..240] != COOKIE {
        return None;
    }
    let addr = |b: &[u8]| Ipv4Addr::new(b[0], b[1], b[2], b[3]);
    let mut m = Dhcp {
        typ: 0,
        mac: req[28..34].try_into().unwrap(),
        xid: req[4..8].try_into().unwrap(),
        flags: req[10..12].try_into().unwrap(),
        ciaddr: addr(&req[12..16]),
        giaddr: req[24..28].try_into().unwrap(),
        requested: None,
        server_id: None,
        chaddr: req[28..44].try_into().unwrap(),
    };
    let mut o = &req[240..];
    while !o.is_empty() {
        match o[0] {
            0 => {
                o = &o[1..];
                continue;
            }
            255 => return (m.typ != 0).then_some(m),
            _ => {}
        }
        if o.len() < 2 || o.len() < 2 + o[1] as usize {
            return None;
        }
        let v = &o[2..2 + o[1] as usize];
        match (o[0], v.len()) {
            (53, 1) => m.typ = v[0],
            (50, 4) => m.requested = Some(addr(v)),
            (54, 4) => m.server_id = Some(addr(v)),
            _ => {}
        }
        o = &o[2 + v.len()..];
    }
    (m.typ != 0).then_some(m)
}

/// Whether a BOOTREQUEST carries DHCP message type option 53.
fn is_dhcp(req: &[u8]) -> bool {
    let o = &req[236..];
    if o.len() < 4 || o[..4] != COOKIE {
        return false;
    }
    let mut o = &o[4..];
    while !o.is_empty() {
        match o[0] {
            0 => {
                o = &o[1..];
                continue;
            }
            255 => return false,
            _ => {}
        }
        if o.len() < 2 || o.len() < 2 + o[1] as usize {
            return false;
        }
        if o[0] == 53 {
            return true;
        }
        o = &o[2 + o[1] as usize..];
    }
    false
}

/// The reply type for a message, or None for no reply: DISCOVER draws an
/// OFFER, and a REQUEST an ACK when it names the client's address and a NAK
/// for any other. One that selected another server isn't for us.
fn dhcp_answer(m: &Dhcp, c: &Client, server: Ipv4Addr) -> Option<u8> {
    match m.typ {
        1 => Some(2),
        3 => {
            if m.server_id.is_some_and(|s| s != server) {
                return None;
            }
            let want = m
                .requested
                .or((m.ciaddr != Ipv4Addr::UNSPECIFIED).then_some(m.ciaddr))?;
            Some(if want == c.ip { 5 } else { 6 })
        }
        _ => None,
    }
}

fn mask(prefix: u8) -> [u8; 4] {
    u32::MAX
        .checked_shl(32 - prefix as u32)
        .unwrap_or(0)
        .to_be_bytes()
}

fn build_dhcp(m: &Dhcp, c: &Client, typ: u8, server: Ipv4Addr, prefix: u8) -> Vec<u8> {
    let mut rep = vec![0u8; 240];
    rep[0..3].copy_from_slice(&[2, 1, 6]);
    rep[4..8].copy_from_slice(&m.xid);
    rep[10..12].copy_from_slice(&m.flags);
    if typ == 5 {
        rep[12..16].copy_from_slice(&m.ciaddr.octets());
    }
    if typ != 6 {
        rep[16..20].copy_from_slice(&c.ip.octets());
        rep[20..24].copy_from_slice(&server.octets());
    }
    rep[24..28].copy_from_slice(&m.giaddr);
    rep[28..44].copy_from_slice(&m.chaddr);
    rep[236..240].copy_from_slice(&COOKIE);
    let mut opt = |code: u8, v: &[u8]| {
        rep.push(code);
        rep.push(v.len() as u8);
        rep.extend_from_slice(v);
    };
    opt(53, &[typ]);
    opt(54, &server.octets());
    if typ != 6 {
        opt(51, &LEASE_SECONDS.to_be_bytes());
        opt(1, &mask(prefix));
        if let Some(h) = &c.hostname {
            opt(12, h.as_bytes());
        }
    }
    rep.push(255);
    if rep.len() < BOOTP_LEN {
        rep.resize(BOOTP_LEN, 0);
    }
    rep
}

fn build_bootp(req: &[u8], c: &Client, server: Ipv4Addr, prefix: u8) -> Vec<u8> {
    let mut rep = req[..BOOTP_LEN].to_vec();
    rep[0] = 2;
    rep[3] = 0;
    rep[16..20].copy_from_slice(&c.ip.octets());
    rep[20..24].copy_from_slice(&server.octets());
    rep[44..108].fill(0);
    let vend = &mut rep[236..BOOTP_LEN];
    vend.fill(0);
    vend[0..4].copy_from_slice(&COOKIE);
    let mut w = 4;
    vend[w..w + 6].copy_from_slice(&[
        1,
        4,
        mask(prefix)[0],
        mask(prefix)[1],
        mask(prefix)[2],
        mask(prefix)[3],
    ]);
    w += 6;
    if let Some(h) = &c.hostname {
        if w + 2 + h.len() < vend.len() {
            vend[w] = 12;
            vend[w + 1] = h.len() as u8;
            vend[w + 2..w + 2 + h.len()].copy_from_slice(h.as_bytes());
            w += 2 + h.len();
        }
    }
    vend[w] = 255;
    rep
}
