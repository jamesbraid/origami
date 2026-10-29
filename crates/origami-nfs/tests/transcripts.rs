//! Replays every RPC transcript in Instigator's `nfs/testdata/transcripts`
//! against this server, over UDP and over TCP, as Instigator's own Go
//! harness does, and requires every reply to match byte for byte. The
//! fixture and transcripts are read from the instigator submodule; the
//! format is described in Instigator's docs/nfs-shares.md.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use origami_nfs::sharefs::{Options, Share};
use origami_nfs::tree::{Content, MemTree, TreeEntry};
use origami_nfs::{Attr, Ports, Server, Serving, Time};

fn testdata() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../instigator/nfs/testdata");
    assert!(
        p.join("fixture.txt").is_file(),
        "{} is missing: run `git submodule update --init instigator`",
        p.display()
    );
    p
}

/// The ports portmap advertises in every transcript, on both transports.
/// The server binds ephemeral ports, so these are only the values the
/// replies carry.
const TRANSCRIPT_PORTS: Ports = Ports {
    portmap: 111,
    mount: 635,
    nfs: 2049,
};

struct Exchange {
    label: String,
    call: Vec<u8>,
    reply: Vec<u8>,
}

struct Transcript {
    fixture: String,
    exchanges: Vec<Exchange>,
}

fn unhex(rest: &str, line: usize) -> Vec<u8> {
    let digits: String = rest.split_whitespace().collect();
    assert!(
        digits.len().is_multiple_of(2),
        "line {line}: odd number of hex digits"
    );
    (0..digits.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&digits[i..i + 2], 16)
                .unwrap_or_else(|_| panic!("line {line}: bad hex"))
        })
        .collect()
}

fn parse_transcript(text: &str) -> Transcript {
    let mut tr = Transcript {
        fixture: String::new(),
        exchanges: Vec::new(),
    };
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "fixture" => tr.fixture = rest.trim().to_string(),
            "call" => tr.exchanges.push(Exchange {
                label: rest.trim().to_string(),
                call: Vec::new(),
                reply: Vec::new(),
            }),
            ">" | "<" => {
                let ex = tr
                    .exchanges
                    .last_mut()
                    .unwrap_or_else(|| panic!("line {n}: bytes before any call"));
                let b = unhex(rest, n);
                if key == ">" {
                    ex.call.extend(b);
                } else {
                    ex.reply.extend(b);
                }
            }
            _ => panic!("line {n}: unknown keyword {key:?}"),
        }
    }
    tr
}

fn fixture_entries(dir: &Path) -> Vec<TreeEntry> {
    let text = fs::read_to_string(dir.join("fixture.txt")).unwrap();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        assert!(f.len() >= 4, "fixture: short line {line:?}");
        let mode = u32::from_str_radix(f[2], 8).unwrap();
        let mtime: i64 = f[3].parse().unwrap();
        let content = match f[0] {
            "dir" => Content::Dir,
            "file" => Content::File(fixture_content(f[4])),
            "link" => Content::Link(f[4].as_bytes().to_vec()),
            k => panic!("fixture: unknown kind {k:?}"),
        };
        out.push(TreeEntry {
            path: f[1].to_string(),
            mode,
            mtime,
            content,
        });
    }
    out
}

fn fixture_content(spec: &str) -> Vec<u8> {
    let (kind, arg) = spec.split_once(':').unwrap_or((spec, ""));
    match kind {
        "hex" => unhex(arg, 0),
        "pattern" => (0..arg.parse::<usize>().unwrap())
            .map(|i| (i % 251) as u8)
            .collect(),
        _ => panic!("fixture: unknown content {spec:?}"),
    }
}

/// Stands in, in share transcripts, for every time the host chose rather
/// than the fixture or a client.
const HOST_TIME: Time = Time {
    sec: 2_000_000_000,
    nsec: 0,
};

