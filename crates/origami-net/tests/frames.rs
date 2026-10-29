//! Replays every frame transcript in Instigator's
//! `internal/qemunet/testdata/transcripts` against the stack, as its own Go
//! harness does for the private network: each exchange sends one Ethernet
//! frame and requires the one frame in reply, or silence. The stack runs
//! as the private network's only host, 10.98.0.2, with no share to serve.
//! The format is described in Instigator's docs/nfs-shares.md.

use std::fs;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use origami_net::{Addressing, Client, Stack, StackConfig};

const HOST_MAC: [u8; 6] = [0x08, 0x00, 0x69, 0x00, 0x00, 0x02];
const MACHINE_MAC: [u8; 6] = [0x08, 0x00, 0x69, 0x12, 0x34, 0x56];

fn transcripts() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../instigator/internal/qemunet/testdata/transcripts");
    assert!(
        dir.is_dir(),
        "{} is missing: run `git submodule update --init instigator`",
        dir.display()
    );
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    files.sort();
    assert!(files.len() >= 8, "only {} transcripts", files.len());
    files
}

struct Exchange {
    label: String,
    frame: Vec<u8>,
    reply: Vec<u8>,
}

struct Transcript {
    addressing: Option<Addressing>,
    exchanges: Vec<Exchange>,
}

fn unhex(rest: &str) -> Vec<u8> {
    let digits: String = rest.split_whitespace().collect();
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).expect("hex"))
        .collect()
}

fn parse(text: &str) -> Transcript {
    let mut tr = Transcript {
        addressing: None,
        exchanges: Vec::new(),
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "server" => assert_eq!(rest.trim(), "10.98.0.2/24 08:00:69:00:00:02"),
            "bootp" => {
                let mut words = rest.split_whitespace();
                assert_eq!(words.next(), Some("08:00:69:12:34:56"));
                let ip: Ipv4Addr = words.next().unwrap().parse().unwrap();
                let mut client = Client {
                    mac: MACHINE_MAC,
                    ip,
                    hostname: None,
                };
                let mut dhcp = false;
                for w in words {
                    match w.strip_prefix("hostname=") {
                        Some(name) => client.hostname = Some(name.to_string()),
                        None => {
                            assert_eq!(w, "dhcp", "unknown bootp option {w}");
                            dhcp = true;
                        }
                    }
                }
                tr.addressing = Some(Addressing { client, dhcp });
            }
            "frame" => tr.exchanges.push(Exchange {
                label: rest.trim().to_string(),
                frame: Vec::new(),
                reply: Vec::new(),
            }),
            "none" => {}
            ">" => tr.exchanges.last_mut().unwrap().frame.extend(unhex(rest)),
            "<" => tr.exchanges.last_mut().unwrap().reply.extend(unhex(rest)),
            other => panic!("unknown keyword {other}"),
        }
    }
    tr
}

/// Zeroes the IPv4 identification, which the sender picks, and recomputes
/// the header checksum, so replies compare on everything else.
fn normalize(frame: &[u8]) -> Vec<u8> {
    let mut f = frame.to_vec();
    if f.len() >= 34 && f[12] == 8 && f[13] == 0 {
        f[18] = 0;
        f[19] = 0;
        f[24] = 0;
        f[25] = 0;
        let mut sum = 0u32;
        for pair in f[14..34].chunks(2) {
            sum += u32::from(u16::from_be_bytes([pair[0], pair[1]]));
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        let c = !(sum as u16);
        f[24..26].copy_from_slice(&c.to_be_bytes());
    }
    f
}

#[test]
fn every_frame_transcript_replays() {
    for path in transcripts() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let tr = parse(&fs::read_to_string(&path).unwrap());
        let (tx, rx) = mpsc::channel();
        let stack = Stack::start(
            StackConfig {
                mac: HOST_MAC,
                ip: Ipv4Addr::new(10, 98, 0, 2),
                prefix: 24,
                addressing: tr.addressing.clone(),
            },
            None,
            move |f| {
                let _ = tx.send(f);
            },
        );
        assert!(!tr.exchanges.is_empty(), "{name}: no exchanges");
        for ex in &tr.exchanges {
            stack.receive(ex.frame.clone());
            let what = format!("{name}: {}", ex.label);
            if ex.reply.is_empty() {
                let got = rx.recv_timeout(Duration::from_millis(300));
                assert!(got.is_err(), "{what}: the host answered {got:02x?}");
                continue;
            }
            let got = rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("{what}: no reply"));
            assert_eq!(
                normalize(&got),
                normalize(&ex.reply),
                "{what}: reply differs"
            );
            let extra = rx.recv_timeout(Duration::from_millis(100));
            assert!(extra.is_err(), "{what}: a second frame followed");
        }
    }
}
