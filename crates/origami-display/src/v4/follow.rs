//! Follow mode for a relative pointer: one PS/2 packet at a time toward the
//! host pointer, each sized from where the guest drew its cursor after the last.

use super::wire;
use std::collections::VecDeque;
use std::io;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// A packet the guest has not acted on in this long moved nothing. Guests
/// stall for a few hundred ms; a second packet sooner would move twice.
const LOST: Duration = Duration::from_millis(500);
/// A draw sooner than this after a packet (IRIX takes about 10 ms) is its
/// answer only if it moved as modelled; else it is a redraw, such as a new shape.
const TOO_SOON: Duration = Duration::from_millis(4);
/// An early draw with nothing after it in this long was a fast guest's answer.
const EARLY_WAIT: Duration = Duration::from_millis(30);
/// How often the cursor is read while an answer is due.
const POLL: Duration = Duration::from_micros(500);
/// How long a click waits for the cursor to arrive.
const HOLD: Duration = Duration::from_millis(150);
/// The most counts one PS/2 packet carries.
const STEP_MAX: i32 = 127;

/// The pixels `n` counts move under the X server's default acceleration
/// (2/1, threshold 4), per axis. IRIX 6.5.30 on a VPro measured exactly this.
pub fn accel(n: i32) -> i32 {
    let a = n.abs();
    n.signum() * if a <= 4 { a } else { 2 * a - 4 }
}

/// The largest count that moves no further than `px`.
fn counts(px: i32) -> i32 {
    let a = px.abs();
    px.signum()
        * if a <= 4 {
            a
        } else {
            ((a + 4) / 2).min(STEP_MAX)
        }
}

/// Where the guest last drew its cursor; `seq` changes with every draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub seq: u32,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug)]
struct Flight {
    d: [i32; 2],
    at: Instant,
    /// The cursor's draw count before this packet's answer.
    seq: u32,
    from: [i32; 2],
    early: bool,
}

/// What a poll asks the caller to send, in order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub packet: Option<(i32, i32)>,
    pub release: Vec<Vec<u8>>,
}

#[derive(Debug)]
pub struct Follow {
    target: Option<[i32; 2]>,
    seen: Option<Cursor>,
    flight: Option<Flight>,
    /// Pixels per modelled pixel on each axis, for a guest that accelerates
    /// otherwise.
    scale: [f64; 2],
    /// The axis is as close as it gets to this target: one count passed it,
    /// or pushing moved nothing.
    close: [bool; 2],
    /// The head's size; a cursor on its edge was stopped there.
    size: [i32; 2],
    held: VecDeque<Vec<u8>>,
    held_since: Option<Instant>,
}

impl Default for Follow {
    fn default() -> Self {
        Follow {
            target: None,
            seen: None,
            flight: None,
            scale: [1.0; 2],
            close: [false; 2],
            size: [i32::MAX; 2],
            held: VecDeque::new(),
            held_since: None,
        }
    }
}

impl Follow {
    /// The host pointer is at `x`, `y` on a head of `w` by `h` pixels.
    pub fn aim(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let t = [x.clamp(0, w - 1), y.clamp(0, h - 1)];
        if self.target != Some(t) {
            self.close = [false; 2];
        }
        self.target = Some(t);
        self.size = [w, h];
    }

    /// Stop following, for relative motion or a guest with no cursor; what
    /// was held is returned to send now.
    pub fn cancel(&mut self) -> Vec<Vec<u8>> {
        self.target = None;
        self.flight = None;
        self.held_since = None;
        self.held.drain(..).collect()
    }

    /// Whether the cursor still has somewhere to go, or a click waits.
    pub fn busy(&self) -> bool {
        self.moving() || !self.held.is_empty()
    }

    fn moving(&self) -> bool {
        if self.flight.is_some() {
            return true;
        }
        match (self.target, self.seen) {
            (Some(_), None) => true,
            (Some(t), Some(c)) => (0..2).any(|i| t[i] != [c.x, c.y][i] && !self.close[i]),
            _ => false,
        }
    }

    /// A button or key message: returned to send now, or kept until the
    /// cursor arrives. Once one waits, everything after it waits too.
    pub fn hold(&mut self, msg: Vec<u8>, button: bool, now: Instant) -> Option<Vec<u8>> {
        if self.held.is_empty() && (!button || !self.moving()) {
            return Some(msg);
        }
        if self.held.is_empty() {
            self.held_since = Some(now);
        }
        self.held.push_back(msg);
        None
    }

