//! Upgrade machines from earlier formats. Format 1 kept a copy of the boot
//! PROM and let the launcher build flash and NVRAM files under its own
//! names. Format 2 machines predate the module controller NVRAM store.
//! Formats 1 to 3 name their graphics with one word, which becomes the
//! pipes the catalogue describes. A current machine gains the flash store
//! each InfiniteReality pipe slot has had since QEMU gave it one.

use crate::{
    profiles, resolve, runtime, MachineFile, MachineInit, Machines, Offering, Result, StorageItem,
};
use std::fs;
use std::path::{Path, PathBuf};

/// The `format` a machine.toml's `text` declares.
pub fn format(text: &str) -> Option<i64> {
    toml::from_str::<toml::Table>(text)
        .ok()?
        .get("format")?
        .as_integer()
}

/// Bring the stopped machine in `dir` to the current format, reading PROMs
/// from `prom` and `io_prom` or verified downloads where the upgrade needs
/// them. Returns what changes for the guest, or None for a machine that is
/// already current, which is left untouched. On failure machine.toml is
/// unchanged.
pub fn upgrade(
    dir: &Path,
    machines: &Machines,
    prom: Option<&Path>,
    io_prom: Option<&Path>,
) -> Result<Option<Vec<String>>> {
    // A running machine, or another edit, holds this lock.
    let _lock = crate::control::lock_for_edit(dir, "upgrading it")?;
    let path = dir.join("machine.toml");
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let format = format(&text);
    let result = match format {
        Some(1) => from_format1(dir, machines, &text, prom, io_prom),
        Some(2) => from_format2(dir, machines, &text, prom, io_prom),
        Some(3) => from_format3(dir, machines, &text, prom, io_prom),
        Some(current) if current == i64::from(crate::MACHINE_FORMAT) => {
            let file: MachineFile = toml::from_str(&text)?;
            return add_pipe_flash(dir, machines, &file, prom, io_prom).map_err(|error| {
                format!(
                    "cannot add the pipe flash to {}: {error}; its state is unchanged",
                    dir.display()
                )
                .into()
            });
        }
        _ => return Err(format!("{} has an unsupported format", path.display()).into()),
    };
    result.map(Some).map_err(|error| {
        format!(
            "cannot upgrade the format {} machine {}: {error}; its machine.toml is unchanged",
            format.unwrap_or_default(),
            dir.display()
        )
        .into()
    })
}

/// QEMU's init tool creates the storage, then the flash and NVRAM contents
/// the machine already has replace the fresh ones. The old files stay where
/// they are.
fn from_format1(
    dir: &Path,
    machines: &Machines,
    text: &str,
    prom: Option<&Path>,
    io_prom: Option<&Path>,
) -> Result<Vec<String>> {
    let (file, own_prom, notes) = convert(dir, machines, text)?;
    let offering = crate::offering(machines, &file.machine)?;
    // Format 1 kept no IO PROM.
    let (prom, io_prom) = crate::firmware(offering, Some(prom.unwrap_or(&own_prom)), io_prom)?;
    let init = MachineInit {
        tool: &runtime::machine_init_path()?,
        boot_prom: &prom,
        io_prom: io_prom.as_deref(),
    };
    install(dir, &file, offering, &init)?;
    Ok(notes)
}

