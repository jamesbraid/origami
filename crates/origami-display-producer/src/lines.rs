//! Sets of display lines, and the shared marks a source raises when its
//! picture changes.

use std::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A set of line numbers below a fixed height.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lines {
    bits: Vec<u64>,
    n: u32,
}

impl Lines {
    pub fn new(n: u32) -> Lines {
        Lines {
            bits: vec![0; (n as usize).div_ceil(64)],
            n,
        }
    }
    pub fn full(n: u32) -> Lines {
        let mut l = Lines::new(n);
        l.set_all();
        l
    }
    pub fn len(&self) -> u32 {
        self.n
    }
    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|&w| w == 0)
    }
    pub fn set(&mut self, y: u32) {
        if y < self.n {
            self.bits[y as usize / 64] |= 1 << (y % 64);
        }
    }
    pub fn set_all(&mut self) {
        self.bits.fill(!0);
        self.trim();
    }
    fn trim(&mut self) {
        if !self.n.is_multiple_of(64) {
            if let Some(w) = self.bits.last_mut() {
                *w &= (1u64 << (self.n % 64)) - 1;
            }
        }
    }
    pub fn clear(&mut self) {
        self.bits.iter_mut().for_each(|w| *w = 0);
    }
    pub fn contains(&self, y: u32) -> bool {
        y < self.n && self.bits[y as usize / 64] >> (y % 64) & 1 != 0
    }
    pub fn count(&self) -> u32 {
        self.bits.iter().map(|w| w.count_ones()).sum()
    }
    pub fn union(&mut self, o: &Lines) {
        for (a, b) in self.bits.iter_mut().zip(&o.bits) {
            *a |= *b;
        }
    }
    pub fn words(&self) -> &[u64] {
        &self.bits
    }
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.n).filter(|&y| self.contains(y))
    }
    /// Runs of consecutive lines as `(first, count)`.
    pub fn runs(&self) -> Vec<(u32, u32)> {
        let mut v: Vec<(u32, u32)> = vec![];
        for y in self.iter() {
            match v.last_mut() {
                Some((s, c)) if *s + *c == y => *c += 1,
                _ => v.push((y, 1)),
            }
        }
        v
    }
}

/// Wakes the producer's thread from any other.
pub struct Waker(pub(crate) crate::plat::Wake);

impl Waker {
    pub fn new() -> std::io::Result<Arc<Waker>> {
        Ok(Arc::new(Waker(crate::plat::Wake::new()?)))
    }
    pub fn wake(&self) {
        self.0.wake()
    }
    pub(crate) fn drain(&self) {
        self.0.drain()
    }
}

/// Lines a source has changed since the producer last looked, and the flag
/// that tells the producer there is something to look at.
pub struct Marks {
    bits: Vec<AtomicU64>,
    all: AtomicBool,
    flag: AtomicBool,
    waker: Arc<Waker>,
}

/// Most lines a head may have; the mark bitmap is sized once for it.
pub const MAX_LINES: u32 = 16384;

impl Marks {
    pub(crate) fn new(waker: Arc<Waker>) -> Arc<Marks> {
        Arc::new(Marks {
            bits: (0..MAX_LINES / 64).map(|_| AtomicU64::new(0)).collect(),
            all: AtomicBool::new(false),
            flag: AtomicBool::new(false),
            waker,
        })
    }

    /// Mark line `y` changed: set its bit, then raise the flag, waking the
    /// producer if the flag was clear. Call after the store that changed it.
    pub fn mark(&self, y: u32) {
        self.set_bit(y);
        self.raise();
    }

    pub fn set_bit(&self, y: u32) {
        if y < MAX_LINES {
            self.bits[y as usize / 64].fetch_or(1 << (y % 64), Ordering::Relaxed);
        }
    }

    /// Full barrier, then exchange the flag; the producer is woken if it was clear.
    pub fn raise(&self) {
        fence(Ordering::SeqCst);
        if !self.flag.swap(true, Ordering::SeqCst) {
            self.waker.wake();
        }
    }

    pub fn mark_all(&self) {
        self.all.store(true, Ordering::Relaxed);
        self.raise();
    }

    /// Mark line `y` from another thread, pausing `delay` between the bit and
    /// the flag so a request can land in the gap (the suite's race check).
    pub fn mark_delayed(self: &Arc<Self>, y: u32, delay: Duration) {
        let m = self.clone();
        std::thread::spawn(move || {
            m.set_bit(y);
            std::thread::sleep(delay);
            m.raise();
        });
    }

    pub fn pending(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Clear the flag, full barrier, then take every marked line below `height`.
    pub(crate) fn take(&self, height: u32) -> Lines {
        self.flag.store(false, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        let mut l = Lines::new(height);
        if self.all.swap(false, Ordering::Relaxed) {
            l.set_all();
        }
        for (i, w) in self.bits.iter().enumerate() {
            let v = w.swap(0, Ordering::Relaxed);
            if v != 0 && i < l.bits.len() {
                l.bits[i] |= v;
            }
        }
        l.trim();
        l
    }
}

impl Marks {
    /// Mark `lines` from another thread, pausing `delay` between the bits
    /// and the flag so a request can land in the gap (the suite's race check).
    pub fn mark_lines_delayed(self: &Arc<Self>, lines: std::ops::Range<u32>, delay: Duration) {
        let m = self.clone();
        std::thread::spawn(move || {
            for y in lines {
                m.set_bit(y);
            }
            std::thread::sleep(delay);
            m.raise();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_group_consecutive_lines() {
        let mut l = Lines::new(100);
        for y in [1, 2, 3, 10, 64, 65] {
            l.set(y);
        }
        assert_eq!(l.runs(), vec![(1, 3), (10, 1), (64, 2)]);
        assert_eq!(l.count(), 6);
    }

    #[test]
    fn full_stops_at_the_height() {
        assert_eq!(Lines::full(70).count(), 70);
    }
}
