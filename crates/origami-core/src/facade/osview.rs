//! The activity view as data, so every app paints the same gr_osview-style bars.
//! Only host time of each emulated CPU's thread is measured: user, sys and the spare rest.

use super::cpu::CpuLoadReport;

/// Fraction of the way a bar moves to a new sample, so it glides and damps spikes.
pub const ATTACK: f32 = 0.5;
/// The ruler under each bar is marked in tenths.
pub const TICKS: u32 = 10;
/// The panel's and the bars' colours, 0xRRGGBB.
pub const BACKGROUND: u32 = 0xBFBFBF;
pub const BAR_BACKGROUND: u32 = 0x000000;
pub const USER: u32 = 0x0000FF;
pub const SYS: u32 = 0xFF0000;
pub const SPARE: u32 = 0x00FF00;

/// One legend entry, which is also a segment's name.
#[derive(Clone, Debug, PartialEq)]
pub struct OsviewKey {
    pub label: String,
    pub rgb: u32,
}

/// One segment of a bar: a share of its length, 0 to 1, in order from the left.
#[derive(Clone, Debug, PartialEq)]
pub struct OsviewSegment {
    pub fraction: f32,
    pub rgb: u32,
}

/// A labelled bar. `segments` has one entry per `legend` entry and the
/// fractions sum to 1.
#[derive(Clone, Debug, PartialEq)]
pub struct OsviewBar {
    pub title: String,
    pub legend: Vec<OsviewKey>,
    pub segments: Vec<OsviewSegment>,
}

/// Everything an app paints. With no `bars`, `note` says why.
#[derive(Clone, Debug, PartialEq)]
pub struct Osview {
    pub bars: Vec<OsviewBar>,
    pub ticks: u32,
    pub background: u32,
    pub bar_background: u32,
    /// What the numbers are, shown small under the bars.
    pub caption: String,
    pub note: Option<String>,
}

pub const CAPTION: &str = "Emulated CPU load, measured on the host, not the guest";

impl Osview {
    /// The picture for `report`. `prev`, the picture last painted, is blended
    /// in at [`ATTACK`] for each bar it has a match for (same title and legend).
    pub fn from_cpu_load(report: &CpuLoadReport, prev: Option<&Osview>) -> Osview {
        let bars = report
            .cpus
            .iter()
            .map(|c| {
                let user = (c.user_pct / 100.0).clamp(0.0, 1.0);
                let sys = (c.sys_pct / 100.0).clamp(0.0, 1.0 - user);
                let title = format!("CPU {} host time", c.cpu);
                let mut bar = OsviewBar {
                    legend: vec![
                        OsviewKey {
                            label: "user".into(),
                            rgb: USER,
                        },
                        OsviewKey {
                            label: "sys".into(),
                            rgb: SYS,
                        },
                        OsviewKey {
                            label: "spare".into(),
                            rgb: SPARE,
                        },
                    ],
                    segments: [(user, USER), (sys, SYS), (1.0 - user - sys, SPARE)]
                        .map(|(fraction, rgb)| OsviewSegment { fraction, rgb })
                        .to_vec(),
                    title,
                };
                if let Some(old) = prev.and_then(|p| p.bars.iter().find(|b| b.title == bar.title)) {
                    for (new, old) in bar.segments.iter_mut().zip(&old.segments) {
                        new.fraction = old.fraction + (new.fraction - old.fraction) * ATTACK;
                    }
                }
                bar
            })
            .collect::<Vec<_>>();
        let note = bars.is_empty().then(|| {
            report
                .unavailable
                .clone()
                .unwrap_or_else(|| "measuring…".into())
        });
        Osview {
            bars,
            ticks: TICKS,
            background: BACKGROUND,
            bar_background: BAR_BACKGROUND,
            caption: CAPTION.into(),
            note,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facade::{CpuLoad, CpuLoadSource};

    fn report(cpus: &[(f32, f32)], why: Option<&str>) -> CpuLoadReport {
        CpuLoadReport {
            source: CpuLoadSource::Host,
            cpus: cpus
                .iter()
                .enumerate()
                .map(|(i, &(u, s))| CpuLoad {
                    cpu: i as u32,
                    user_pct: u,
                    sys_pct: s,
                })
                .collect(),
            unavailable: why.map(String::from),
        }
    }

    fn fractions(b: &OsviewBar) -> Vec<f32> {
        b.segments.iter().map(|s| s.fraction).collect()
    }

    #[test]
    fn a_bar_is_user_sys_and_the_spare_rest() {
        let v = Osview::from_cpu_load(&report(&[(30.0, 10.0), (60.0, 5.0)], None), None);
        assert_eq!(v.bars.len(), 2);
        assert_eq!(v.bars[1].title, "CPU 1 host time");
        let f = fractions(&v.bars[1]);
        assert!(
            (f[0] - 0.60).abs() < 1e-6 && (f[1] - 0.05).abs() < 1e-6 && (f[2] - 0.35).abs() < 1e-6,
            "{f:?}"
        );
        let names: Vec<_> = v.bars[0].legend.iter().map(|k| k.label.as_str()).collect();
        assert_eq!(names, ["user", "sys", "spare"]);
        assert_eq!(v.note, None);
    }

    #[test]
    fn segments_never_overfill_the_bar() {
        let v = Osview::from_cpu_load(&report(&[(90.0, 40.0)], None), None);
        let f = fractions(&v.bars[0]);
        assert!(
            (f.iter().sum::<f32>() - 1.0).abs() < 1e-6 && f.iter().all(|x| *x >= 0.0),
            "{f:?}"
        );
    }

    #[test]
    fn a_bar_moves_halfway_to_the_new_sample() {
        let first = Osview::from_cpu_load(&report(&[(0.0, 0.0)], None), None);
        let next = Osview::from_cpu_load(&report(&[(80.0, 0.0)], None), Some(&first));
        assert!((fractions(&next.bars[0])[0] - 0.4).abs() < 1e-6);
    }

    #[test]
    fn with_no_bars_the_note_says_why_or_that_it_is_measuring() {
        assert_eq!(
            Osview::from_cpu_load(&report(&[], None), None)
                .note
                .as_deref(),
            Some("measuring…")
        );
        assert_eq!(
            Osview::from_cpu_load(&report(&[], Some("no sampler")), None)
                .note
                .as_deref(),
            Some("no sampler")
        );
    }
}
