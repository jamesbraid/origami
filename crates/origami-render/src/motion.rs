//! How the cube turns: an orientation and an angular velocity. A drag tumbles it,
//! a release flings it, and friction returns it to the calm spin, rest or, settling, the pose.

use std::f32::consts::{PI, TAU};

/// The calm spin about the screen's vertical axis, in turns a minute.
pub const CALM_RPM: f32 = 10.0;
/// How fast a release's extra speed dies away, per second.
pub const FRICTION: f32 = 1.3;
/// The fastest fling, in turns a second.
pub const MAX_TURNS: f32 = 6.0;
/// The calm spin eases in over this many seconds.
const EASE_IN: f32 = 1.2;
/// Easing back to the mark pose takes this many seconds.
const HOME_SECS: f32 = 0.75;
/// A pointer that waited longer than this before release throws nothing.
const STALE_MS: u64 = 100;
/// Below this speed (rad/s) a cube without calm spin counts as stopped.
const STOPPED: f32 = 0.05;
/// A settling cube starts easing home once it has slowed to this (rad/s).
const SETTLE_BELOW: f32 = 1.5;

type V3 = [f32; 3];
/// x, y, z, w.
type Quat = [f32; 4];

pub struct Motion {
    q: Quat,
    w: V3,
    ease: f32,
    /// The calm spin's angular velocity as of the last step.
    target: V3,
    calm: bool,
    reduce: bool,
    /// After a drag, coast and then ease back to the still pose.
    settle: bool,
    drag: Option<(f32, f32, u64)>,
    /// Easing to the mark pose: where it started and how far along (0 to 1).
    home: Option<(Quat, f32)>,
}

