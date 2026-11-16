//! The engine-neutral machine: what a machine is, with nothing in it that
//! only one engine reads except [`EngineChoice::private`].
//!
//! [`Machine::from_file`] and [`Machine::to_file`] are the bridge between
//! this and the format 5 machine file, whose sections follow the same
//! split. The pair is lossless for any file `validate` accepts.

use crate::graphics::Pipe;
use crate::{
    profiles, Drive, Identity, MachineFile, Machines, Network, Offering, Result, Share,
    MACHINE_FORMAT,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The engine id machine files made before engines were named run under.
pub const QEMU_ENGINE: &str = "qemu";

/// A machine as the product describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    /// `origin200`, `octane2`, `o2` and so on: the offering's product.
    pub product: String,
    /// The hardware arrangement; absent from a machine made before
    /// topologies were named.
    pub topology: Option<String>,
    /// CPUs per node, where the file lists them; empty means every node
    /// has `processor.cpus_per_node`.
    pub population: Vec<u32>,
    pub nodes: u32,
    pub processor: ProcessorChoice,
    pub memory: MemoryChoice,
    pub graphics: Vec<Pipe>,
    pub stores: StoreSet,
    pub drives: Vec<Drive>,
    pub network: Network,
    pub shares: Vec<Share>,
    /// The serial line that is the console, where it is not the offering's
    /// primary one.
    pub console: Option<String>,
    pub identity: Option<Identity>,
    /// Neutral setting names to values.
    pub settings: BTreeMap<String, String>,
    /// The catalogue's firmware images that seeded the stores, where known.
    pub firmware: Option<FirmwareRecord>,
    /// Settings still to apply once the PROM has initialised the machine.
    pub firstboot: Option<FirstBoot>,
    pub engine: EngineChoice,
}

/// Settings a new machine cannot take until its PROM has run once, because
/// the PROM writes its environment into a blank NVRAM at its first boot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirstBoot {
    /// PROM environment variables, as `origami nvram` names them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub nvram: BTreeMap<String, String>,
}

/// The firmware images that seeded a machine's stores, by the catalogue's
/// kind name and digest. Never derived from a store's bytes: a used flash
/// holds the guest's own PROM updates.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareRecord {
    #[serde(default, rename = "boot-prom", skip_serializing_if = "Option::is_none")]
    pub boot_prom: Option<FirmwareImage>,
    #[serde(default, rename = "io-prom", skip_serializing_if = "Option::is_none")]
    pub io_prom: Option<FirmwareImage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareImage {
    /// The catalogue's name for the kind of image, such as `ip30-prom`.
    pub kind: String,
    pub sha256: String,
}

impl FirmwareRecord {
    /// The catalogue rows the images match, by digest; None when neither
    /// does, as when the user supplied their own PROM.
    pub fn of(boot_prom: &std::path::Path, io_prom: Option<&std::path::Path>) -> Option<Self> {
        let manifest = crate::assets::manifest().ok()?;
        Self::from_manifest(&manifest, boot_prom, io_prom)
    }

