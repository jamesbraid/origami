//! Settings a machine takes once its PROM has run for the first time.

use crate::inspect::open;
use crate::Report;
use origami_engine::Engine;
use origami_machine::{control, nvram, write_machine, Result};
use std::path::Path;

/// Apply the first-boot settings of the stopped machine in `dir` if its
/// PROM has initialised the NVRAM, and forget them. Call it before every
/// start; it changes nothing once there is nothing pending or while the
/// machine runs.
pub fn apply_first_boot(engine: &dyn Engine, dir: &Path) -> Result<Report> {
    let mut report = Report::default();
    let pending = origami_machine::read_machine(dir)?
        .firstboot
        .is_some_and(|firstboot| !firstboot.nvram.is_empty());
    // A running machine keeps its NVRAM; the start reports it as running.
    if !pending || control::is_locked(dir)? {
        return Ok(report);
    }
    let _lock = control::lock_for_edit(dir, "applying its first-boot settings")?;
    let mut loaded = open(engine, dir)?;
    let offering = loaded.offering()?.clone();
    let applied = nvram::apply_first_boot(dir, &offering, &mut loaded.file)?;
    if applied.is_empty() {
        return Ok(report);
    }
    write_machine(dir, &loaded.file)?;
    for (name, value) in applied {
        report.detail(format!("{name}={value}"));
    }
    Ok(report)
}
