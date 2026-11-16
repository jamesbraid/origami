//! The display protocol 4 conformance suite against the iris host, with synthetic
//! boards presenting on their own threads through the runner's `IrisHost` and `Presenter`.

#![cfg(target_os = "linux")]

use display_conformance::client::Target;
use display_conformance::{run, Outcome};
use origami_display::v4::testext::{self, kind, pattern, Stimulus};
use origami_display::v4::wire;
use origami_display_producer::{Config, Handle, HeadSpec, Host, InputEvent, Marks, Producer};
use origami_iris::display::{Board, Hands, IrisHost, Presenter};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Where iris keeps a composed frame: rows 2048 pixels apart.
const STRIDE: usize = 2048;
/// Frames between heartbeat presents, as iris's idle floor.
const HEARTBEAT: u32 = 6;

#[derive(Clone, Copy)]
struct Pic {
    pat: u32,
    seed: u64,
    step: u32,
    version: u64,
}

struct FakeBoard {
    pic: Mutex<Pic>,
    forced: AtomicBool,
    delay_us: Mutex<u32>,
    stop: AtomicBool,
}

impl Board for FakeBoard {
    fn request_frame(&self) {
        self.forced.store(true, Ordering::Release);
    }
}

struct NoHands;

impl Hands for NoHands {
    fn key(&mut self, _: u32, _: bool) {}
    fn buttons(&mut self, _: u8) {}
    fn nudge(&mut self, _: i32, _: i32) {}
    fn wheel(&mut self, _: i32) {}
}

/// The board's display thread: present on a change, a request, or the
/// heartbeat, and compose only when the presenter asks.
fn display_thread(
    board: Arc<FakeBoard>,
    head: u32,
    handle: Arc<OnceLock<Handle>>,
    mut p: Presenter,
) {
    let mut buf = vec![0u32; STRIDE * 1024];
    let (mut seen, mut idle) = (0, 0);
    while !board.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(3));
        let Some(handle) = handle.get() else { continue };
        let (w, h) = handle.with(|e| e.head_size(head));
        if w == 0 || h == 0 || h > 1024 || w as usize > STRIDE {
            continue;
        }
        let pic = *board.pic.lock().unwrap();
        let changed = pic.version != seen;
        idle += 1;
        if !(changed || board.forced.swap(false, Ordering::AcqRel) || idle >= HEARTBEAT) {
            continue;
        }
        (seen, idle) = (pic.version, 0);
        if let Some(t) = p.begin((w, h), changed) {
            let mut row = vec![0u32; w as usize];
            for y in 0..h {
                testext::pattern_line(pic.pat, pic.seed, pic.step, h, y, &mut row);
                for (d, &v) in buf[y as usize * STRIDE..][..w as usize]
                    .iter_mut()
                    .zip(&row)
                {
                    // 0x00RRGGBB as iris's 0xAABBGGRR words.
                    *d = 0xFF00_0000 | (v & 0xFF) << 16 | v & 0xFF00 | v >> 16 & 0xFF;
                }
            }
            p.finish(t, Some((&buf, STRIDE)));
        }
    }
}

/// The real host, plus the suite's hooks into the fake boards.
struct TestHost {
    inner: IrisHost,
    boards: Arc<Mutex<Vec<Arc<FakeBoard>>>>,
}

impl Host for TestHost {
    fn attach(&mut self, head: u32, marks: Arc<Marks>) {
        self.inner.attach(head, marks);
    }
    fn request_frame(&mut self, head: u32) {
        self.inner.request_frame(head);
    }
    fn inject(&mut self, ev: &InputEvent) {
        self.inner.inject(ev);
    }
    fn stimulus(&mut self, s: &Stimulus) {
        let Some(board) = self.boards.lock().unwrap().get(s.head as usize).cloned() else {
            return;
        };
        match s.kind {
            kind::PATTERN => {
                let delay = std::mem::take(&mut *board.delay_us.lock().unwrap());
                let (pat, seed, step) = (s.arg, s.seed, s.step);
                let apply = move || {
                    let mut pic = board.pic.lock().unwrap();
                    (pic.pat, pic.seed, pic.step) = (pat, seed, step);
                    pic.version += 1;
                };
                if delay == 0 {
                    apply();
                } else {
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_micros(delay as u64));
                        apply();
                    });
                }
            }
            kind::MODE => {
                let mut pic = board.pic.lock().unwrap();
                *pic = Pic {
                    pat: pattern::SOLID,
                    seed: 0,
                    step: 0,
                    version: pic.version + 1,
                };
            }
            _ => {}
        }
    }
    fn delay_next_mark(&mut self, head: u32, micros: u32) {
        if let Some(board) = self.boards.lock().unwrap().get(head as usize) {
            *board.delay_us.lock().unwrap() = micros;
        }
    }
}