    fn from_manifest(
        manifest: &crate::assets::Manifest,
        boot_prom: &std::path::Path,
        io_prom: Option<&std::path::Path>,
    ) -> Option<Self> {
        let row = |path: &std::path::Path| {
            let sha256 = crate::sha256_file(path).ok()?;
            let prom = manifest.proms.iter().find(|prom| prom.sha256 == sha256)?;
            Some(FirmwareImage {
                kind: prom.kind.clone(),
                sha256,
            })
        };
        let record = Self {
            boot_prom: row(boot_prom),
            io_prom: io_prom.and_then(row),
        };
        (record != Self::default()).then_some(record)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessorChoice {
    pub cpus_per_node: u32,
    /// The processor module, where the topology offers a choice; absent
    /// means its default.
    pub module: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryChoice {
    pub per_node_mib: u32,
}

/// A machine's stores: catalogue store name to its file, relative to the
/// machine directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreSet(pub BTreeMap<String, PathBuf>);

impl StoreSet {
    /// The stores `offering` lists, each in its place under `state/`.
    pub fn of(offering: &Offering) -> Self {
        Self(
            offering
                .storage
                .iter()
                .map(|item| {
                    let file = format!("{}.raw", item.name);
                    (item.name.clone(), PathBuf::from("state").join(file))
                })
                .collect(),
        )
    }

    pub fn get(&self, name: &str) -> Option<&PathBuf> {
        self.0.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &PathBuf)> {
        self.0.iter()
    }
}

/// Which engine runs the machine, and the settings only that engine reads.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EngineChoice {
    pub id: String,
    /// Opaque to everything but the engine's adapter. QEMU's holds
    /// `extra-args`, the unsupported options appended to its command line.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub private: toml::Table,
}

impl EngineChoice {
    pub fn qemu(extra_args: &[String]) -> Self {
        let mut private = toml::Table::new();
        if !extra_args.is_empty() {
            let args = extra_args.iter().cloned().map(toml::Value::String);
            private.insert("extra-args".into(), toml::Value::Array(args.collect()));
        }
        Self {
            id: QEMU_ENGINE.into(),
            private,
        }
    }

    /// QEMU's unsupported options, appended after every option the product
    /// generates. Empty for another engine.
    pub fn extra_args(&self) -> Vec<String> {
        self.private
            .get("extra-args")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|arg| arg.as_str().map(str::to_owned))
            .collect()
    }
}

impl Machine {
    /// The machine `file` describes. The checks run first are the ones
    /// `validate` runs first, with the same messages.
    pub fn from_file(machines: &Machines, file: &MachineFile) -> Result<Self> {
        if file.format != MACHINE_FORMAT {
            return Err(format!("unsupported machine format {}", file.format).into());
        }
        let per_node_mib = file
            .machine
            .memory_per_node
            .strip_suffix("MiB")
            .ok_or("memory_per_node must use MiB")?
            .parse::<u32>()?;
        let offering = crate::offering(machines, &file.machine)?;
        Ok(Self {
            product: file.machine.model.clone(),
            topology: file.machine.topology.clone(),
            population: file.machine.population.clone(),
            nodes: file.machine.nodes,
            processor: ProcessorChoice {
                cpus_per_node: file.machine.cpus_per_node,
                module: file.machine.processor.clone(),
            },
            memory: MemoryChoice { per_node_mib },
            graphics: file.pipe.clone(),
            stores: StoreSet::of(offering),
            drives: file.drive.clone(),
            network: file.network.clone(),
            shares: file.share.clone(),
            console: file.machine.console.clone(),
            identity: file.identity.clone(),
            settings: file.machine.inputs.clone(),
            firmware: file.firmware.clone(),
            firstboot: file.firstboot.clone(),
            engine: file.engine.clone(),
        })
    }

    /// A new machine of `offering`, with no drives and the default network.
    pub fn from_offering(
        offering: &Offering,
        memory_per_node: Option<u32>,
        identity: Option<Identity>,
        graphics: Vec<Pipe>,
        settings: BTreeMap<String, String>,
    ) -> Self {
        Self {
            product: offering.product.clone(),
            topology: Some(offering.topology.clone()),
            population: offering.cpus_per_node.clone(),
            nodes: offering.nodes,
            processor: ProcessorChoice {
                cpus_per_node: offering.cpus_per_node[0],
                module: profiles::selects_processor(offering)
                    .then(|| offering.processor.id.clone()),
            },
            memory: MemoryChoice {
                per_node_mib: memory_per_node.unwrap_or(profiles::default_memory(offering)),
            },
            graphics,
            stores: StoreSet::of(offering),
            drives: vec![],
            network: Network::default(),
            shares: vec![],
            console: None,
            identity,
            settings,
            firmware: None,
            firstboot: None,
            engine: EngineChoice::qemu(&[]),
        }
    }

    /// The machine file for this machine.
    pub fn to_file(&self) -> MachineFile {
        MachineFile {
            format: MACHINE_FORMAT,
            machine: crate::Machine {
                topology: self.topology.clone(),
                population: self.population.clone(),
                inputs: self.settings.clone(),
                model: self.product.clone(),
                nodes: self.nodes,
                cpus_per_node: self.processor.cpus_per_node,
                processor: self.processor.module.clone(),
                memory_per_node: format!("{}MiB", self.memory.per_node_mib),
                console: self.console.clone(),
            },
            identity: self.identity.clone(),
            network: self.network.clone(),
            pipe: self.graphics.clone(),
            drive: self.drives.clone(),
            share: self.shares.clone(),
            firmware: self.firmware.clone(),
            firstboot: self.firstboot.clone(),
            engine: self.engine.clone(),
        }
    }

