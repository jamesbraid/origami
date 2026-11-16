//! The engine-neutral machine: what a machine is, with nothing in it that
//! only one engine reads except [`EngineChoice::private`].
//!
//! Format 4 machine files describe QEMU machines, so [`Machine::from_file`]
//! and [`Machine::to_file`] are the bridge until the file format itself is
//! neutral. The pair is lossless for any file `validate` accepts.

use crate::graphics::Pipe;
use crate::{
    profiles, Drive, Identity, MachineFile, Machines, Network, Offering, Result, Share,
    MACHINE_FORMAT,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The engine id format 4 machine files run under.
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
    pub engine: EngineChoice,
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
#[derive(Clone, Debug, PartialEq)]
pub struct EngineChoice {
    pub id: String,
    /// Opaque to everything but the engine's adapter. QEMU's holds
    /// `extra-args`, the unsupported options appended to its command line.
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

    fn extra_args(&self) -> Vec<String> {
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
            engine: EngineChoice::qemu(&file.qemu.extra_args),
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
            engine: EngineChoice::qemu(&[]),
        }
    }

    /// The format 4 file for this machine.
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
            qemu: crate::Qemu {
                extra_args: self.engine.extra_args(),
            },
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
            qemu: crate::Qemu {
                extra_args: vec!["-global".into(), "x.y=z".into()],
            },
        };
        let machine = Machine::from_file(&machines, &file).unwrap();
        assert_eq!(machine.engine.id, QEMU_ENGINE);
        assert_eq!(machine.memory.per_node_mib, 256);
        assert_eq!(machine.stores.0.len(), offering.storage.len());
        assert_eq!(
            toml::to_string(&machine.to_file()).unwrap(),
            toml::to_string(&file).unwrap()
        );
        file.format = 3;
        let error = Machine::from_file(&machines, &file).unwrap_err();
        assert_eq!(error.to_string(), "unsupported machine format 3");
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
}