    /// Read the guest's cursor and say what to send.
    pub fn poll(&mut self, now: Instant, cursor: Cursor) -> Step {
        self.seen = Some(cursor);
        let c = [cursor.x, cursor.y];
        if let Some(f) = &mut self.flight {
            let since = now - f.at;
            let as_modelled = (0..2)
                .all(|i| c[i] - f.from[i] == (accel(f.d[i]) as f64 * self.scale[i]).round() as i32);
            if cursor.seq != f.seq && !as_modelled && since < TOO_SOON {
                f.seq = cursor.seq;
                f.early = true;
            } else if cursor.seq != f.seq || (f.early && since >= EARLY_WAIT) {
                let f = self.flight.take().unwrap();
                self.learn(&f, cursor);
            } else if since >= LOST {
                // Pushing that way moves nothing, as at the edge of the screen.
                let f = self.flight.take().unwrap();
                self.close = [0, 1].map(|i| self.close[i] || f.d[i] != 0);
            }
        }
        let mut step = Step::default();
        if self.flight.is_none() {
            if let Some(t) = self.target {
                let d = [0, 1].map(|i| {
                    let err = t[i] - [cursor.x, cursor.y][i];
                    let n = counts((err as f64 / self.scale[i]).trunc() as i32);
                    // A guest that moves further than modelled still gets one count.
                    if self.close[i] {
                        0
                    } else if n != 0 {
                        n
                    } else {
                        err.signum()
                    }
                });
                if d != [0, 0] {
                    self.flight = Some(Flight {
                        d,
                        at: now,
                        seq: cursor.seq,
                        from: c,
                        early: false,
                    });
                    step.packet = Some((d[0], d[1]));
                }
            }
        }
        let waited = self.held_since.is_some_and(|s| now - s >= HOLD);
        if !self.held.is_empty() && (!self.moving() || waited) {
            step.release = self.held.drain(..).collect();
            self.held_since = None;
        }
        step
    }

    fn learn(&mut self, f: &Flight, c: Cursor) {
        for (i, got) in [c.x - f.from[0], c.y - f.from[1]].into_iter().enumerate() {
            let want = accel(f.d[i]);
            let err = self.target.map_or(0, |t| t[i] - f.from[i]);
            if f.d[i].abs() == 1 && got.abs() > err.abs() {
                self.close[i] = true;
            }
            let at_edge = [c.x, c.y][i] <= 0 || [c.x, c.y][i] >= self.size[i] - 1;
            // Small, contrary or edge-stopped moves say nothing about the curve.
            if want.abs() >= 4 && got.signum() == want.signum() && !at_edge {
                let r = got as f64 / want as f64;
                self.scale[i] = (0.5 * self.scale[i] + 0.5 * r).clamp(0.25, 4.0);
            }
        }
    }
}

/// One input message for a [`Link`], in the order the user made it.
pub enum Item {
    /// A key, or a button when the flag is set.
    Press(Vec<u8>, bool),
    /// The host pointer at `x`, `y` on a `w` by `h` head; `abs` goes instead
    /// while the guest has drawn no cursor.
    At {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        abs: Vec<u8>,
    },
    /// Relative motion, or anything else that ends following.
    Stop(Vec<u8>),
}

type Source = Box<dyn Fn() -> Option<Cursor> + Send + Sync>;
type Sink = Box<dyn Fn(&[u8]) -> io::Result<()> + Send + Sync>;

struct LinkState {
    follow: Follow,
    head: u32,
    stop: bool,
}

/// Follow mode on a session: a thread reads the guest's cursor while it has
/// somewhere to go, and every input message is sent under one lock, in order.
pub struct Link {
    state: Mutex<LinkState>,
    cv: Condvar,
    cursor: Source,
    sink: Sink,
}

fn reports(msgs: Vec<Vec<u8>>, out: &mut Vec<u8>) {
    for m in msgs {
        out.extend(m);
        out.extend(wire::sync(0, 0));
    }
}

impl Link {
    pub fn new(cursor: Source, sink: Sink) -> Link {
        Link {
            state: Mutex::new(LinkState {
                follow: Follow::default(),
                head: 0,
                stop: false,
            }),
            cv: Condvar::new(),
            cursor,
            sink,
        }
    }