    /// The file `store` is kept in, under the machine directory `dir`.
    pub fn store_path(&self, dir: &crate::dir::MachineDir, store: &str) -> Option<PathBuf> {
        self.stores.get(store).map(|file| dir.path().join(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stores;

    #[test]
    fn a_machine_file_round_trips_through_the_neutral_machine() {
        let machines = crate::test_machines();
        let offering = crate::preset(&machines, "origin200-1").unwrap();
        let mut file = MachineFile {
            format: MACHINE_FORMAT,
            machine: crate::Machine {
                topology: Some(offering.topology.clone()),
                population: offering.cpus_per_node.clone(),
                inputs: [("a".to_string(), "b".to_string())].into(),
                model: offering.product.clone(),
                nodes: offering.nodes,
                cpus_per_node: offering.cpus_per_node[0],
                processor: None,
                memory_per_node: "256MiB".into(),
                console: None,
            },
            identity: Some(Identity::new("08:00:69:12:34:56", Some(7))),
            network: Network::default(),
            pipe: vec![],
            drive: vec![],
            share: vec![],
            firmware: None,
            firstboot: None,
            engine: EngineChoice::qemu(&["-global".into(), "x.y=z".into()]),
        };
        let machine = Machine::from_file(&machines, &file).unwrap();
        assert_eq!(machine.engine.id, QEMU_ENGINE);
        assert_eq!(machine.memory.per_node_mib, 256);
        assert_eq!(machine.stores.0.len(), offering.storage.len());
        assert_eq!(
            toml::to_string(&machine.to_file()).unwrap(),
            toml::to_string(&file).unwrap()
        );
        file.format = 4;
        let error = Machine::from_file(&machines, &file).unwrap_err();
        assert_eq!(error.to_string(), "unsupported machine format 4");
        file.format = MACHINE_FORMAT;
        file.machine.memory_per_node = "256".into();
        let error = Machine::from_file(&machines, &file).unwrap_err();
        assert_eq!(error.to_string(), "memory_per_node must use MiB");
    }

    #[test]
    fn a_new_machine_has_the_offerings_stores_under_state() {
        let machines = crate::test_machines();
        let offering = crate::preset(&machines, "origin200-1").unwrap();
        let machine = Machine::from_offering(offering, None, None, vec![], BTreeMap::new());
        for item in &offering.storage {
            let dir = crate::dir::MachineDir::new("/m");
            let path = machine.store_path(&dir, &item.name).unwrap();
            assert_eq!(path, stores::state_path(dir.path(), item));
        }
    }

    #[test]
    fn the_firmware_record_names_the_catalogue_rows_by_digest() {
        let dir = tempfile::tempdir().unwrap();
        let (boot, io, own) = (
            dir.path().join("boot"),
            dir.path().join("io"),
            dir.path().join("own"),
        );
        std::fs::write(&boot, b"boot image").unwrap();
        std::fs::write(&io, b"io image").unwrap();
        std::fs::write(&own, b"my own prom").unwrap();
        let prom = |kind: &str, file: &std::path::Path| crate::assets::Prom {
            kind: kind.into(),
            path: format!("prom/{kind}"),
            size: 0,
            sha256: crate::sha256_file(file).unwrap(),
            version: String::new(),
        };
        let manifest = crate::assets::Manifest {
            base_url: String::new(),
            proms: vec![prom("ip27-prom", &boot), prom("ip27-io6", &io)],
        };
        let record = FirmwareRecord::from_manifest(&manifest, &boot, Some(&io)).unwrap();
        assert_eq!(record.boot_prom.unwrap().kind, "ip27-prom");
        assert_eq!(record.io_prom.unwrap().kind, "ip27-io6");
        // A PROM the user supplied that matches no row is not recorded, and
        // neither is a record of nothing.
        let record = FirmwareRecord::from_manifest(&manifest, &boot, Some(&own)).unwrap();
        assert!(record.io_prom.is_none());
        assert_eq!(FirmwareRecord::from_manifest(&manifest, &own, None), None);
    }
}