/// Create the module controller NVRAM files a format 2 machine lacks. Their
/// first contents depend only on the machine, not on what the guest saved,
/// so a fresh file is the one create would have made. Any other missing
/// file holds guest state that cannot be rebuilt.
fn from_format2(
    dir: &Path,
    machines: &Machines,
    text: &str,
    prom: Option<&Path>,
    io_prom: Option<&Path>,
) -> Result<Vec<String>> {
    let mut table: toml::Table = toml::from_str(text)?;
    let notes = upgrade_graphics(machines, &mut table)?;
    let file: MachineFile = toml::Value::Table(table).try_into()?;
    let offering = crate::offering(machines, &file.machine)?;
    let state = dir.join("state");
    let raw = |root: &Path, item: &StorageItem| root.join(format!("{}.raw", item.name));
    let missing: Vec<&StorageItem> = offering
        .storage
        .iter()
        .filter(|item| !raw(&state, item).is_file())
        .collect();
    // Only these hold nothing the guest saved, so a fresh file is right.
    if let Some(item) = missing
        .iter()
        .find(|item| item.initial != "controller-nvram" && item.initial != PIPE_FLASH)
    {
        return Err(format!(
            "missing machine state {}. Restore it from a backup",
            raw(&state, item).display()
        )
        .into());
    }
    // Left by an interrupted upgrade.
    let staging = dir.join("state.format3");
    let _ = fs::remove_dir_all(&staging);
    if !missing.is_empty() {
        // The init tool reads the machine's PROMs before it writes anything,
        // although these files do not depend on them.
        let (prom, io_prom) = crate::firmware(offering, prom, io_prom)?;
        let result = (|| -> Result<()> {
            let arguments = runtime::init_arguments(offering, &prom, io_prom.as_deref(), &staging);
            runtime::create_state(&runtime::machine_init_path()?, &arguments)?;
            for item in &missing {
                fs::rename(raw(&staging, item), raw(&state, item))?;
            }
            Ok(())
        })();
        let _ = fs::remove_dir_all(&staging);
        result?;
    }
    replace_file(dir, &file)?;
    Ok(notes)
}

/// The initial contents of an InfiniteReality pipe's flash store. Nothing in
/// it depends on the machine, so a fresh file is what create would have made.
const PIPE_FLASH: &str = "kona-flash";

/// Create the `<slot>-kona-flash` stores a current machine lacks. QEMU
/// refuses to reopen state without a store its catalogue lists, and a pipe's
/// flash holds only what the guest's ircombine stored there, which a machine
/// made before the store existed never could. Returns None when the machine
/// has every one.
fn add_pipe_flash(
    dir: &Path,
    machines: &Machines,
    file: &MachineFile,
    prom: Option<&Path>,
    io_prom: Option<&Path>,
) -> Result<Option<Vec<String>>> {
    let offering = crate::offering(machines, &file.machine)?;
    let state = dir.join("state");
    let raw = |root: &Path, item: &StorageItem| root.join(format!("{}.raw", item.name));
    let missing: Vec<&StorageItem> = offering
        .storage
        .iter()
        .filter(|item| item.initial == PIPE_FLASH && !raw(&state, item).is_file())
        .collect();
    if missing.is_empty() {
        return Ok(None);
    }
    // Left by an interrupted upgrade.
    let staging = dir.join("state.pipe-flash");
    let _ = fs::remove_dir_all(&staging);
    // The init tool reads the machine's PROMs before it writes anything,
    // although these files do not depend on them.
    let (prom, io_prom) = crate::firmware(offering, prom, io_prom)?;
    let result = (|| -> Result<()> {
        let arguments = runtime::init_arguments(offering, &prom, io_prom.as_deref(), &staging);
        runtime::create_state(&runtime::machine_init_path()?, &arguments)?;
        for item in &missing {
            fs::rename(raw(&staging, item), raw(&state, item))?;
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&staging);
    result?;
    Ok(Some(
        missing
            .iter()
            .map(|item| {
                format!(
                    "{}.raw holds the flash of that InfiniteReality pipe",
                    item.name
                )
            })
            .collect(),
    ))
}

/// Store each format 3 graphics value as the pipes it now is.
fn from_format3(
    dir: &Path,
    machines: &Machines,
    text: &str,
    prom: Option<&Path>,
    io_prom: Option<&Path>,
) -> Result<Vec<String>> {
    let mut table: toml::Table = toml::from_str(text)?;
    let mut notes = upgrade_graphics(machines, &mut table)?;
    let file: MachineFile = toml::Value::Table(table).try_into()?;
    let offering = crate::offering(machines, &file.machine)?;
    crate::graphics::validate(machines, &offering.topology, offering.nodes, &file.pipe)?;
    notes.extend(add_pipe_flash(dir, machines, &file, prom, io_prom)?.unwrap_or_default());
    replace_file(dir, &file)?;
    Ok(notes)
}

fn replace_file(dir: &Path, file: &MachineFile) -> Result<()> {
    let temporary = dir.join("machine.toml.upgrade");
    fs::write(&temporary, toml::to_string_pretty(file)?)?;
    fs::rename(temporary, dir.join("machine.toml"))?;
    Ok(())
}

/// Topologies the catalogue renamed, each applied once the catalogue no
/// longer lists the old name.
const RENAMED: &[(&str, &str)] = &[
    ("fuel-vpro", "fuel"),
    ("tezro-rack-base", "tezro-rack"),
    (
        "tezro-deskside-700mhz-4mib-serial-trial",
        "tezro-tower-multi",
    ),
    (
        "origin300-numalink-pci-expansion-experiment",
        "origin300-numalink-pci-expansion",
    ),
    (
        "origin300-pci-expansion-configuration-storage-experiment",
        "origin300-pci-expansion",
    ),
    (
        "origin300-pci-expansion-experiment",
        "origin300-pci-expansion",
    ),
    (
        "origin300-pci-expansion-pending-register-experiment",
        "origin300-pci-expansion",
    ),
    (
        "origin300-pci-expansion-unconnected-uart-experiment",
        "origin300-pci-expansion",
    ),
];

/// Topologies removed with nothing in their place, and why.
const REMOVED: &[(&str, &str)] = &[
    (
        "origin300-v12-direct",
        "a VPro V12 on an Origin 300 cable port is hardware SGI never built",
    ),
    (
        "origin300-vbrick",
        "an Origin 300 cabled to a V-brick was never an SGI configuration",
    ),
];

/// The topology `name` is now in the catalogue.
fn current_topology<'a>(machines: &Machines, name: &'a str) -> Result<&'a str> {
    if let Some((_, reason)) = REMOVED.iter().find(|(old, _)| *old == name) {
        return Err(format!("topology '{name}' was removed: {reason}").into());
    }
    let listed = |name: &str| machines.offerings.iter().any(|o| o.topology == name);
    Ok(RENAMED
        .iter()
        .find(|(old, new)| *old == name && !listed(old) && listed(new))
        .map_or(name, |(_, new)| new))
}