/// Divides times: the fixture's are in 1999 and a client's in 2001, so
/// anything after it came from the host's clock.
const HOST_EPOCH: i64 = 1_262_304_000; // 2010-01-01

/// Makes a share's attributes the same on every host. A modification time
/// after the epoch becomes the host time, as every change time does. The
/// access time is shown as the modification time, since whether a read
/// updates it depends on how the host mounts its disk. A directory's size
/// and link count, and every file's storage use, depend on the host
/// filesystem and are replaced.
fn pin_host_attrs(a: &mut Attr) {
    if a.mtime.sec > HOST_EPOCH || (a.mtime.sec == HOST_EPOCH && a.mtime.nsec > 0) {
        a.mtime = HOST_TIME;
    }
    a.atime = a.mtime;
    a.ctime = HOST_TIME;
    if a.mode & 0o170000 == 0o040000 {
        a.size = 4096;
        a.nlink = 2;
    }
    a.used = (a.size + 4095) & !4095;
}

/// Writes the fixture to a temporary directory and exports it as the
/// share "share", with a path table so its handles and fileids don't
/// depend on the host's inode numbers.
fn share_fixture(testdata: &Path, base: &Path) -> Share {
    let dir = base.join("share");
    let mut tree = fixture_entries(testdata);
    tree.sort_by(|a, b| a.path.cmp(&b.path)); // parents before children
    let host_path = |p: &str| if p == "." { dir.clone() } else { dir.join(p) };
    for e in &tree {
        let host = host_path(&e.path);
        match &e.content {
            Content::Dir => fs::create_dir_all(&host).unwrap(),
            #[cfg(unix)]
            Content::Link(t) => {
                std::os::unix::fs::symlink(String::from_utf8_lossy(t).as_ref(), &host).unwrap();
            }
            #[cfg(not(unix))]
            Content::Link(_) => {}
            Content::File(d) => fs::write(&host, d).unwrap(),
        }
    }
    // Modes and times last, deepest first, since adding an entry to a
    // directory changes its modification time.
    for e in tree.iter().rev() {
        if matches!(e.content, Content::Link(_)) {
            continue;
        }
        let host = host_path(&e.path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&host, fs::Permissions::from_mode(e.mode)).unwrap();
        }
        let t = Time::new(e.mtime, 0).to_system();
        let f = fs::OpenOptions::new()
            .read(true)
            .open(&host)
            .or_else(|_| fs::File::open(&host))
            .unwrap();
        f.set_times(fs::FileTimes::new().set_accessed(t).set_modified(t))
            .unwrap();
    }
    Share::open(
        "share",
        &dir,
        Options {
            path_table: Some(base.join("handles")),
            attr_filter: Some(Box::new(pin_host_attrs)),
            ..Options::default()
        },
    )
    .unwrap()
}

/// One server for one transcript, listening on UDP and TCP.
struct TestServer {
    serving: Serving,
    udp: std::net::SocketAddr,
    tcp: std::net::SocketAddr,
    conn: Option<TcpStream>,
    _base: tempdir::Dir,
}

impl TestServer {
    fn start(testdata: &Path, fixture: &str) -> TestServer {
        let base = tempdir::Dir::new();
        let server = match fixture {
            "tree" => Server::new(Arc::new(MemTree::new(fixture_entries(testdata)))),
            "share" => Server::new(Arc::new(share_fixture(testdata, base.path())))
                .with_verifier(*b"verifier"),
            f => panic!("unknown fixture {f:?}"),
        };
        server.set_ports(TRANSCRIPT_PORTS, TRANSCRIPT_PORTS);
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (ua, ta) = (udp.local_addr().unwrap(), tcp.local_addr().unwrap());
        let serving = Serving::start(Arc::new(server), Some(udp), Some(tcp)).unwrap();
        TestServer {
            serving,
            udp: ua,
            tcp: ta,
            conn: None,
            _base: base,
        }
    }

