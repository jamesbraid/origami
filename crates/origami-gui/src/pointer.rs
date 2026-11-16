//! How the host pointer reaches the guest, apart from any window.
//!
//! Every SGI pointer is a relative PS/2 mouse, so there are two honest ways
//! to drive one. Captured (the default) the viewer hides and locks the host
//! cursor and sends the raw motion it sees, one to one, which the guest
//! accelerates as it would for a real mouse. Absolute, the viewer names a
//! place in the picture and QEMU walks the guest's cursor there in a closed
//! loop: it sends a step, waits for the cursor to settle, sizes the next step
//! and repeats (`ui/sgi-display-input.c`). A new target restarts that loop, so
//! [`AbsThrottle`] gives it one target at a time and lets it finish.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::core::Key;

/// Which way the pointer reaches the guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerMode {
    /// Click the picture to capture the host pointer; send its raw motion.
    Capture,
    /// Send the pointer's place in the picture and let QEMU close the loop.
    Absolute,
}

/// Raw device motion is fractional on some hosts (macOS reports it in points
/// as a float). The guest takes whole counts, so the remainder is carried to
/// the next event and no slow motion is lost to rounding.
///
/// X11 (XInput raw motion) and Windows (raw input) report the device's own
/// counts. Whether macOS's values include the system's pointer acceleration
/// is not known here; if they do, IRIX accelerates motion that has been
/// accelerated once already.
#[derive(Default, Debug)]
pub struct RelAccum {
    fx: f64,
    fy: f64,
}

impl RelAccum {
    /// Add a motion; the whole counts it has now made, if any.
    pub fn push(&mut self, dx: f64, dy: f64) -> Option<(i32, i32)> {
        self.fx += dx;
        self.fy += dy;
        let (ix, iy) = (self.fx.trunc(), self.fy.trunc());
        self.fx -= ix;
        self.fy -= iy;
        (ix != 0.0 || iy != 0.0).then_some((ix as i32, iy as i32))
    }

    pub fn reset(&mut self) {
        *self = RelAccum::default();
    }
}

/// Whether `held` (the keys the guest believes are down) is the chord that
/// frees a captured pointer: Ctrl and Option on a Mac, as UTM does, and Ctrl
/// and Alt elsewhere. Slint swaps Command and Control on Apple platforms
/// (Control arrives as Meta, Command as Control) so on a Mac the physical
/// Control key is Slint's Meta.
///
/// Windows sends AltGr as Ctrl and Alt together, so there only the left keys
/// count, or typing an AltGr character would free the pointer.
pub fn is_release_chord(held: &HashSet<Key>) -> bool {
    let any = |keys: &[Key]| keys.iter().any(|k| held.contains(k));
    let ctrl = if cfg!(target_os = "macos") {
        any(&[Key::MetaLeft, Key::MetaRight])
    } else if cfg!(windows) {
        any(&[Key::ControlLeft])
    } else {
        any(&[Key::ControlLeft, Key::ControlRight])
    };
    let alt = if cfg!(windows) {
        any(&[Key::AltLeft])
    } else {
        any(&[Key::AltLeft, Key::AltRight])
    };
    ctrl && alt
}

/// The chord's name, for the status line.
pub fn release_chord_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Ctrl+Option"
    } else {
        "Ctrl+Alt"
    }
}

/// A place in a head's pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub head: u32,
    pub x: i32,
    pub y: i32,
}

/// How long one absolute target gets before the next is sent.
///
/// QEMU's loop sends a step, waits until the guest cursor has been still for
/// 40 ms of guest time, and sizes the next step from it: a move across a
/// 1280 pixel head under IRIX acceleration takes a few such steps, a couple of
/// hundred milliseconds. A target sent sooner restarts the loop with its gain
/// forgotten, so a pointer moving at an ordinary speed, which names a new
/// target every few milliseconds, never lets it finish. Waiting out a period
/// and then sending only the latest place lets the cursor arrive each time.
///
/// QEMU could do this better itself, and then the viewer need not guess a
/// period: it could keep walking toward the newest target without
/// forgetting the gain it has learned for the old one, and send the guest
/// cursor's own motion back to the viewer so a viewer can see it arrive.
pub const ABS_PERIOD: Duration = Duration::from_millis(250);

/// Absolute targets, at most one per [`ABS_PERIOD`], the newest wins.
#[derive(Default, Debug)]
pub struct AbsThrottle {
    pending: Option<Target>,
    sent: Option<(Target, Instant)>,
}

impl AbsThrottle {
    /// Note where the pointer is now.
    pub fn offer(&mut self, t: Target) {
        // Nothing to say when it is where the last one was sent.
        self.pending = (self.sent.map(|s| s.0) != Some(t)).then_some(t);
    }