fn mul(a: Quat, b: Quat) -> Quat {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

/// The rotation by the vector `v`: about its direction, by its length (rad).
fn rotation(v: V3) -> Quat {
    let a = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if !(a >= 1e-9 && a.is_finite()) {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let s = (a * 0.5).sin() / a;
    [v[0] * s, v[1] * s, v[2] * s, (a * 0.5).cos()]
}

fn norm(v: V3) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

impl Motion {
    /// At mark.svg's pose, at rest.
    pub fn new(calm: bool, reduce: bool, settle: bool) -> Motion {
        Motion {
            q: [0.0, 0.0, 0.0, 1.0],
            w: [0.0; 3],
            ease: 0.0,
            target: [0.0; 3],
            calm,
            reduce,
            settle,
            drag: None,
            home: None,
        }
    }

    /// Reduced motion shows the mark.svg pose and never spins; a drag still
    /// turns the cube and it stops dead on release.
    pub fn configure(&mut self, calm: bool, reduce: bool, settle: bool) {
        if reduce && !self.reduce {
            *self = Motion::new(calm, true, settle);
            return;
        }
        self.calm = calm;
        self.reduce = reduce;
        self.settle = settle;
    }

    fn at_home(&self) -> bool {
        self.q[3].abs() > 1.0 - 1e-6
    }

    /// A settling cube that is off the still pose has somewhere to go.
    fn settling(&self) -> bool {
        self.settle && !self.calm && !self.at_home()
    }

    pub fn press(&mut self, x: f32, y: f32, ms: u64) {
        self.drag = Some((x, y, ms));
        self.home = None;
        self.w = [0.0; 3];
    }

    /// Ease back to the mark pose, stopping any spin.
    pub fn go_home(&mut self) {
        self.home = Some((self.q, 0.0));
        self.w = [0.0; 3];
    }

    /// The pointer moved to `x`, `y` (points); the cube is `side` points wide
    /// and turns half a turn for each width of travel.
    pub fn drag_to(&mut self, x: f32, y: f32, ms: u64, side: f32) {
        let Some((px, py, pms)) = self.drag else {
            return;
        };
        let k = PI / side.max(1.0);
        let turn = [(y - py) * k, (x - px) * k, 0.0];
        self.q = mul(rotation(turn), self.q);
        if ms > pms {
            let dt = (ms - pms) as f32 * 1e-3;
            for (w, t) in self.w.iter_mut().zip(turn) {
                *w = 0.5 * *w + 0.5 * t / dt;
            }
        }
        self.drag = Some((x, y, ms));
    }

    pub fn release(&mut self, ms: u64) {
        let Some((_, _, last)) = self.drag.take() else {
            return;
        };
        let top = MAX_TURNS * TAU;
        let speed = norm(self.w);
        if self.reduce || ms.saturating_sub(last) > STALE_MS {
            self.w = [0.0; 3];
        } else if speed > top {
            self.w = self.w.map(|c| c * top / speed);
        }
        if self.reduce && self.settle && !self.calm {
            // Reduced motion eases nothing: the still pose is back at once.
            self.q = [0.0, 0.0, 0.0, 1.0];
        }
    }

    /// Advance by `dt` seconds of wall time.
    pub fn step(&mut self, dt: f32) {
        if self.drag.is_some() {
            return;
        }
        if let Some((from, t)) = self.home {
            let t = (t + dt / HOME_SECS).min(1.0);
            let e = t * t * (3.0 - 2.0 * t);
            // The shorter way round, by normalised linear interpolation.
            let sign = if from[3] < 0.0 { -1.0 } else { 1.0 };
            let mut q = from.map(|c| c * sign * (1.0 - e));
            q[3] += e;
            let n = q.iter().map(|c| c * c).sum::<f32>().sqrt();
            self.q = q.map(|c| c / n);
            self.home = (t < 1.0).then_some((from, t));
            return;
        }
        if self.settling() && norm(self.w) < SETTLE_BELOW {
            self.go_home();
            return self.step(dt);
        }
        let spin = self.calm && !self.reduce;
        self.ease = if spin {
            (self.ease + dt / EASE_IN).min(1.0)
        } else {
            0.0
        };
        let eased = self.ease * self.ease * (3.0 - 2.0 * self.ease);
        let target = [0.0, CALM_RPM * TAU / 60.0 * eased, 0.0];
        self.target = target;
        let keep = (-FRICTION * dt).exp();
        for (w, t) in self.w.iter_mut().zip(target) {
            *w = t + (*w - t) * keep;
        }
        if !spin && norm(self.w) < STOPPED {
            self.w = [0.0; 3];
        }
        let turn = self.w.map(|c| c * dt);
        let q = mul(rotation(turn), self.q);
        let n = q.iter().map(|c| c * c).sum::<f32>().sqrt();
        self.q = q.map(|c| c / n);
    }

    /// Something is still changing: draw again next frame.
    pub fn moving(&self) -> bool {
        self.drag.is_some()
            || self.home.is_some()
            || (self.calm && !self.reduce)
            || norm(self.w) > 0.0
            || self.settling()
    }

    /// Only the calm spin is turning it, so a slow frame rate shows the same
    /// motion: nobody is dragging, flinging or easing it to a pose.
    pub fn coasting(&self) -> bool {
        let off = [0, 1, 2].map(|i| self.w[i] - self.target[i]);
        self.drag.is_none() && self.home.is_none() && norm(off) < 1.2
    }

    /// The orientation as a matrix, row by row.
    pub fn matrix(&self) -> [V3; 3] {
        let [x, y, z, w] = self.q;
        [
            [
                1.0 - 2.0 * (y * y + z * z),
                2.0 * (x * y - z * w),
                2.0 * (x * z + y * w),
            ],
            [
                2.0 * (x * y + z * w),
                1.0 - 2.0 * (x * x + z * z),
                2.0 * (y * z - x * w),
            ],
            [
                2.0 * (x * z - y * w),
                2.0 * (y * z + x * w),
                1.0 - 2.0 * (x * x + y * y),
            ],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the cube's front (0, 0, 1) points.
    fn front(m: &Motion) -> V3 {
        let r = m.matrix();
        [r[0][2], r[1][2], r[2][2]]
    }

    #[test]
    fn a_calm_cube_makes_10_turns_a_minute_once_eased_in() {
        let mut m = Motion::new(true, false, false);
        for _ in 0..600 {
            m.step(0.01);
        }
        let before = front(&m);
        // 6 s at 10 rpm is one full turn: back where it started.
        for _ in 0..600 {
            m.step(0.01);
        }
        let after = front(&m);
        assert!(before.iter().zip(after).all(|(a, b)| (a - b).abs() < 2e-2));
        assert!(m.moving());
        assert!(m.coasting());
        m.press(0.0, 0.0, 0);
        assert!(!m.coasting());
    }

    #[test]
    fn a_drag_turns_half_a_turn_a_width_and_a_fast_release_flings() {
        let mut m = Motion::new(false, false, false);
        m.press(0.0, 0.0, 0);
        m.drag_to(50.0, 0.0, 10, 100.0);
        // Half a width: a quarter turn about the vertical axis, so the front
        // faces screen right.
        assert!((front(&m)[0] - 1.0).abs() < 1e-3);
        m.drag_to(60.0, 0.0, 20, 100.0);
        m.release(25);
        assert!(m.moving());
        let turned = front(&m);
        m.step(0.1);
        assert_ne!(front(&m), turned);
    }

    #[test]
    fn a_cube_without_calm_spin_coasts_to_a_stop_and_stops_dead_under_reduce() {
        let mut m = Motion::new(false, false, false);
        m.press(0.0, 0.0, 0);
        m.drag_to(30.0, 0.0, 16, 100.0);
        m.release(17);
        for _ in 0..2000 {
            m.step(0.016);
        }
        assert!(!m.moving());

        let mut r = Motion::new(true, true, false);
        r.press(0.0, 0.0, 0);
        r.drag_to(30.0, 0.0, 16, 100.0);
        r.release(17);
        assert!(!r.moving());
    }

    #[test]
    fn a_double_click_eases_back_to_the_mark_pose() {
        let mut m = Motion::new(false, false, false);
        m.press(0.0, 0.0, 0);
        m.drag_to(40.0, 25.0, 10, 100.0);
        m.release(500);
        m.go_home();
        assert!(m.moving());
        for _ in 0..100 {
            m.step(0.01);
        }
        assert!(!m.moving());
        let r = m.matrix();
        for (i, row) in r.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                assert!((v - if i == j { 1.0 } else { 0.0 }).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn a_pointer_that_waited_before_release_throws_nothing() {
        let mut m = Motion::new(false, false, false);
        m.press(0.0, 0.0, 0);
        m.drag_to(30.0, 0.0, 16, 100.0);
        m.release(500);
        assert!(!m.moving());
    }

    fn at_mark_pose(m: &Motion) -> bool {
        let r = m.matrix();
        (0..3).all(|i| (0..3).all(|j| (r[i][j] - if i == j { 1.0 } else { 0.0 }).abs() < 1e-3))
    }

    #[test]
    fn a_settling_cube_coasts_after_a_fling_then_eases_back_and_rests() {
        let mut m = Motion::new(false, false, true);
        m.press(0.0, 0.0, 0);
        m.drag_to(40.0, 10.0, 16, 100.0);
        m.drag_to(80.0, 20.0, 32, 100.0);
        m.release(34);
        let flung = front(&m);
        m.step(0.1);
        assert_ne!(front(&m), flung, "it coasts on after the release");
        let mut frames = 0;
        while m.moving() && frames < 2000 {
            m.step(0.016);
            frames += 1;
        }
        assert!(frames < 1000, "it comes to rest: {frames} frames");
        assert!(at_mark_pose(&m), "and rests at the mark pose");
        // A slow drag, released without a throw, goes straight home.
        m.press(0.0, 0.0, 0);
        m.drag_to(30.0, 0.0, 16, 100.0);
        m.release(500);
        assert!(m.moving() && !at_mark_pose(&m));
        for _ in 0..100 {
            m.step(0.016);
        }
        assert!(!m.moving() && at_mark_pose(&m));
    }

    #[test]
    fn reduced_motion_still_drags_but_neither_coasts_nor_eases() {
        let mut m = Motion::new(false, true, true);
        m.press(0.0, 0.0, 0);
        m.drag_to(50.0, 0.0, 16, 100.0);
        assert!(!at_mark_pose(&m), "the drag turns it");
        m.release(17);
        assert!(
            !m.moving(),
            "and it is back at the pose with nothing animated"
        );
        assert!(at_mark_pose(&m));
    }
}
