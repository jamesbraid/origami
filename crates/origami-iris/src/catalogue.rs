//! The machines iris runs, in the product's neutral form.
//!
//! iris lists no machines in a form we can read, so this is authored here from
//! its profile names and bank rules (`MachineProfile` and `bank_sizes` in its
//! `config.rs`).

use crate::ENGINE;
use origami_engine::Catalogue;
use serde::Deserialize;
use serde_json::json;

/// What the iris adapter needs of an offering: the value of `engine.iris`.
#[derive(Clone, Debug, Deserialize)]
pub struct Data {
    /// iris's `[machine] profile`.
    pub profile: String,
    /// iris's `[machine] cpu`.
    pub cpu: String,
    /// The RAM bank sizes in MiB iris accepts, ascending.
    pub bank_sizes: Vec<u32>,
    /// How many banks the machine fills.
    pub banks: usize,
    /// The serial channel that is the PROM console: `a` or `b`.
    pub console: String,
    /// The graphics board iris is told to fit, for a machine whose board is
    /// not the default Newport.
    #[serde(default)]
    pub board: Option<String>,
}

pub fn data(offering: &origami_machine::Offering) -> origami_engine::Result<Data> {
    let value = offering
        .engine
        .get(ENGINE)
        .ok_or_else(|| format!("{} has no data for iris", offering.id))?;
    Ok(serde_json::from_value(value.clone())?)
}

/// Fill `banks` from bank 0, largest size first, with `mib` in all; None if
/// the sizes cannot make it.
pub fn banks_for(mib: u32, sizes: &[u32], banks: usize) -> Option<Vec<u32>> {
    let mut left = mib;
    let mut out = Vec::new();
    for _ in 0..banks {
        let Some(&size) = sizes.iter().rev().find(|&&size| size <= left) else {
            break;
        };
        out.push(size);
        left -= size;
    }
    (left == 0 && !out.is_empty()).then_some(out)
}

/// The memory totals a machine accepts, from the sizes iris takes.
fn accepted(candidates: &[u32], sizes: &[u32], banks: usize) -> Vec<u32> {
    candidates
        .iter()
        .copied()
        .filter(|&mib| banks_for(mib, sizes, banks).is_some())
        .collect()
}