    /// Sends one call over UDP from a socket of its own, as the Go harness
    /// does with a fresh `net.Dial` per call, and returns the reply.
    fn round_trip(&self, call: &[u8]) -> Vec<u8> {
        let c = UdpSocket::bind("127.0.0.1:0").unwrap();
        c.connect(self.udp).unwrap();
        origami_nfs::raise_udp_buffers(&c).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.send(call).unwrap();
        let mut buf = vec![0u8; 65536];
        let n = c.recv(&mut buf).expect("no reply");
        buf.truncate(n);
        buf
    }

    /// Sends one call on the transcript's TCP connection, record marked,
    /// and returns the reply with its mark checked and removed.
    fn round_trip_tcp(&mut self, call: &[u8]) -> Vec<u8> {
        let tcp = self.tcp;
        let c = self.conn.get_or_insert_with(|| {
            let c = TcpStream::connect(tcp).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            c
        });
        c.write_all(&(0x8000_0000u32 | call.len() as u32).to_be_bytes())
            .unwrap();
        c.write_all(call).unwrap();
        let mut hdr = [0u8; 4];
        c.read_exact(&mut hdr).expect("no reply");
        let h = u32::from_be_bytes(hdr);
        assert!(
            h & 0x8000_0000 != 0,
            "reply fragment not marked last: {h:#x}"
        );
        let mut reply = vec![0u8; (h & 0x7fff_ffff) as usize];
        c.read_exact(&mut reply).expect("short reply");
        reply
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.conn = None;
        self.serving.stop();
    }
}

/// A temporary directory removed on drop.
mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    pub struct Dir(PathBuf);

    impl Dir {
        pub fn new() -> Dir {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "origami-nfs-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn every_transcript_replays_byte_for_byte() {
    let dir = testdata();
    let mut files: Vec<PathBuf> = fs::read_dir(dir.join("transcripts"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no transcripts in {}", dir.display());

    let mut failures = Vec::new();
    let mut passed = 0;
    for file in &files {
        let name = file.file_stem().unwrap().to_string_lossy().to_string();
        let tr = parse_transcript(&fs::read_to_string(file).unwrap());
        // The share fixture holds symlinks and Unix modes, which a Windows
        // host can't make (symlinks need a privilege, and a file there has
        // one permission bit), so its replies can't match the transcript.
        if cfg!(windows) && tr.fixture == "share" {
            eprintln!("SKIP {name}: the share fixture needs a Unix host");
            continue;
        }
        for transport in ["udp", "tcp"] {
            let mut ts = TestServer::start(&dir, &tr.fixture);
            let mut bad = None;
            for ex in &tr.exchanges {
                let got = if transport == "udp" {
                    ts.round_trip(&ex.call)
                } else {
                    ts.round_trip_tcp(&ex.call)
                };
                if got != ex.reply {
                    bad = Some(format!(
                        "{name} {transport} {:?}:\n got {}\nwant {}",
                        ex.label,
                        hexs(&got),
                        hexs(&ex.reply)
                    ));
                    break;
                }
            }
            match bad {
                None => {
                    passed += 1;
                    eprintln!(
                        "PASS {name} over {transport} ({} exchanges)",
                        tr.exchanges.len()
                    );
                }
                Some(m) => failures.push(m),
            }
        }
    }
    eprintln!("{passed} replays passed, {} transcripts", files.len());
    assert!(
        failures.is_empty(),
        "{} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn transcript_parser_reads_the_documented_format() {
    let tr = parse_transcript("# c\nfixture tree\n\ncall one\n> 00000001 0002\n> 03\n< ff\n");
    assert_eq!(tr.fixture, "tree");
    assert_eq!(tr.exchanges.len(), 1);
    let ex = &tr.exchanges[0];
    assert_eq!(ex.label, "one");
    assert_eq!(ex.call, [0, 0, 0, 1, 0, 2, 3]);
    assert_eq!(ex.reply, [0xff]);
}