    /// The target to send now, if there is one and its period has begun.
    pub fn take_due(&mut self, now: Instant) -> Option<Target> {
        let t = self.pending?;
        if let Some((_, at)) = self.sent {
            if now.saturating_duration_since(at) < ABS_PERIOD {
                return None;
            }
        }
        self.pending = None;
        self.sent = Some((t, now));
        Some(t)
    }

    /// The newest target, now, whatever the period: a click must land where
    /// the pointer is.
    pub fn take_now(&mut self, now: Instant) -> Option<Target> {
        let t = self.pending.take()?;
        self.sent = Some((t, now));
        Some(t)
    }

    /// How long until a pending target is due; None when nothing waits.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.pending?;
        Some(match self.sent {
            Some((_, at)) => ABS_PERIOD.saturating_sub(now.saturating_duration_since(at)),
            None => Duration::ZERO,
        })
    }

    pub fn reset(&mut self) {
        *self = AbsThrottle::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: i32) -> Target {
        Target { head: 0, x, y: 5 }
    }

    #[test]
    fn fractions_carry_until_they_make_a_count() {
        let mut r = RelAccum::default();
        assert_eq!(r.push(0.4, 0.0), None);
        assert_eq!(r.push(0.4, 0.0), None);
        assert_eq!(r.push(0.4, 0.0), Some((1, 0)));
        // 0.2 is left over, then -0.1.
        assert_eq!(r.push(-1.3, 2.0), Some((-1, 2)));
        assert_eq!(r.push(0.0, 0.0), None);
        assert_eq!(r.push(3.0, -4.0), Some((2, -4)));
    }

    #[test]
    fn a_stream_of_targets_is_one_per_period_and_the_newest_wins() {
        let t0 = Instant::now();
        let mut a = AbsThrottle::default();
        a.offer(at(1));
        assert_eq!(a.wait(t0), Some(Duration::ZERO));
        assert_eq!(a.take_due(t0), Some(at(1)));
        // A hundred moves in the next 100 ms send nothing.
        let mut sent = 0;
        for i in 2..102 {
            a.offer(at(i));
            sent += usize::from(a.take_due(t0 + Duration::from_millis(i as u64)).is_some());
        }
        assert_eq!(sent, 0);
        assert_eq!(
            a.wait(t0 + Duration::from_millis(100)),
            Some(ABS_PERIOD - Duration::from_millis(100))
        );
        // When the period is up the newest goes, once.
        assert_eq!(a.take_due(t0 + ABS_PERIOD), Some(at(101)));
        assert_eq!(a.take_due(t0 + ABS_PERIOD * 2), None);
        assert_eq!(a.wait(t0 + ABS_PERIOD * 2), None);
    }

    #[test]
    fn the_place_already_sent_is_not_sent_again_and_a_click_does_not_wait() {
        let t0 = Instant::now();
        let mut a = AbsThrottle::default();
        a.offer(at(7));
        assert_eq!(a.take_due(t0), Some(at(7)));
        a.offer(at(7));
        assert_eq!(a.wait(t0), None);
        a.offer(at(8));
        assert_eq!(a.take_due(t0 + Duration::from_millis(1)), None);
        assert_eq!(a.take_now(t0 + Duration::from_millis(1)), Some(at(8)));
        assert_eq!(a.take_now(t0 + Duration::from_millis(2)), None);
        // The click started a period of its own.
        a.offer(at(9));
        assert_eq!(a.take_due(t0 + Duration::from_millis(100)), None);
        assert_eq!(a.take_due(t0 + Duration::from_millis(251)), Some(at(9)));
    }

    #[test]
    fn the_release_chord_is_the_hosts_own() {
        let mut held = HashSet::new();
        assert!(!is_release_chord(&held));
        held.insert(Key::AltLeft);
        assert!(!is_release_chord(&held));
        // Slint's Meta is the physical Control key on a Mac.
        let ctrl = if cfg!(target_os = "macos") {
            Key::MetaLeft
        } else {
            Key::ControlLeft
        };
        held.insert(ctrl);
        assert!(is_release_chord(&held));
        // The right-hand keys count everywhere but Windows, where AltGr is
        // Ctrl and Alt.
        held.remove(&Key::AltLeft);
        held.insert(Key::AltRight);
        assert_eq!(is_release_chord(&held), !cfg!(windows));
        held.remove(&Key::AltRight);
        held.insert(Key::AltLeft);
        // Command (Slint's Control on a Mac) is not it there; Meta is not it elsewhere.
        let other = if cfg!(target_os = "macos") {
            Key::ControlLeft
        } else {
            Key::MetaLeft
        };
        held.remove(&ctrl);
        held.insert(other);
        assert!(!is_release_chord(&held));
    }
}