pub fn catalogue() -> Catalogue {
    let sizes = [8, 16, 32, 64, 128];
    let gio = |id: &str, product: &str, profile: &str, storage: serde_json::Value| {
        json!({
            "id": id,
            "product": product,
            "topology": product,
            "nodes": 1,
            "cpus": 1,
            "cpus-per-node": [1],
            "hostid-source": "address",
            "processor": {
                "id": "r4400-150mhz",
                "part": "R4400",
                "clock-hz": 150_000_000u64,
                "cache-mib": 1,
            },
            "memory-per-node-mib": {
                "accepted": accepted(&[32, 64, 128, 256], &sizes, 2),
                "default": 128,
            },
            "storage": storage,
            "scsi-adapters": [{"bus": "scsi.0", "targets": [1, 2, 3, 4, 5, 6, 7]}],
            "consoles": [
                {"kind": "console", "node": 0, "name": "tty1"},
                {"kind": "second-port", "node": 0, "name": "tty2"},
            ],
            "networks": [{"kind": "seeq8003-ethernet", "interface": "ec0", "default": true}],
            // 6.5.23 and later do not install on the Indy and Indigo2.
            "irix-releases": ["6.5.22", "6.2", "5.3"],
            "engines": [ENGINE],
            "engine": {ENGINE: {
                "profile": profile,
                "cpu": "r4400",
                "bank_sizes": sizes,
                "banks": 2,
                "console": "b",
                "display": true,
            }},
        })
    };
    let octane_sizes = [64, 128, 256, 512];
    // The id is not octane2.1: that is QEMU's Octane2, and an id names one
    // offering across the engines.
    let octane2 = json!({
        "id": "octane2.1.iris",
        "product": "octane2",
        "topology": "octane2",
        "nodes": 1,
        "cpus": 1,
        "cpus-per-node": [1],
        "hostid-source": "address",
        "processor": {
            "id": "r10000-195mhz",
            "part": "R10000",
            "clock-hz": 195_000_000u64,
            "cache-mib": 1,
        },
        "memory-per-node-mib": {
            "accepted": accepted(&[128, 256, 512, 1024], &octane_sizes, 2),
            "default": 256,
        },
        "storage": [
            {"name": "nvram0", "size": crate::nvram::OCTANE_SIZE, "read-only": false, "initial": "zero"},
        ],
        "scsi-adapters": [{"bus": "scsi.0", "targets": [1, 2, 3, 4, 5, 6, 7]}],
        "consoles": [
            {"kind": "console", "node": 0, "name": "ioc3_a"},
            {"kind": "second-port", "node": 0, "name": "ioc3_b"},
        ],
        "networks": [{"kind": "ioc3-ethernet", "interface": "ef0", "default": true}],
        "engines": [ENGINE],
        "engine": {ENGINE: {
            "profile": "octane_ip30",
            "cpu": "r10000_195",
            "bank_sizes": octane_sizes,
            "banks": 2,
            "console": "a",
            "display": true,
            "board": "v12",
        }},
    });
    let indy = gio(
        "indy.1",
        "indy",
        "indy_ip24",
        json!([{"name": "nvram", "size": crate::nvram::SIZE, "read-only": false, "initial": "indy-nvram"}]),
    );
    let indigo2 = gio(
        "indigo2.1",
        "indigo2",
        "indigo2_ip22",
        json!([{"name": "nveeprom", "size": 256, "read-only": false, "initial": "indigo2-eeprom"}]),
    );
    serde_json::from_value(json!({"version": 1, "offerings": [indy, indigo2, octane2]}))
        .expect("the iris catalogue is well formed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banks_fill_largest_first_and_refuse_what_they_cannot_make() {
        let sizes = [8, 16, 32, 64, 128];
        assert_eq!(banks_for(256, &sizes, 2), Some(vec![128, 128]));
        assert_eq!(banks_for(192, &sizes, 2), Some(vec![128, 64]));
        assert_eq!(banks_for(32, &sizes, 2), Some(vec![32]));
        assert_eq!(banks_for(384, &sizes, 2), None);
        assert_eq!(banks_for(4, &sizes, 2), None);
    }

    #[test]
    fn every_iris_offering_says_it_has_a_display() {
        for offering in &catalogue().offerings {
            assert_eq!(offering.engine[ENGINE]["display"], true, "{}", offering.id);
        }
    }

    #[test]
    fn the_indy_and_indigo2_install_nothing_after_6_5_22() {
        use origami_machine::install::releases_for;
        let names =
            |offering| -> Vec<&str> { releases_for(offering).iter().map(|r| r.name).collect() };
        let catalogue = catalogue();
        for offering in catalogue
            .offerings
            .iter()
            .filter(|o| o.product != "octane2")
        {
            assert_eq!(names(offering), ["6.5.22", "6.2", "5.3"], "{}", offering.id);
        }
        let octane2 = catalogue.offerings.iter().find(|o| o.product == "octane2");
        assert_eq!(names(octane2.unwrap()), ["6.5.30", "6.5.22"]);
    }

    #[test]
    fn every_offering_has_a_plain_label() {
        let labels: Vec<_> = catalogue()
            .offerings
            .iter()
            .map(origami_machine::listing::offering_label)
            .collect();
        assert_eq!(labels, ["Indy, 1 CPU", "Indigo2, 1 CPU", "Octane2, 1 CPU"]);
    }

    #[test]
    fn every_offering_names_its_iris_data() {
        let catalogue = catalogue();
        for offering in &catalogue.offerings {
            let data = data(offering).unwrap();
            assert!(offering.memory.accepted.contains(&offering.memory.default));
            for &mib in &offering.memory.accepted {
                assert!(banks_for(mib, &data.bank_sizes, data.banks).is_some());
            }
        }
    }
}
