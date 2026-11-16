//! The reference producer's hosts: test pictures driven by `qemu.test`
//! stimuli, in the pull style and the push style.

use crate::engine::{Config, HeadSpec, Host, InputEvent};
use crate::lines::Marks;
use crate::server::{Handle, Producer};
use origami_display::v4::testext::{self, kind, pattern, Stimulus};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone, Copy)]
struct Src {
    pat: u32,
    seed: u64,
    step: u32,
    w: u32,
    h: u32,
    delay_us: u32,
}

#[derive(Default)]
struct Pictures {
    srcs: HashMap<u32, Src>,
    marks: HashMap<u32, Arc<Marks>>,
}

impl Pictures {
    fn apply(&mut self, s: &Stimulus) {
        match s.kind {
            kind::PATTERN => {
                if let Some(src) = self.srcs.get_mut(&s.head) {
                    let old = *src;
                    (src.pat, src.seed, src.step) = (s.arg, s.seed, s.step);
                    let delay = std::mem::take(&mut src.delay_us);
                    let marks = self.marks[&s.head].clone();
                    // Mark only the lines whose pixels differ, as a real source would.
                    let (mut a, mut b) = (vec![0u32; src.w as usize], vec![0u32; src.w as usize]);
                    let mut changed = vec![];
                    for y in 0..src.h {
                        testext::pattern_line(old.pat, old.seed, old.step, old.h, y, &mut a);
                        testext::pattern_line(src.pat, src.seed, src.step, src.h, y, &mut b);
                        if a != b {
                            changed.push(y);
                        }
                    }
                    if delay > 0 {
                        let (lo, hi) = (
                            changed.first().copied().unwrap_or(0),
                            changed.last().map_or(0, |y| y + 1),
                        );
                        marks.mark_lines_delayed(lo..hi, Duration::from_micros(delay as u64));
                    } else {
                        for y in changed {
                            marks.set_bit(y);
                        }
                        marks.raise();
                    }
                }
            }
            kind::MODE => {
                if let Some(src) = self.srcs.get_mut(&s.head) {
                    *src = Src {
                        pat: pattern::SOLID,
                        seed: 0,
                        step: 0,
                        w: (s.seed & 0xFFFF) as u32,
                        h: (s.seed >> 16 & 0xFFFF) as u32,
                        delay_us: 0,
                    };
                    self.marks[&s.head].mark_all();
                }
            }
            _ => {}
        }
    }

    fn line(&self, head: u32, y: u32, row: &mut [u32]) {
        if let Some(s) = self.srcs.get(&head) {
            testext::pattern_line(s.pat, s.seed, s.step, s.h, y, &mut row[..s.w as usize]);
        }
    }
}

/// Pull style: the producer asks for the lines the stimulus marked.
pub struct PullHost {
    pics: Pictures,
    sizes: HashMap<u32, (u32, u32)>,
    pub events: Arc<Mutex<Vec<InputEvent>>>,
}

impl PullHost {
    pub fn new() -> PullHost {
        PullHost {
            pics: Pictures::default(),
            sizes: HashMap::new(),
            events: Default::default(),
        }
    }
}

impl Default for PullHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Host for PullHost {
    fn attach(&mut self, head: u32, marks: Arc<Marks>) {
        self.pics.marks.insert(head, marks);
        let (w, h) = self.sizes.get(&head).copied().unwrap_or((256, 128));
        self.pics.srcs.insert(
            head,
            Src {
                pat: pattern::SOLID,
                seed: 0,
                step: 0,
                w,
                h,
                delay_us: 0,
            },
        );
    }
    fn resolve_line(&mut self, head: u32, y: u32, row: &mut [u32]) {
        self.pics.line(head, y, row);
    }
    fn stimulus(&mut self, s: &Stimulus) {
        self.pics.apply(s);
    }
    fn delay_next_mark(&mut self, head: u32, micros: u32) {
        if let Some(s) = self.pics.srcs.get_mut(&head) {
            s.delay_us = micros;
        }
    }
    fn inject(&mut self, ev: &InputEvent) {
        self.events.lock().unwrap().push(ev.clone());
    }
}

struct PushShared {
    pics: Mutex<Pictures>,
    asked: Mutex<Vec<u32>>,
    cv: Condvar,
    stop: AtomicBool,
    handle: OnceLock<Handle>,
}

