//! The QEMU adapter: the product's machine catalogue as QEMU reports it,
//! QEMU's command line, its control channel and the tools that create a
//! machine's stores. The neutral model is in `origami-machine`, which does
//! not depend on this crate.

use origami_machine::addressing::Addresses;
use origami_machine::{MachineFile, Machines, Offering, Result};
use std::path::Path;

pub mod audio;
pub mod catalogue;
pub mod control;
pub mod engine;
pub mod graphics;
pub mod init;
#[cfg(test)]
mod preset_tests;
pub mod runtime;
pub mod settings;
pub mod shares;

/// The engine id this adapter's data is keyed by in the catalogue.
pub const ENGINE: &str = "qemu";

pub use catalogue::{offering_data, OfferingData};
pub use control::connect_console;
pub use engine::QemuEngine;
pub use init::InitTool;
pub use runtime::Runtime;

/// The machine list of the QEMU that launches guests.
pub fn machines(runtime: &Runtime) -> Result<Machines> {
    catalogue::load(&runtime.qemu())
}

/// `path` as QEMU reads it in an option value, where a comma is doubled.
pub fn qemu_path_option(path: &Path) -> String {
    path.display().to_string().replace(',', ",,")
}

/// The `-netdev user` options that set the network up.
pub fn user_netdev(addresses: &Addresses) -> String {
    format!(
        "user,id=net0,net={},host={},dhcpstart={}",
        addresses.cidr(),
        addresses.host(),
        addresses.guest()
    )
}

/// Check `file` against the neutral rules, then QEMU's: the extra arguments
/// it carries must not take over the options Origami drives the guest with.
pub fn validate<'a>(
    machines: &'a Machines,
    dir: &Path,
    file: &MachineFile,
) -> Result<&'a Offering> {
    let offering = origami_machine::validate(machines, dir, file)?;
    runtime::check_extra_args(&file.engine.extra_args())?;
    Ok(offering)
}

/// A hand-trimmed excerpt of a `query-sgi-machines` reply: one offering of
/// each kind the unit tests exercise, translated as a launch would.
#[cfg(test)]
pub(crate) fn test_machines() -> Machines {
    let wire =
        serde_json::from_str(include_str!("../../../tests/fixtures/sgi-machines.json")).unwrap();
    catalogue::translate(wire).unwrap()
}

/// Shell-script stand-ins for QEMU and its init tool.
#[cfg(all(test, unix))]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_network_names_the_subnet_host_and_first_guest_address() {
        let a = Addresses::default();
        assert_eq!(
            user_netdev(&a),
            "user,id=net0,net=10.98.0.0/24,host=10.98.0.2,dhcpstart=10.98.0.65"
        );
        let a = Addresses::parse("10.42.0.0/24").unwrap();
        assert!(user_netdev(&a).contains("net=10.42.0.0/24,host=10.42.0.2"));
    }

    #[test]
    fn a_comma_in_a_path_is_doubled() {
        assert_eq!(qemu_path_option(Path::new("a,b")), "a,,b");
    }
}
