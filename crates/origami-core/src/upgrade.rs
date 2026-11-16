//! Upgrading a machine made by an earlier Origami.

use crate::report::{collect, Incomplete};
use crate::{Kind, Report};
use origami_engine::{Engine, EngineStores};
use std::path::Path;

/// Upgrade the stopped machine in `dir`. `prom` and `io_prom` are local
/// firmware for a machine that needs it, instead of a download. If the
/// machine file is rewritten but a later step fails, the error carries the
/// notes so far.
pub fn upgrade(
    engine: &dyn Engine,
    dir: &Path,
    prom: Option<&Path>,
    io_prom: Option<&Path>,
) -> Result<Report, Incomplete> {
    collect(|report| {
        let machines = engine.catalogue()?;
        let upgraded = origami_machine::migrate::upgrade(
            &EngineStores(engine),
            dir,
            &machines,
            prom,
            io_prom,
        )?;
        // A machine made before emu-ids gets one, and keeps its MAC.
        let emu_id = match origami_machine::emu_id::add_to_machine(dir, &machines) {
            Ok(emu_id) => emu_id,
            Err(error) => {
                // machine.toml is already rewritten; do not lose what it says.
                for note in upgraded.iter().flatten() {
                    report.push(Kind::Note, note);
                }
                return Err(error);
            }
        };
        if upgraded.is_none() && emu_id.is_none() {
            report.result(format!("{} is current; it needs no upgrade", dir.display()));
        } else {
            report.result(format!("upgraded {}", dir.display()));
        }
        for note in upgraded.into_iter().flatten() {
            let kind = if origami_machine::migrate::is_bookkeeping(&note) {
                Kind::Note
            } else {
                Kind::GuestNote
            };
            report.push(kind, note);
        }
        for note in emu_id.into_iter().flatten() {
            report.push(Kind::Note, note);
        }
        Ok(())
    })
}