/// Push style: a thread of the host's own composes when asked and hands the
/// whole picture over, as iris's renderer does.
pub struct PushHost {
    shared: Arc<PushShared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PushHost {
    pub fn new() -> PushHost {
        let shared = Arc::new(PushShared {
            pics: Mutex::new(Pictures::default()),
            asked: Mutex::new(vec![]),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            handle: OnceLock::new(),
        });
        let s2 = shared.clone();
        let thread = std::thread::spawn(move || compose_loop(s2));
        PushHost {
            shared,
            thread: Some(thread),
        }
    }

    /// Give the composing thread the producer's handle once it exists.
    pub fn connect(&self, h: Handle) {
        let _ = self.shared.handle.set(h);
    }
}

impl Default for PushHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PushHost {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.cv.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn compose_loop(sh: Arc<PushShared>) {
    loop {
        let heads: Vec<u32> = {
            let mut a = sh.asked.lock().unwrap();
            while a.is_empty() && !sh.stop.load(Ordering::SeqCst) {
                a = sh.cv.wait(a).unwrap();
            }
            if sh.stop.load(Ordering::SeqCst) {
                return;
            }
            std::mem::take(&mut *a)
        };
        let Some(handle) = sh.handle.get() else {
            continue;
        };
        for h in heads {
            let Some((id, _)) = handle.with(|e| e.push_begin(h)) else {
                continue;
            };
            // Composition reads the source after the producer cleared its marks.
            let pic = {
                let p = sh.pics.lock().unwrap();
                p.srcs.get(&h).map(|s| {
                    let stride = (s.w as usize * 4).div_ceil(256) * 256;
                    let mut buf = vec![0u8; stride * s.h as usize];
                    let mut row = vec![0u32; s.w as usize];
                    for y in 0..s.h {
                        p.line(h, y, &mut row);
                        for (x, px) in row.iter().enumerate() {
                            buf[y as usize * stride + x * 4..][..4]
                                .copy_from_slice(&px.to_le_bytes());
                        }
                    }
                    buf
                })
            };
            handle.with(|e| e.push_complete(h, id, pic.as_deref()));
        }
    }
}

impl Host for PushHost {
    fn attach(&mut self, head: u32, marks: Arc<Marks>) {
        let mut p = self.shared.pics.lock().unwrap();
        p.marks.insert(head, marks);
        p.srcs.insert(
            head,
            Src {
                pat: pattern::SOLID,
                seed: 0,
                step: 0,
                w: 256,
                h: 128,
                delay_us: 0,
            },
        );
    }
    fn request_frame(&mut self, head: u32) {
        let mut a = self.shared.asked.lock().unwrap();
        if !a.contains(&head) {
            a.push(head);
        }
        self.shared.cv.notify_one();
    }
    fn stimulus(&mut self, s: &Stimulus) {
        self.shared.pics.lock().unwrap().apply(s);
    }
    fn delay_next_mark(&mut self, head: u32, micros: u32) {
        if let Some(s) = self.shared.pics.lock().unwrap().srcs.get_mut(&head) {
            s.delay_us = micros;
        }
    }
}

/// Start the reference producer on `listener`: `heads` test heads of
/// `w` by `h`, the test extension on, pulling or pushing.
pub fn start(
    listener: std::os::unix::net::UnixListener,
    heads: u32,
    w: u32,
    h: u32,
    push: bool,
) -> std::io::Result<Producer> {
    let spec = HeadSpec {
        width: w,
        height: h,
        pull: !push,
        ..Default::default()
    };
    let cfg = Config {
        test: true,
        test_head: spec.clone(),
        ..Default::default()
    };
    let waker = Producer::waker()?;
    let p = if push {
        let host = PushHost::new();
        let shared = host.shared.clone();
        let p = Producer::start(cfg, listener, Box::new(host), waker)?;
        let _ = shared.handle.set(p.handle());
        p
    } else {
        let mut host = PullHost::new();
        host.sizes.insert(0, (w, h));
        Producer::start(cfg, listener, Box::new(host), waker)?
    };
    p.with(|e| {
        for _ in 0..heads {
            e.add_head(spec.clone()).expect("head capacity");
        }
    });
    Ok(p)
}