    fn lock(&self) -> MutexGuard<'_, LinkState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where the guest last drew its cursor, once it has drawn one.
    pub fn cursor(&self) -> Option<Cursor> {
        (self.cursor)()
    }

    /// Send `items` for `head` as one report, behind anything still held.
    pub fn send(&self, head: u32, items: Vec<Item>) -> io::Result<()> {
        let mut st = self.lock();
        st.head = head;
        let mut out = Vec::new();
        let cursor = self.cursor();
        if cursor.is_none() {
            reports(st.follow.cancel(), &mut out);
        }
        let mut tail = Vec::new();
        let now = Instant::now();
        for item in items {
            match item {
                Item::Press(m, button) if cursor.is_some() => {
                    tail.extend(st.follow.hold(m, button, now).unwrap_or_default())
                }
                Item::Press(m, _) => tail.extend(m),
                Item::At { x, y, w, h, .. } if cursor.is_some() => st.follow.aim(x, y, w, h),
                Item::At { abs, .. } => tail.extend(abs),
                Item::Stop(m) => {
                    // What was sent before anything was held is older than it.
                    if !tail.is_empty() {
                        reports(vec![std::mem::take(&mut tail)], &mut out);
                    }
                    reports(st.follow.cancel(), &mut out);
                    tail = m;
                }
            }
        }
        if !tail.is_empty() {
            out.extend(tail);
            out.extend(wire::sync(0, 0));
        }
        self.cv.notify_all();
        if out.is_empty() {
            return Ok(());
        }
        (self.sink)(&out)
    }

    /// Serve follow mode until [`Link::stop`].
    pub fn serve(&self) {
        let mut st = self.lock();
        loop {
            if st.stop {
                return;
            }
            if !st.follow.busy() {
                st = self.cv.wait(st).unwrap_or_else(|e| e.into_inner());
                continue;
            }
            let step = match self.cursor() {
                Some(c) => st.follow.poll(Instant::now(), c),
                None => Step {
                    packet: None,
                    release: st.follow.cancel(),
                },
            };
            let mut out = Vec::new();
            if let Some((dx, dy)) = step.packet {
                reports(vec![wire::rel(st.head, dx, dy)], &mut out);
            }
            reports(step.release, &mut out);
            if !out.is_empty() && (self.sink)(&out).is_err() {
                return;
            }
            drop(st);
            // No answer comes sooner than TOO_SOON after a packet.
            std::thread::sleep(if step.packet.is_some() {
                TOO_SOON
            } else {
                POLL
            });
            st = self.lock();
        }
    }