/// Replace the machine's `graphics` value in `table` with its pipes and
/// rename its topology where the catalogue did, then mark `table` current.
/// Returns what changes for the guest.
fn upgrade_graphics(machines: &Machines, table: &mut toml::Table) -> Result<Vec<String>> {
    let machine = table
        .get_mut("machine")
        .and_then(toml::Value::as_table_mut)
        .ok_or("no [machine] table")?;
    let graphics = match machine.remove("graphics") {
        Some(value) => value
            .as_str()
            .ok_or("graphics is not a string")?
            .to_string(),
        None => "none".into(),
    };
    let topology = machine
        .get("topology")
        .and_then(toml::Value::as_str)
        .ok_or("no machine topology")?
        .to_string();
    let topology = current_topology(machines, &topology)?.to_string();
    let nodes = machine
        .get("nodes")
        .and_then(toml::Value::as_integer)
        .and_then(|nodes| u32::try_from(nodes).ok())
        .ok_or("no machine node count")?;
    machine.insert("topology".into(), topology.clone().into());
    let (pipes, note) = crate::graphics::from_legacy(machines, &topology, nodes, &graphics)?;
    if !pipes.is_empty() {
        table.insert("pipe".into(), toml::Value::try_from(pipes)?);
    }
    table.insert("format".into(), i64::from(crate::MACHINE_FORMAT).into());
    Ok(note.map(String::from).into_iter().collect())
}