/// An iris-shaped producer on `path`: push heads in iris's pixel order.
fn start(path: &std::path::Path, w: u32, h: u32) -> (Producer, Arc<Mutex<Vec<Arc<FakeBoard>>>>) {
    let spec = HeadSpec {
        width: w,
        height: h,
        label: "iris".into(),
        pointer_mode: wire::POINTER_RELATIVE,
        pull: false,
        fourcc: wire::FOURCC_XB24,
        ..Default::default()
    };
    let boards: Arc<Mutex<Vec<Arc<FakeBoard>>>> = Default::default();
    let handle: Arc<OnceLock<Handle>> = Default::default();
    let attach = {
        let (boards, handle) = (boards.clone(), handle.clone());
        Box::new(move |head: u32, presenter: Presenter| {
            let board = Arc::new(FakeBoard {
                pic: Mutex::new(Pic {
                    pat: pattern::SOLID,
                    seed: 0,
                    step: 0,
                    version: 0,
                }),
                forced: AtomicBool::new(false),
                delay_us: Mutex::new(0),
                stop: AtomicBool::new(false),
            });
            boards.lock().unwrap().push(board.clone());
            let (b, hd) = (board.clone(), handle.clone());
            std::thread::spawn(move || display_thread(b, head, hd, presenter));
            Some(board as Arc<dyn Board>)
        })
    };
    let host = TestHost {
        inner: IrisHost::new(handle.clone(), Box::new(NoHands), attach),
        boards: boards.clone(),
    };
    let cfg = Config {
        test: true,
        test_head: spec.clone(),
        ..Default::default()
    };
    let p = Producer::start(
        cfg,
        UnixListener::bind(path).unwrap(),
        Box::new(host),
        Producer::waker().unwrap(),
    )
    .unwrap();
    let _ = handle.set(p.handle());
    p.with(|e| e.add_head(spec).unwrap());
    (p, boards)
}

#[test]
fn the_iris_host_passes_the_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("display.sock");
    let (_producer, _boards) = start(&path, 256, 128);
    let report = run(&Target { path }, "", false, |name, r, _| match r {
        Outcome::Fail(why) => eprintln!("FAIL {name}: {why}"),
        Outcome::Skip(why) => eprintln!("SKIP {name}: {why}"),
        Outcome::Pass => eprintln!("PASS {name}"),
    })
    .unwrap();
    let failed: Vec<_> = report
        .results
        .iter()
        .filter_map(|(n, r, _)| match r {
            Outcome::Fail(why) | Outcome::Skip(why) => Some(format!("{n}: {why}")),
            _ => None,
        })
        .collect();
    assert!(failed.is_empty(), "{failed:#?}");
    assert!(
        report.results.len() >= 40,
        "{} checks ran",
        report.results.len()
    );
}

#[test]
fn a_still_board_publishes_nothing_until_its_picture_changes() {
    use origami_display::v4::session::{Event, HelloParams, Session};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("display.sock");
    let (_producer, boards) = start(&path, 64, 32);
    let mut s = Session::connect(
        std::os::unix::net::UnixStream::connect(&path).unwrap(),
        &HelloParams::default(),
    )
    .unwrap();
    let frame = |s: &mut Session, wait: u64| {
        s.request(0, false).unwrap();
        match s.recv(Some(Duration::from_millis(wait))).unwrap() {
            Some(Event::Frame(f)) => Some(f.frame.serial),
            _ => None,
        }
    };
    assert_eq!(frame(&mut s, 2000), Some(1));
    // Many heartbeat presents pass with the board saying nothing changed.
    assert_eq!(frame(&mut s, 400), None);
    {
        let board = boards.lock().unwrap()[0].clone();
        let mut pic = board.pic.lock().unwrap();
        (pic.step, pic.version) = (1, pic.version + 1);
    }
    assert_eq!(frame(&mut s, 2000), Some(2));
}
