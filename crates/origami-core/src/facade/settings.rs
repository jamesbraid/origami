//! What a machine's file says, as plain data for a settings pane.

use crate::Loaded;
use origami_machine::{PortForward, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipeSetting {
    pub slot: String,
    pub product: String,
    pub family: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveSetting {
    pub name: String,
    /// `disk`, `cdrom` or `tape`.
    pub kind: String,
    pub bus: u32,
    pub target: u32,
    pub image: String,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareSetting {
    pub name: String,
    pub host_dir: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MachineSettings {
    pub nodes: u32,
    /// The processor module and its part, such as `R12000`. Fixed at creation.
    pub processor: String,
    pub processor_part: String,
    pub cpus_per_node: u32,
    /// The CPU counts per node the machine's topology and module take.
    pub cpus_per_node_allowed: Vec<u32>,
    pub memory_per_node_mib: u32,
    pub memory_allowed_mib: Vec<u32>,
    pub pipes: Vec<PipeSetting>,
    pub drives: Vec<DriveSetting>,
    /// `user`, `private`, `none`, `tap` or `vmnet`.
    pub network_mode: String,
    pub forwards: Vec<PortForward>,
    pub shares: Vec<ShareSetting>,
}

pub(crate) fn read(loaded: &Loaded) -> Result<MachineSettings> {
    let (file, offer) = (&loaded.file, loaded.offering()?);
    Ok(MachineSettings {
        nodes: file.machine.nodes,
        processor: offer.processor.id.clone(),
        processor_part: offer.processor.part.clone(),
        cpus_per_node: file.machine.cpus_per_node,
        cpus_per_node_allowed: crate::change::cpu_counts(&loaded.catalogue, offer),
        memory_per_node_mib: file
            .machine
            .memory_per_node
            .trim_end_matches("MiB")
            .parse()
            .unwrap_or(0),
        memory_allowed_mib: offer.memory.accepted.clone(),
        pipes: file
            .pipe
            .iter()
            .map(|p| PipeSetting {
                slot: p.slot.clone(),
                product: p.product.clone(),
                family: p.family.clone(),
            })
            .collect(),
        drives: file
            .drive
            .iter()
            .map(|d| DriveSetting {
                name: d.name.clone(),
                kind: d.kind.clone(),
                bus: d.bus,
                target: d.target,
                image: d.image.clone(),
                read_only: d.read_only,
            })
            .collect(),
        network_mode: file.network.mode.clone(),
        forwards: file.network.forward.clone(),
        shares: file
            .share
            .iter()
            .map(|s| ShareSetting {
                name: s.name.clone(),
                host_dir: s.path.to_string_lossy().into_owned(),
            })
            .collect(),
    })
}