/// The current machine file for format 1 `text`, its boot PROM and what
/// changes for the guest.
fn convert(
    dir: &Path,
    machines: &Machines,
    text: &str,
) -> Result<(MachineFile, PathBuf, Vec<String>)> {
    let mut table: toml::Table = toml::from_str(text)?;
    let prom = table
        .remove("firmware")
        .and_then(|firmware| Some(resolve(dir, firmware.get("image")?.as_str()?)))
        .ok_or("no firmware image")?;
    // QEMU now builds the identity records and the adapter address from one
    // MAC, so the SPD and IO8 record paths go.
    let network_mac = table
        .get_mut("network")
        .and_then(toml::Value::as_table_mut)
        .and_then(|network| network.remove("mac"));
    let identity_mac = table
        .remove("identity")
        .and_then(|identity| identity.get("mac").cloned());
    if let Some(mac) = identity_mac.or(network_mac) {
        table.insert(
            "identity".into(),
            toml::Table::from_iter([("mac".into(), mac)]).into(),
        );
    }
    // Machines from before topologies named only the model, and Origami
    // 0.1.1 ran them on the topology this picks.
    if let Some(machine) = table
        .get_mut("machine")
        .and_then(toml::Value::as_table_mut)
        .filter(|machine| !machine.contains_key("topology"))
    {
        let model = machine.get("model").and_then(toml::Value::as_str);
        let nodes = machine.get("nodes").and_then(toml::Value::as_integer);
        let topology = match (model, nodes) {
            (Some("origin200"), Some(2)) => "origin200-dual",
            (Some("origin2000"), _) => "origin2000-rack",
            (model, _) => model.ok_or("no machine model")?,
        };
        machine.insert("topology".into(), topology.into());
    }
    // Fuel's board inputs took the QEMU property names.
    if let Some(inputs) = table
        .get_mut("machine")
        .and_then(|machine| machine.get_mut("inputs"))
        .and_then(toml::Value::as_table_mut)
    {
        *inputs = std::mem::take(inputs)
            .into_iter()
            .map(|(key, value)| (key.strip_prefix("fuel-").unwrap_or(&key).into(), value))
            .collect();
    }
    let notes = upgrade_graphics(machines, &mut table)?;
    Ok((toml::Value::Table(table).try_into()?, prom, notes))
}

/// Where format 1 kept the contents of storage item `name`.
fn old_state(dir: &Path, name: &str) -> Vec<PathBuf> {
    let state = dir.join("state");
    let node = name
        .strip_prefix("node")
        .and_then(|rest| rest.strip_suffix("-flash"))
        .and_then(|node| node.parse::<u32>().ok());
    match (node, name.strip_suffix("-clock")) {
        // Origin 2000 and Onyx2 numbered node flash from 1; Origin 300
        // kept a boot flash file.
        (Some(0), _) => vec![
            state.join("node-proms/node1.bin"),
            state.join("ip35-boot-flash.raw"),
        ],
        (Some(node), _) => vec![state.join(format!("node-proms/node{}.bin", node + 1))],
        (None, Some(nvram)) => vec![state.join(format!("{nvram}.raw.clock"))],
        (None, None) => vec![state.join(format!("{name}.raw"))],
    }
}

fn install(
    dir: &Path,
    file: &MachineFile,
    offering: &Offering,
    init: &MachineInit<'_>,
) -> Result<()> {
    profiles::validate_inputs(offering, &file.machine.inputs)?;
    let state = dir.join("state");
    let raw = |root: &Path, name: &str| root.join(format!("{name}.raw"));
    // Format 1 kept NVRAM under the name QEMU now uses. Only a file of the
    // item's size can stay; replacing another would lose what it holds.
    for item in &offering.storage {
        let target = raw(&state, &item.name);
        if fs::metadata(&target).is_ok_and(|m| m.len() != item.size) {
            return Err(format!(
                "{} is not {} bytes; move it aside to upgrade",
                target.display(),
                item.size
            )
            .into());
        }
    }
    let staging = dir.join("state.format2");
    // Left by an interrupted upgrade.
    let _ = fs::remove_dir_all(&staging);
    let result = (|| -> Result<()> {
        let arguments = runtime::init_arguments(offering, init.boot_prom, init.io_prom, &staging);
        runtime::create_state(init.tool, &arguments)?;
        let mut moves = vec![];
        for item in &offering.storage {
            let (fresh, target) = (raw(&staging, &item.name), raw(&state, &item.name));
            // A file of another size is not this item's contents.
            match old_state(dir, &item.name)
                .into_iter()
                .find(|old| fs::metadata(old).is_ok_and(|m| m.len() == item.size))
            {
                Some(old) if old == target => continue,
                Some(old) => {
                    fs::copy(old, &fresh)?;
                }
                None => (),
            }
            moves.push((fresh, target));
        }
        fs::create_dir_all(&state)?;
        for (fresh, target) in moves {
            fs::rename(fresh, target)?;
        }
        let temporary = dir.join("machine.toml.format2");
        fs::write(&temporary, toml::to_string_pretty(file)?)?;
        fs::rename(temporary, dir.join("machine.toml"))?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}
