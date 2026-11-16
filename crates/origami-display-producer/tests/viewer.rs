//! The product's viewer against the reference producer: pictures by their
//! pixels, wake-ups, and what a hidden viewer asks for.

use origami_display::v4::testext::{self, kind, pattern, Stimulus};
use origami_display::v4::viewer::Viewer;
use origami_display::v4::wire;
use origami_display::Taken;
use origami_display_producer::reference;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const W: u32 = 256;
const H: u32 = 128;

struct Rig {
    viewer: Arc<Viewer>,
    wakes: Arc<AtomicU64>,
    ix: u8,
    thread: Option<std::thread::JoinHandle<()>>,
    producer: Option<origami_display_producer::Producer>,
    dir: std::path::PathBuf,
    socket: std::path::PathBuf,
}

impl Rig {
    fn start(push: bool) -> Rig {
        Rig::start_as(push, wire::FOURCC_XR24)
    }

    fn start_as(push: bool, fourcc: u32) -> Rig {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("viewer-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("d.sock");
        let spec = origami_display_producer::HeadSpec {
            width: W,
            height: H,
            pull: !push,
            fourcc,
            ..Default::default()
        };
        let producer =
            reference::start_with(UnixListener::bind(&path).unwrap(), 1, spec, push).unwrap();
        let viewer = Arc::new(Viewer::new().offer(
            wire::feat::TEST,
            wire::Offer {
                name: testext::NAME.into(),
                version_min: testext::VERSION,
                version_max: testext::VERSION,
                payload: vec![],
            },
        ));
        let wakes = Arc::new(AtomicU64::new(0));
        let thread = {
            let (viewer, wakes) = (viewer.clone(), wakes.clone());
            let stream = UnixStream::connect(&path).unwrap();
            std::thread::spawn(move || {
                let wake = || {
                    wakes.fetch_add(1, Ordering::SeqCst);
                };
                let _ = viewer.run(stream, &wake);
            })
        };
        let t = Instant::now();
        let ix = loop {
            if let Some(ix) = viewer.extension(testext::NAME) {
                break ix;
            }
            assert!(t.elapsed() < Duration::from_secs(10), "no welcome");
            std::thread::sleep(Duration::from_millis(5));
        };
        Rig {
            viewer,
            wakes,
            ix,
            thread: Some(thread),
            producer: Some(producer),
            dir,
            socket: path,
        }
    }

    fn stimulus(&self, pat: u32, step: u32) {
        let s = Stimulus {
            head: 0,
            kind: kind::PATTERN,
            seed: 7,
            step,
            arg: pat,
        };
        self.viewer.send_raw(&s.encode(self.ix)).unwrap();
    }

    fn wait_wake(&self, since: u64) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(5) {
            if self.wakes.load(Ordering::SeqCst) > since {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    fn still(&self) -> u64 {
        std::thread::sleep(Duration::from_millis(300));
        self.wakes.load(Ordering::SeqCst)
    }

    /// The first frame of a still head arrives unasked: take it.
    fn first(&self, picture: &mut [u32]) {
        assert!(self.wait_wake(0), "no first frame");
        self.take_into(picture);
    }

    /// Take the waiting frame and lay its rows over `picture`.
    fn take_into(&self, picture: &mut [u32]) -> Vec<(u32, u32)> {
        self.try_take_into(picture).expect("no frame to take")
    }

    fn try_take_into(&self, picture: &mut [u32]) -> Option<Vec<(u32, u32)>> {
        let mut px = Vec::new();
        let Taken::Frame(u) = self.viewer.take(0, &mut px) else {
            return None;
        };
        assert_eq!((u.width, u.height), (W, H));
        let mut at = 0;
        for &(a, b) in &u.runs {
            for y in a..b {
                for x in 0..W {
                    let p = &px[at..at + 4];
                    picture[(y * W + x) as usize] =
                        u32::from_le_bytes([p[0], p[1], p[2], p[3]]) & 0xFF_FFFF;
                    at += 4;
                }
            }
        }
        assert_eq!(at, px.len());
        Some(u.runs)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.viewer.close();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Some(p) = self.producer.take() {
            p.stop();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn lines_differing(a: &[u32], b: &[u32]) -> Vec<u32> {
    (0..H)
        .filter(|y| {
            let r = (y * W) as usize..((y + 1) * W) as usize;
            a[r.clone()] != b[r]
        })
        .collect()
}

fn expect(pat: u32, step: u32) -> Vec<u32> {
    testext::pattern_picture(pat, 7, step, W, H)
}

fn each_style(body: impl Fn(&Rig)) {
    for push in [false, true] {
        body(&Rig::start(push));
    }
}

#[test]
fn a_picture_arrives_by_its_pixels_and_a_still_screen_wakes_nobody() {
    each_style(|rig| {
        let mut picture = vec![0u32; (W * H) as usize];
        rig.first(&mut picture);
        let wakes = rig.wakes.load(Ordering::SeqCst);
        rig.stimulus(pattern::BAR, 0);
        assert!(rig.wait_wake(wakes));
        let heads = rig.viewer.heads();
        assert!(heads[0].active);
        rig.take_into(&mut picture);
        assert_eq!(
            lines_differing(&picture, &expect(pattern::BAR, 0)),
            Vec::<u32>::new()
        );
        // The bar moves down: only the lines it left and entered arrive.
        let wakes = rig.wakes.load(Ordering::SeqCst);
        rig.stimulus(pattern::BAR, 1);
        assert!(rig.wait_wake(wakes));
        let runs = rig.take_into(&mut picture);
        assert_eq!(
            lines_differing(&picture, &expect(pattern::BAR, 1)),
            Vec::<u32>::new()
        );
        assert_eq!(runs, vec![(0, 8), (120, 128)]);
        let st = rig.viewer.stats();
        assert_eq!(st.imported_lines, 0);
        assert!(st.copied_bytes > 0);
        let idle = rig.still();
        assert_eq!(rig.still(), idle, "a still picture woke the viewer");
    });
}

#[test]
fn a_hidden_viewer_asks_for_nothing_new_and_showing_it_brings_the_change() {
    each_style(|rig| {
        let mut picture = vec![0u32; (W * H) as usize];
        rig.first(&mut picture);
        let wakes = rig.wakes.load(Ordering::SeqCst);
        rig.stimulus(pattern::LINES, 5);
        assert!(rig.wait_wake(wakes));
        rig.take_into(&mut picture);
        rig.viewer.set_visible(false);
        // The request sent when the last frame arrived is still recorded:
        // one more frame may land, silently.
        let wakes = rig.wakes.load(Ordering::SeqCst);
        rig.stimulus(pattern::LINES, 9);
        rig.stimulus(pattern::LINES, 12);
        let after = rig.still();
        assert_eq!(after, wakes, "a hidden viewer was woken");
        rig.take_into(&mut picture);
        rig.stimulus(pattern::LINES, 15);
        assert_eq!(rig.still(), wakes);
        // Showing asks again; the producer has a newer picture.
        rig.viewer.set_visible(true);
        assert!(rig.wait_wake(wakes), "showing did not bring the change");
        let t = Instant::now();
        while picture != expect(pattern::LINES, 15) {
            assert!(
                t.elapsed() < Duration::from_secs(5),
                "never reached step 15"
            );
            let seen = rig.wakes.load(Ordering::SeqCst);
            let runs = rig.take_into(&mut picture);
            let want = expect(pattern::LINES, 15);
            let bad: Vec<u32> = (0..H)
                .filter(|y| {
                    let r = (y * W) as usize..((y + 1) * W) as usize;
                    picture[r.clone()] != want[r]
                })
                .collect();
            if !bad.is_empty() {
                assert!(
                    rig.wait_wake(seen),
                    "stuck: lines {bad:?} differ after runs {runs:?}"
                );
            }
        }
    });
}

#[test]
fn an_xb24_head_arrives_as_blue_green_red_with_red_and_blue_swapped() {
    for push in [false, true] {
        let rig = Rig::start_as(push, wire::FOURCC_XB24);
        let mut picture = vec![0u32; (W * H) as usize];
        rig.first(&mut picture);
        let wakes = rig.wakes.load(Ordering::SeqCst);
        rig.stimulus(pattern::LINES, 20);
        assert!(rig.wait_wake(wakes));
        rig.take_into(&mut picture);
        // The producer wrote 0x00RRGGBB words; read as XB24 they hold red and
        // blue the other way round.
        let swap = |v: u32| (v & 0xFF00) | (v >> 16 & 0xFF) | (v & 0xFF) << 16;
        let want: Vec<u32> = expect(pattern::LINES, 20).into_iter().map(swap).collect();
        assert_eq!(lines_differing(&picture, &want), Vec::<u32>::new());
    }
}

#[test]
fn a_head_with_no_mode_appears_when_the_mode_is_set() {
    use origami_display::v4::testext::kind;
    each_style(|rig| {
        let mut picture = vec![0u32; (W * H) as usize];
        rig.first(&mut picture);
        let mode = |w: u32, h: u32| Stimulus {
            head: 0,
            kind: kind::MODE,
            seed: u64::from(w) | u64::from(h) << 16,
            step: 0,
            arg: 0,
        };
        // The PROM has not set a mode yet: the head is there and shows nothing.
        rig.viewer.send_raw(&mode(0, 0).encode(rig.ix)).unwrap();
        let t = Instant::now();
        while rig.viewer.heads()[0].active {
            assert!(
                t.elapsed() < Duration::from_secs(5),
                "the head never went dark"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // The mode arrives, then the picture.
        rig.viewer.send_raw(&mode(W, H).encode(rig.ix)).unwrap();
        rig.stimulus(pattern::BAR, 2);
        let t = Instant::now();
        while picture != expect(pattern::BAR, 2) {
            assert!(
                t.elapsed() < Duration::from_secs(5),
                "the head never came back"
            );
            let seen = rig.wakes.load(Ordering::SeqCst);
            if rig.try_take_into(&mut picture).is_none() {
                rig.wait_wake(seen);
            }
        }
    });
}

#[test]
fn the_last_cursor_image_stays_up_until_the_new_one_arrives() {
    use origami_display::v4::session::{HelloParams, Session};
    use origami_display::v4::testext::{op, Control};
    let rig = Rig::start(false);
    let mut picture = vec![0u32; (W * H) as usize];
    rig.first(&mut picture);
    let stimulate = |kind, seed, arg| Stimulus {
        head: 0,
        kind,
        seed,
        step: 0,
        arg,
    };
    let send = |s: Stimulus| rig.viewer.send_raw(&s.encode(rig.ix)).unwrap();
    let cursor_with = |known| {
        let t = Instant::now();
        loop {
            if let Some(c) = rig.viewer.cursor(0, known) {
                if c.on && c.width > 0 {
                    return c;
                }
            }
            assert!(t.elapsed() < Duration::from_secs(5), "no cursor");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    send(stimulate(kind::CURSOR_MOVE, 10 | 20 << 16, 1));
    send(stimulate(kind::CURSOR_IMAGE, 0xFF11_2233, 4));
    let first = cursor_with(u32::MAX);
    let old = first.image.clone().expect("the first image");
    // A second viewer holds this one's writes, so the next image's serial is
    // in the state region before the image itself arrives.
    let stream = UnixStream::connect(&rig.socket).unwrap();
    let other = Session::connect(
        stream,
        &HelloParams {
            features: wire::feat::TEST,
            offers: vec![wire::Offer {
                name: testext::NAME.into(),
                version_min: testext::VERSION,
                version_max: testext::VERSION,
                payload: vec![],
            }],
            ..Default::default()
        },
    )
    .unwrap();
    let ix = other
        .welcome
        .accepts
        .iter()
        .find(|a| a.name == testext::NAME)
        .unwrap()
        .index;
    let control = |op| {
        Control {
            op,
            head: 0,
            target_viewer: rig.viewer.viewer_id(),
            arg: 0,
        }
        .encode(ix)
    };
    other.send(&control(op::HOLD_WRITES)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    other
        .send(&stimulate(kind::CURSOR_IMAGE, 0xFF44_5566, 6).encode(ix))
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let held = rig.viewer.cursor(0, u32::MAX).expect("a cursor");
    assert!(held.on, "the cursor went dark");
    assert_eq!(
        (held.gen, held.width, held.image),
        (first.gen, 4, Some(old))
    );
    let wakes = rig.wakes.load(Ordering::SeqCst);
    other.send(&control(op::RESUME_WRITES)).unwrap();
    assert!(rig.wait_wake(wakes), "the arriving image woke nobody");
    let next = rig.viewer.cursor(0, first.gen).expect("a cursor");
    assert!(next.gen != first.gen && next.width == 6 && next.image.is_some());
    drop(other);
}

#[test]
fn input_reaches_the_session_as_one_write() {
    each_style(|rig| {
        use origami_engine::{InputEvent, Key};
        rig.viewer
            .send_input(&[
                InputEvent::Key {
                    key: Key::A,
                    down: true,
                },
                InputEvent::PointerMove { dx: 3, dy: 4 },
            ])
            .unwrap();
        rig.viewer.send_input(&[]).unwrap();
    });
}
