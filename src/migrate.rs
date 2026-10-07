//! Upgrade machines from format 1, which kept a copy of the boot PROM and
//! let the launcher build flash and NVRAM files under its own names.

use crate::{profiles, resolve, runtime, MachineFile, MachineInit, Machines, Offering, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// The `format` a machine.toml's `text` declares.
pub fn format(text: &str) -> Option<i64> {
    toml::from_str::<toml::Table>(text)
        .ok()?
        .get("format")?
        .as_integer()
}

/// Rewrite the stopped format 1 machine in `dir` as the current format,
/// reading an IO PROM from `io_prom` or a verified download. QEMU's init
/// tool creates its storage, then the flash and NVRAM contents the machine
/// already has replace the fresh ones. The old files stay where they are;
/// on failure, so does machine.toml.
pub fn upgrade(dir: &Path, machines: &Machines, io_prom: Option<&Path>) -> Result<()> {
    // A running machine, or another edit, holds this lock.
    let _lock = crate::control::lock_for_edit(dir, "upgrading it")?;
    let path = dir.join("machine.toml");
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if format(&text) != Some(1) {
        return Err(format!(
            "{} is not from Origami 0.1; it needs no upgrade",
            path.display()
        )
        .into());
    }
    let result = (|| -> Result<()> {
        let (file, prom) = convert(dir, &text)?;
        let offering = crate::offering(machines, &file.machine)?;
        // Format 1 kept no IO PROM.
        let (prom, io_prom) = crate::firmware(offering, Some(&prom), io_prom)?;
        let init = MachineInit {
            tool: &runtime::machine_init_path()?,
            boot_prom: &prom,
            io_prom: io_prom.as_deref(),
        };
        install(dir, &file, offering, &init)
    })();
    result.map_err(|error| {
        format!(
            "cannot upgrade the format 1 machine {}: {error}; its machine.toml is unchanged",
            dir.display()
        )
        .into()
    })
}

/// The format 2 machine file for format 1 `text`, and its boot PROM.
fn convert(dir: &Path, text: &str) -> Result<(MachineFile, PathBuf)> {
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
    table.insert("format".into(), i64::from(crate::MACHINE_FORMAT).into());
    Ok((toml::Value::Table(table).try_into()?, prom))
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