    pub fn stop(&self) {
        self.lock().stop = true;
        self.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IRIX 6.5.30 on a VPro as measured: each packet is acted on 10 ms after
    /// it is sent, accelerated on its own, and draws the cursor once.
    struct Guest {
        queue: VecDeque<(Instant, i32, i32)>,
        x: i32,
        y: i32,
        seq: u32,
        gain: f64,
        size: (i32, i32),
        stall: Option<(Instant, Instant)>,
        lat: Duration,
    }

    impl Guest {
        fn new(x: i32, y: i32) -> Guest {
            Guest {
                queue: VecDeque::new(),
                x,
                y,
                seq: 0,
                gain: 1.0,
                size: (1280, 1024),
                stall: None,
                lat: Duration::from_millis(10),
            }
        }
        fn run(&mut self, now: Instant) {
            if self.stall.is_some_and(|(a, b)| now >= a && now < b) {
                return;
            }
            while self.queue.front().is_some_and(|p| now - p.0 >= self.lat) {
                let (_, dx, dy) = self.queue.pop_front().unwrap();
                let mv = |n: i32| (accel(n) as f64 * self.gain).round() as i32;
                self.x = (self.x + mv(dx)).clamp(0, self.size.0 - 1);
                self.y = (self.y + mv(dy)).clamp(0, self.size.1 - 1);
                self.seq += 2;
            }
        }
        fn cursor(&self) -> Cursor {
            Cursor {
                seq: self.seq,
                x: self.x,
                y: self.y,
            }
        }
    }

    struct Run {
        errors: Vec<f64>,
        draws: Vec<Instant>,
        end: (i32, i32),
        last: (i32, i32),
    }

    /// Move the host pointer along `path` (seconds to pixels) at 120 Hz for
    /// `secs`, polling every 0.5 ms, then let the cursor settle for 200 ms.
    fn drive(g: &mut Guest, path: impl Fn(f64) -> (i32, i32), secs: f64) -> Run {
        drive_from(Instant::now(), g, path, secs)
    }

    fn drive_from(t0: Instant, g: &mut Guest, path: impl Fn(f64) -> (i32, i32), secs: f64) -> Run {
        let mut f = Follow::default();
        let mut r = Run {
            errors: vec![],
            draws: vec![],
            end: (0, 0),
            last: (0, 0),
        };
        let mut aim = path(0.0);
        let mut seq = g.seq;
        let ticks = ((secs + 0.2) * 2000.0) as u32;
        for k in 0..ticks {
            let t = k as f64 / 2000.0;
            let now = t0 + Duration::from_micros(500 * k as u64);
            if k % 17 == 0 && t <= secs {
                aim = path(t);
                f.aim(aim.0, aim.1, 1280, 1024);
            }
            g.run(now);
            if g.seq != seq {
                seq = g.seq;
                r.draws.push(now);
            }
            if t > 0.2 && t <= secs {
                r.errors
                    .push(((g.x - aim.0) as f64).hypot((g.y - aim.1) as f64));
            }
            if let Some((dx, dy)) = f.poll(now, g.cursor()).packet {
                g.queue.push_back((now, dx, dy));
            }
        }
        r.end = (g.x, g.y);
        r.last = aim;
        r
    }

    fn p95(v: &[f64]) -> f64 {
        let mut s = v.to_vec();
        s.sort_by(f64::total_cmp);
        s[s.len() * 95 / 100]
    }

    fn circle(t: f64) -> (i32, i32) {
        let a = std::f64::consts::PI * t;
        (
            (640.0 + 200.0 * a.cos()).round() as i32,
            (512.0 + 200.0 * a.sin()).round() as i32,
        )
    }

    #[test]
    fn accel_and_counts_are_inverse_without_overshoot() {
        assert_eq!([1, 4, 5, 8, 64, 127].map(accel), [1, 4, 6, 12, 124, 250]);
        for px in -300..=300 {
            let n = counts(px);
            assert!(accel(n).abs() <= px.abs(), "{px}");
            assert!(px.abs() > 250 || px.abs() - accel(n).abs() <= 1, "{px}");
        }
    }

    // The old loop waited 40 ms after each answer and drew the cursor at
    // 20 Hz, 13 px behind on this circle with 30 px at the 95th percentile.
    #[test]
    fn a_circling_pointer_is_followed_every_guest_draw() {
        let mut g = Guest::new(840, 512);
        let r = drive(&mut g, circle, 4.0);
        let gaps: Vec<f64> = r
            .draws
            .windows(2)
            .map(|w| (w[1] - w[0]).as_secs_f64() * 1e3)
            .collect();
        let mean_gap = gaps.iter().sum::<f64>() / gaps.len() as f64;
        let mean = r.errors.iter().sum::<f64>() / r.errors.len() as f64;
        eprintln!(
            "drawn every {mean_gap:.1} ms, mean {mean:.1} p95 {:.1}",
            p95(&r.errors)
        );
        assert!(mean_gap < 11.0, "cursor drawn every {mean_gap:.1} ms");
        assert!(
            mean < 12.0 && p95(&r.errors) < 16.0,
            "mean {mean:.1} p95 {:.1}",
            p95(&r.errors)
        );
        assert_eq!(r.end, r.last);
    }

    #[test]
    fn a_jump_lands_exactly_without_passing_the_target() {
        for (to, from) in [
            ((1000, 900), (200, 200)),
            ((207, 3), (640, 512)),
            ((641, 513), (640, 512)),
        ] {
            let mut g = Guest::new(from.0, from.1);
            let mut f = Follow::default();
            f.aim(to.0, to.1, 1280, 1024);
            let t0 = Instant::now();
            let (mut lo, mut hi) = (from, from);
            for k in 0..400u64 {
                let now = t0 + Duration::from_micros(500 * k);
                g.run(now);
                lo = (lo.0.min(g.x), lo.1.min(g.y));
                hi = (hi.0.max(g.x), hi.1.max(g.y));
                if let Some((dx, dy)) = f.poll(now, g.cursor()).packet {
                    g.queue.push_back((now, dx, dy));
                }
            }
            assert_eq!((g.x, g.y), to);
            let inside = |v: i32, a: i32, b: i32| v >= a.min(b) && v <= a.max(b);
            assert!(
                inside(lo.0, from.0, to.0) && inside(hi.0, from.0, to.0),
                "{lo:?} {hi:?}"
            );
            assert!(
                inside(lo.1, from.1, to.1) && inside(hi.1, from.1, to.1),
                "{lo:?} {hi:?}"
            );
        }
    }

    #[test]
    fn a_guest_that_accelerates_more_or_less_still_lands() {
        for gain in [0.5, 1.8] {
            let mut g = Guest::new(840, 512);
            g.gain = gain;
            let r = drive(&mut g, circle, 2.0);
            let mean = r.errors.iter().sum::<f64>() / r.errors.len() as f64;
            // At 1.8 one count moves 2 pixels, so an odd pixel is out of reach.
            assert!(
                (r.end.0 - r.last.0).abs() <= 1 && (r.end.1 - r.last.1).abs() <= 1,
                "gain {gain}: {:?} {:?}",
                r.end,
                r.last
            );
            assert!(mean < 15.0, "gain {gain}: mean {mean:.1}");
        }
    }

    // Resending after 60 ms of silence put several packets' motion in the
    // guest's queue during a stall, and the cursor flew 400 px past.
    #[test]
    fn a_stalled_guest_gets_no_motion_twice() {
        let t0 = Instant::now();
        let mut g = Guest::new(840, 512);
        g.stall = Some((
            t0 + Duration::from_secs(1),
            t0 + Duration::from_millis(1340),
        ));
        let r = drive_from(t0, &mut g, circle, 3.0);
        let worst = r.errors.iter().cloned().fold(0.0, f64::max);
        // The pointer covers 210 px of the circle while the guest is stalled.
        assert!(worst < 240.0, "{worst:.0} px off");
        assert_eq!(r.end, r.last);
    }

    // A guest that drew within 4 ms went uncounted: the packet timed out
    // after 500 ms and the cursor froze short of the pointer.
    #[test]
    fn a_guest_that_answers_at_once_is_followed() {
        for gain in [1.0, 1.8] {
            let mut g = Guest::new(840, 512);
            g.lat = Duration::from_micros(500);
            g.gain = gain;
            let r = drive(&mut g, circle, 2.0);
            let mean = r.errors.iter().sum::<f64>() / r.errors.len() as f64;
            assert!(mean < 10.0, "gain {gain}: mean {mean:.1}");
            assert!(
                (r.end.0 - r.last.0).abs() <= 1 && (r.end.1 - r.last.1).abs() <= 1,
                "gain {gain}"
            );
        }
    }

    #[test]
    fn a_redraw_right_after_a_packet_is_not_its_answer() {
        let t0 = Instant::now();
        let mut f = Follow::default();
        f.aim(700, 500, 1280, 1024);
        let c = |seq, x| Cursor { seq, x, y: 500 };
        assert_eq!(f.poll(t0, c(0, 600)).packet, Some((52, 0)));
        // A shape change 1 ms later moves the corner; the packet is still out.
        let p = f.poll(t0 + Duration::from_millis(1), c(2, 594)).packet;
        assert_eq!(p, None);
        assert_eq!(
            f.poll(t0 + Duration::from_millis(10), c(4, 694)).packet,
            Some((5, 0))
        );
    }

    #[test]
    fn a_click_waits_for_the_cursor_and_keys_keep_their_place() {
        let t0 = Instant::now();
        let mut f = Follow::default();
        let c = |seq, x| Cursor { seq, x, y: 0 };
        assert_eq!(f.hold(b"k".to_vec(), false, t0), Some(b"k".to_vec()));
        f.poll(t0, c(0, 0));
        f.aim(100, 0, 1280, 1024);
        f.poll(t0, c(0, 0));
        assert_eq!(f.hold(b"down".to_vec(), true, t0), None);
        assert_eq!(f.hold(b"k".to_vec(), false, t0), None);
        let s = f.poll(t0 + Duration::from_millis(10), c(2, 100));
        assert_eq!(s.packet, None);
        assert_eq!(s.release, vec![b"down".to_vec(), b"k".to_vec()]);
        // A guest that never answers holds a click no longer than HOLD.
        f.aim(300, 0, 1280, 1024);
        f.poll(t0 + Duration::from_millis(11), c(2, 100));
        assert_eq!(
            f.hold(b"up".to_vec(), true, t0 + Duration::from_millis(11)),
            None
        );
        let s = f.poll(t0 + Duration::from_millis(11) + HOLD, c(2, 100));
        assert_eq!(s.release, vec![b"up".to_vec()]);
    }

    #[test]
    fn a_guest_that_never_answers_is_left_alone() {
        let t0 = Instant::now();
        let mut f = Follow::default();
        f.aim(500, 0, 1280, 1024);
        let mut sent = 0;
        for k in 0..1000u64 {
            let s = f.poll(t0 + Duration::from_millis(k), Cursor { seq: 0, x: 0, y: 0 });
            sent += usize::from(s.packet.is_some());
        }
        assert_eq!(sent, 1);
        assert!(!f.busy());
        f.aim(501, 0, 1280, 1024);
        assert!(f
            .poll(t0 + Duration::from_secs(2), Cursor { seq: 0, x: 0, y: 0 })
            .packet
            .is_some());
    }

    /// A link over a cursor and a byte log the test controls.
    type Shared<T> = std::sync::Arc<Mutex<T>>;

    fn link() -> (
        std::sync::Arc<Link>,
        Shared<Option<Cursor>>,
        Shared<Vec<u8>>,
    ) {
        use std::sync::Arc;
        let cursor = Arc::new(Mutex::new(Some(Cursor { seq: 0, x: 0, y: 0 })));
        let log = Arc::new(Mutex::new(Vec::new()));
        let (c, l) = (cursor.clone(), log.clone());
        let link = Link::new(
            Box::new(move || *c.lock().unwrap()),
            Box::new(move |m| {
                l.lock().unwrap().extend_from_slice(m);
                Ok(())
            }),
        );
        (Arc::new(link), cursor, log)
    }

    fn find(log: &[u8], what: &[u8]) -> Option<usize> {
        log.windows(what.len()).position(|w| w == what)
    }

    fn at(x: i32) -> Item {
        Item::At {
            x,
            y: 0,
            w: 1280,
            h: 1024,
            abs: b"ABS!".to_vec(),
        }
    }

    // The thread released a held click outside the lock, so a key sent in
    // that gap went to the guest before the click it followed.
    #[test]
    fn a_key_never_overtakes_the_click_it_follows() {
        for _ in 0..200 {
            let (link, cursor, log) = link();
            let serving = link.clone();
            let t = std::thread::spawn(move || serving.serve());
            link.send(0, vec![at(100)]).unwrap();
            link.send(0, vec![Item::Press(b"CLICK!".to_vec(), true)])
                .unwrap();
            *cursor.lock().unwrap() = Some(Cursor {
                seq: 2,
                x: 100,
                y: 0,
            });
            for _ in 0..50 {
                link.send(0, vec![Item::Press(b"KEY!".to_vec(), false)])
                    .unwrap();
            }
            // Wait for the last key, not a fixed time: a loaded runner is slow to serve.
            let end = Instant::now() + Duration::from_secs(10);
            let keys = || {
                log.lock()
                    .unwrap()
                    .windows(4)
                    .filter(|w| w == b"KEY!")
                    .count()
            };
            while keys() < 50 {
                assert!(Instant::now() < end, "nothing released");
                std::thread::sleep(Duration::from_millis(1));
            }
            link.stop();
            t.join().unwrap();
            let log = log.lock().unwrap();
            let (click, key) = (find(&log, b"CLICK!"), find(&log, b"KEY!"));
            assert!(click.is_some() && click < key, "{click:?} {key:?}");
        }
    }

    #[test]
    fn a_guest_that_loses_its_cursor_still_gets_what_was_held_first() {
        let (link, cursor, log) = link();
        link.send(0, vec![at(100)]).unwrap();
        link.send(0, vec![Item::Press(b"CLICK!".to_vec(), true)])
            .unwrap();
        assert_eq!(find(&log.lock().unwrap(), b"CLICK!"), None);
        *cursor.lock().unwrap() = None;
        link.send(0, vec![Item::Press(b"KEY!".to_vec(), false), at(200)])
            .unwrap();
        let log = log.lock().unwrap();
        let (click, key, abs) = (
            find(&log, b"CLICK!"),
            find(&log, b"KEY!"),
            find(&log, b"ABS!"),
        );
        assert!(
            click.is_some() && click < key && key < abs,
            "{click:?} {key:?} {abs:?}"
        );
    }
}
