//! The conformance suite for display protocol version 4 producers: it checks
//! the contract over the socket of any producer started with `qemu.test` on.

pub mod checks;
pub mod client;

use checks::{Ctx, CHECKS};
use client::{Opts, Target, V};
use origami_display::v4::testext::pattern;
use std::time::{Duration, Instant};

pub enum Outcome {
    Pass,
    Fail(String),
    /// The check could not reach the state it tests; not a failure.
    Skip(String),
}

pub struct Report {
    pub results: Vec<(&'static str, Outcome, Duration)>,
    pub diagnostics: Vec<String>,
}

impl Report {
    pub fn failures(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.1, Outcome::Fail(_)))
            .count()
    }
}

pub fn context(t: &Target) -> Result<Ctx, String> {
    let v = V::connect(t, &Opts::default().no_input())?;
    let h = v.head(0)?;
    Ok(Ctx {
        t: t.clone(),
        w: h.width,
        h: h.height,
    })
}

/// Run every mandatory check whose name contains `filter`, then the diagnostics.
pub fn run(
    t: &Target,
    filter: &str,
    diagnostics: bool,
    mut progress: impl FnMut(&str, &Outcome, Duration),
) -> Result<Report, String> {
    let ctx = context(t)?;
    let mut results = vec![];
    for (name, f) in CHECKS.iter().filter(|(n, _)| n.contains(filter)) {
        let t0 = Instant::now();
        let r = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&ctx))) {
            Ok(Ok(())) => Outcome::Pass,
            Ok(Err(e)) => match e.strip_prefix("skip: ") {
                Some(why) => Outcome::Skip(why.to_string()),
                None => Outcome::Fail(e),
            },
            Err(_) => Outcome::Fail("panicked".into()),
        };
        progress(name, &r, t0.elapsed());
        results.push((*name, r, t0.elapsed()));
    }
    let diagnostics = if diagnostics { diagnose(&ctx) } else { vec![] };
    Ok(Report {
        results,
        diagnostics,
    })
}

/// Things the contract says a producer should do; reported, never failed.
fn diagnose(c: &Ctx) -> Vec<String> {
    let mut out = vec![];
    let r = (|| -> Result<(), String> {
        let mut v = V::connect(&c.t, &Opts::default().no_input())?;
        let s0 = v.stats()?;
        let end = Instant::now() + Duration::from_secs(5);
        let mut i = 0;
        while Instant::now() < end {
            v.pat(0, pattern::SOLID, 9, i)?;
            i += 1;
            std::thread::sleep(Duration::from_millis(5));
        }
        let s1 = v.stats()?;
        out.push(format!(
            "samples across 5 s of steps with nobody waiting: {} (should be 0)",
            s1.samples - s0.samples
        ));
        let (w0, t0) = (v.stats()?.wakeups, Instant::now());
        std::thread::sleep(Duration::from_secs(1));
        let w1 = v.stats()?.wakeups;
        out.push(format!(
            "sampling wake-ups per second idle: {:.1}",
            (w1 - w0) as f64 / t0.elapsed().as_secs_f64()
        ));
        v.pat(0, pattern::LINES, 9, 0)?;
        v.get(0, false)?.release_quiet();
        v.pat(0, pattern::LINES, 9, 1)?;
        let f = v.get(0, false)?;
        out.push(format!(
            "damage of a one-in-seven-lines change: {}",
            match &f.frame.damage {
                origami_display::v4::wire::Damage::Whole =>
                    "whole frame (should be rectangles)".to_string(),
                d => format!("{d:?}").chars().take(60).collect(),
            }
        ));
        Ok(())
    })();
    if let Err(e) = r {
        out.push(format!("diagnostics did not complete: {e}"));
    }
    out
}
