//! Upgrade machines from format 1, which kept a copy of the boot PROM and
//! let the launcher build flash and NVRAM files under its own names.

use crate::{profiles, resolve, runtime, MachineFile, MachineInit, Offering, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Rewrite the format 1 machine in `dir`, whose machine.toml is `text`, as
/// the current format. QEMU's init tool creates its storage, then the flash
/// and NVRAM contents the machine already has replace the fresh ones. The
/// old files stay where they are; on failure, so does machine.toml.
pub fn upgrade(dir: &Path, text: &str) -> Result<()> {
    let result = (|| -> Result<()> {
        let catalog = crate::catalogue()?;
        let (file, prom) = convert(dir, text)?;
        let offering = crate::offering(&catalog, &file.machine)?;
        // Format 1 had no IO PROM, so take the one the matching preset
        // downloads.
        let io_prom = match profiles::STARTERS
            .iter()
            .find(|p| p.topology == offering.topology && p.population == offering.cpus_per_node)
            .and_then(|p| p.io_prom)
        {
            Some(id) => Some(crate::assets::acquire(id)?),
            None => None,
        };
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
    let staging = dir.join("state.format2");
    // Left by an interrupted upgrade.
    let _ = fs::remove_dir_all(&staging);
    let result = (|| -> Result<()> {
        let arguments = runtime::init_arguments(offering, init.boot_prom, init.io_prom, &staging)?;
        runtime::create_state(init.tool, &arguments)?;
        let raw = |root: &Path, name: &str| root.join(format!("{name}.raw"));
        for item in &offering.storage {
            // A file of another size is not this item's contents.
            if let Some(old) = old_state(dir, &item.name)
                .into_iter()
                .find(|old| fs::metadata(old).is_ok_and(|m| m.len() == item.size))
            {
                fs::copy(old, raw(&staging, &item.name))?;
            }
        }
        let state = dir.join("state");
        fs::create_dir_all(&state)?;
        for item in &offering.storage {
            fs::rename(raw(&staging, &item.name), raw(&state, &item.name))?;
        }
        let temporary = dir.join("machine.toml.format2");
        fs::write(&temporary, toml::to_string_pretty(file)?)?;
        fs::rename(temporary, dir.join("machine.toml"))?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn origin2000_keeps_its_node_flash_nvram_drives_and_mac() {
        let root = std::env::temp_dir().join(format!(
            "origami-upgrade-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dir = root.join("machine");
        fs::create_dir_all(dir.join("firmware")).unwrap();
        fs::create_dir_all(dir.join("state/node-proms")).unwrap();
        fs::create_dir_all(dir.join("drives")).unwrap();
        fs::write(dir.join("drives/system.qcow2"), b"disk").unwrap();
        let text = r#"format = 1

[machine]
topology = "origin2000-rack"
population = [2, 2, 2, 2]
model = "origin2000"
nodes = 4
cpus_per_node = 2
memory_per_node = "64MiB"
graphics = "none"

[firmware]
image = "firmware/prom.bin"

[network]
mode = "private"
endpoint = "tcp:127.0.0.1:4242"
mac = "08:00:69:12:34:56"

[[drive]]
name = "system"
type = "disk"
bus = 0
target = 1
image = "drives/system.qcow2"
"#;
        fs::write(dir.join("machine.toml"), text).unwrap();
        fs::write(dir.join("firmware/prom.bin"), b"prom").unwrap();
        let mut old = vec![dir.join("firmware/prom.bin")];
        for node in 1..=4u8 {
            let path = dir.join(format!("state/node-proms/node{node}.bin"));
            fs::write(&path, vec![node; 1048576]).unwrap();
            old.push(path);
        }
        fs::write(dir.join("state/nvram0.raw"), vec![0x5a; 32768]).unwrap();
        // A clock of the wrong size is left for the fresh one.
        fs::write(dir.join("state/nvram0.raw.clock"), [7; 8]).unwrap();
        old.push(dir.join("state/nvram0.raw.clock"));
        let before: Vec<_> = old.iter().map(|path| fs::read(path).unwrap()).collect();

        let catalog = crate::test_catalogue();
        let (file, prom) = convert(&dir, text).unwrap();
        let offering = crate::offering(&catalog, &file.machine).unwrap();
        let tool = crate::test_support::init_tool(&root);
        let io_prom = root.join("io6prom.img");
        let init = MachineInit {
            tool: &tool,
            boot_prom: &prom,
            io_prom: Some(&io_prom),
        };
        let missing = root.join("missing-tool");
        let broken = MachineInit {
            tool: &missing,
            ..init
        };
        assert!(install(&dir, &file, offering, &broken).is_err());
        assert_eq!(fs::read_to_string(dir.join("machine.toml")).unwrap(), text);
        assert!(!dir.join("state/node0-flash.raw").exists());

        install(&dir, &file, offering, &init).unwrap();
        let state = |name: &str| fs::read(dir.join("state").join(name)).unwrap();
        for node in 0..4u8 {
            assert_eq!(state(&format!("node{node}-flash.raw")), [node + 1; 1048576]);
        }
        assert_eq!(state("io0-flash.raw"), [0; 1048576]);
        assert_eq!(state("nvram0.raw"), [0x5a; 32768]);
        assert_eq!(state("nvram0-clock.raw"), [0; 16]);
        assert!(!dir.join("state.format2").exists());
        for (path, bytes) in old.iter().zip(&before) {
            assert_eq!(&fs::read(path).unwrap(), bytes, "{}", path.display());
        }

        let upgraded = crate::read_machine(&dir).unwrap();
        assert_eq!(upgraded.format, crate::MACHINE_FORMAT);
        assert_eq!(upgraded.identity.as_ref().unwrap().mac, "08:00:69:12:34:56");
        assert_eq!(upgraded.network.mode, "private");
        assert_eq!(upgraded.drive[0].image, "drives/system.qcow2");
        crate::validate(&catalog, &dir, &upgraded).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
