//! Upgrade machines from earlier formats. Format 1 kept a copy of the boot
//! PROM and let the launcher build flash and NVRAM files under its own
//! names. Format 2 machines predate the module controller NVRAM store.
//! Formats 1 to 3 name their graphics with one word, which becomes the
//! pipes the catalogue describes. A current machine gains the flash store
//! each InfiniteReality pipe slot has had since QEMU gave it one.

use crate::{resolve, runtime, MachineFile, MachineInit, Machines, Offering, Result, StorageItem};
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
            return from_current(dir, machines, &text, prom, io_prom).map_err(|error| {
                format!(
                    "cannot upgrade {}: {error}; its machine.toml is unchanged",
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
    install(dir, machines, &file, offering, &init)?;
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

/// Whether an upgrade note only says how the machine's files are named, as
/// opposed to what its guest sees.
pub fn is_bookkeeping(note: &str) -> bool {
    note.starts_with("topology ") || note.starts_with("input ")
}

/// Bring a current machine up to date with the catalogue: a topology or
/// setting the catalogue renamed takes its new name, and each InfiniteReality
/// pipe gets the flash store it lacks. Returns None when the machine needs
/// neither.
fn from_current(
    dir: &Path,
    machines: &Machines,
    text: &str,
    prom: Option<&Path>,
    io_prom: Option<&Path>,
) -> Result<Option<Vec<String>>> {
    let mut table: toml::Table = toml::from_str(text)?;
    let renamed = renew(
        machines,
        table
            .get_mut("machine")
            .and_then(toml::Value::as_table_mut)
            .ok_or("no [machine] table")?,
    )?;
    let converted = rename_identity_serial(&mut table);
    let file: MachineFile = toml::Value::Table(table).try_into()?;
    let mut notes = vec![];
    if let Some(note) = &converted {
        notes.push(note.clone());
    }
    if !renamed.is_empty() {
        // The renamed machine must be one the catalogue offers, and keep its
        // storage: QEMU refuses a state directory that lacks an item.
        let offering = crate::offering(machines, &file.machine)?;
        let state = dir.join("state");
        if let Some(item) = offering.storage.iter().find(|item| {
            item.initial != PIPE_FLASH && !state.join(format!("{}.raw", item.name)).is_file()
        }) {
            return Err(format!(
                "{} lacks {}.raw that its new topology needs; recreate the machine",
                dir.display(),
                item.name
            )
            .into());
        }
        crate::graphics::validate(machines, &offering.topology, offering.nodes, &file.pipe)?;
        crate::settings::validate(machines, offering, &file.machine.inputs)?;
        notes.extend(renamed.iter().cloned());
    }
    notes.extend(add_pipe_flash(dir, machines, &file, prom, io_prom)?.unwrap_or_default());
    if renamed.is_empty() && converted.is_none() {
        return Ok((!notes.is_empty()).then_some(notes));
    }
    replace_file(dir, &file)?;
    Ok(Some(notes))
}

/// Store the number an unreleased build kept as `[identity] serial` under
/// its name in QEMU's interface, `emu-id`: `serial` there is the system
/// serial as text. Returns what changed.
fn rename_identity_serial(table: &mut toml::Table) -> Option<String> {
    let identity = table.get_mut("identity")?.as_table_mut()?;
    let number = identity.get("serial")?.as_integer()?;
    identity.remove("serial");
    identity.insert("emu-id".into(), number.into());
    Some(format!("[identity] serial {number} is now emu-id {number}"))
}

/// Give the machine in `table` the topology, processor and settings the
/// catalogue now names, and say what changed.
///
/// A version 2 machine list named a node board with `board=`; a version 3
/// topology is one board, so the stored board folds into the topology name.
fn renew(machines: &Machines, machine: &mut toml::Table) -> Result<Vec<String>> {
    let mut notes = vec![];
    let board = machine.remove("board");
    let board = board.as_ref().and_then(toml::Value::as_str);
    if let Some(topology) = machine
        .get("topology")
        .and_then(toml::Value::as_str)
        .map(str::to_string)
    {
        let population = stored_population(machine);
        let (now, processor) = current_topology(machines, &topology, board, &population)?;
        if let Some(processor) = processor {
            if machine.contains_key("processor") {
                return Err(format!(
                    "topology '{topology}' was a clock option of {now} and the machine names \
                     processor {}",
                    machine["processor"]
                )
                .into());
            }
            machine.insert("processor".into(), processor.into());
        }
        if now != topology {
            notes.push(format!(
                "topology {topology} is now {now}{}",
                processor.map_or(String::new(), |id| format!(" with processor {id}"))
            ));
            machine.insert("topology".into(), now.into());
        } else if board.is_some() {
            notes.push(format!("topology {topology} no longer names a board"));
        }
    }
    if let Some(inputs) = machine
        .get_mut("inputs")
        .and_then(toml::Value::as_table_mut)
    {
        for (old, value) in std::mem::take(inputs) {
            let new = current_input(&old)?;
            if new != old {
                notes.push(format!("input {old} is now {new}"));
            }
            inputs.insert(new, value);
        }
    }
    Ok(notes)
}

/// The CPUs per node a stored machine has.
fn stored_population(machine: &toml::Table) -> Vec<u32> {
    let numbers = |value: &toml::Value| -> Option<Vec<u32>> {
        value
            .as_array()?
            .iter()
            .map(|n| n.as_integer().and_then(|n| u32::try_from(n).ok()))
            .collect()
    };
    if let Some(population) = machine.get("population").and_then(numbers) {
        if !population.is_empty() {
            return population;
        }
    }
    let count = |key: &str| {
        machine
            .get(key)
            .and_then(toml::Value::as_integer)
            .and_then(|n| u32::try_from(n).ok())
    };
    match (count("nodes"), count("cpus_per_node")) {
        (Some(nodes), Some(cpus)) => vec![cpus; nodes as usize],
        _ => vec![],
    }
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

/// A topology the catalogue renamed. `board` is the node board a version 2
/// machine stored beside the old name: None matches a machine that stored
/// none or the topology's IP53 default, which is how 0.5.1 and every version 2
/// machine on the default board look. `processor` is the module a clock
/// topology became an option for.
struct Rename {
    old: &'static str,
    board: Option<&'static str>,
    new: &'static str,
    processor: Option<&'static str>,
}

const fn rename(old: &'static str, board: Option<&'static str>, new: &'static str) -> Rename {
    Rename {
        old,
        board,
        new,
        processor: None,
    }
}

const fn clock(old: &'static str, new: &'static str, processor: &'static str) -> Rename {
    Rename {
        old,
        board: None,
        new,
        processor: Some(processor),
    }
}

/// Topologies the catalogue renamed, each applied once the catalogue no
/// longer lists the old name. Version 3 gives each node board its own
/// topology, so a name that several boards fit is matched by the board the
/// machine stored, the IP53 one where it stored none.
const RENAMED: &[Rename] = &[
    rename("fuel-vpro", None, "fuel"),
    rename("tezro-rack-base", None, "tezro-rack-ip53"),
    rename(
        "tezro-deskside-700mhz-4mib-serial-trial",
        None,
        "tezro-ip53",
    ),
    rename("tezro-tower-multi", None, "tezro-ip53"),
    rename("tezro-tower-one", None, "tezro-one-processor"),
    rename("tezro-tower-multi-ip59", None, "tezro-ip59"),
    rename("tezro", Some("ip59"), "tezro-ip59"),
    rename("tezro", None, "tezro-ip53"),
    rename("tezro-rack", Some("ip59"), "tezro-rack-ip59"),
    rename("tezro-rack", None, "tezro-rack-ip53"),
    rename("onyx350-compat", None, "onyx350-ip53"),
    rename("onyx350", Some("ip59"), "onyx350-ip59"),
    rename("onyx350", None, "onyx350-ip53"),
    rename("origin350", Some("ip59"), "origin350-ip59"),
    rename("origin350", None, "origin350-ip53"),
    rename("origin3900", Some("ip59"), "origin3900-ip59"),
    rename("origin3900", None, "origin3900-ip53"),
    rename("onyx4-host-ip59", None, "onyx4"),
    rename("onyx4-host", Some("ip59"), "onyx4"),
    rename("onyx4-host", None, "onyx4"),
    clock("onyx3000-r14000-500mhz", "onyx3000", "r14000-500mhz-8mb"),
    clock("onyx3000-r14000-600mhz", "onyx3000", "r14000-600mhz-8mb"),
    clock("onyx3000-r16000", "onyx3000", "r16000-800mhz-8mb"),
    clock(
        "origin3200-r14000-500mhz",
        "origin3200",
        "r14000-500mhz-8mb",
    ),
    clock(
        "origin3200-r14000-600mhz",
        "origin3200",
        "r14000-600mhz-8mb",
    ),
    clock("origin3200-r16000", "origin3200", "r16000-800mhz-8mb"),
    clock(
        "origin3400-r14000-500mhz",
        "origin3400",
        "r14000-500mhz-8mb",
    ),
    clock(
        "origin3400-r14000-600mhz",
        "origin3400",
        "r14000-600mhz-8mb",
    ),
    clock("origin3400-r16000", "origin3400", "r16000-800mhz-8mb"),
    rename(
        "origin300-numalink-pci-expansion-experiment",
        None,
        "origin300-numalink-pci-expansion",
    ),
    rename(
        "origin300-pci-expansion-configuration-storage-experiment",
        None,
        "origin300-pci-expansion",
    ),
    rename(
        "origin300-pci-expansion-experiment",
        None,
        "origin300-pci-expansion",
    ),
    rename(
        "origin300-pci-expansion-pending-register-experiment",
        None,
        "origin300-pci-expansion",
    ),
    rename(
        "origin300-pci-expansion-unconnected-uart-experiment",
        None,
        "origin300-pci-expansion",
    ),
];

/// The topologies the rename table moves machines to, for a check against a
/// machine list.
pub fn renamed_to() -> impl Iterator<Item = &'static str> {
    RENAMED.iter().map(|rename| rename.new)
}

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
    (
        "onyx4-host-serial-compat",
        "the Onyx 4 host is now the onyx4 topology, which has only the IP59 node board, \
         and moving a machine built on IP53 to another node board would change the board \
         serial its PROM ties its NVRAM to; create a new machine",
    ),
    (
        "tezro-tower-one-ip59",
        "the one-processor IP59 Tezro tower was removed; the one-processor Tezro is \
         the IP53 board, tezro-one-processor",
    ),
];

/// The settings the catalogue renamed. A version 2 machine list named a CPU
/// property by its part, `r14000-prid`; version 3 has one `prid` for the
/// offering's part.
fn current_input(name: &str) -> Result<String> {
    let removed = [
        (
            "baseio-preset",
            "the base I/O board is a board product of the machine list",
        ),
        (
            "r12000-reset-mode",
            "the CPU reset mode is no longer a setting",
        ),
        (
            "r16000-reset-mode",
            "the CPU reset mode is no longer a setting",
        ),
        (
            "r12000-scache-block-words",
            "the secondary cache comes from the processor module",
        ),
        (
            "r12000-scache-bytes",
            "the secondary cache comes from the processor module",
        ),
        (
            "r16000-scache-bytes",
            "the secondary cache comes from the processor module",
        ),
        (
            "r16000-experimental",
            "the experimental R16000 mode was removed",
        ),
    ];
    if let Some((_, reason)) = removed.iter().find(|(old, _)| *old == name) {
        return Err(format!(
            "the input {name} was removed from the machine list: {reason}; delete it from \
             [machine.inputs]"
        )
        .into());
    }
    // `r14000-prid` is `prid` and `r12000-fpu-id` is `fpu-id`.
    let unpart = name.split_once('-').and_then(|(part, rest)| {
        let digits = part.strip_prefix('r')?;
        let numbered = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
        (numbered && matches!(rest, "prid" | "fpu-id")).then_some(rest)
    });
    Ok(unpart.unwrap_or(name).to_string())
}

/// What a topology the machine list no longer has became, for a message.
pub(crate) fn unlisted_topology(name: &str) -> String {
    let now: Vec<_> = RENAMED
        .iter()
        .filter(|rename| rename.old == name)
        .map(|rename| format!("'{}'", rename.new))
        .collect();
    if !now.is_empty() {
        return format!(
            "topology '{name}' is {} in this QEMU; upgrade the machine once with origami upgrade",
            if now.len() == 1 {
                format!("now {}", now[0])
            } else {
                format!("now one of {}", now.join(", "))
            }
        );
    }
    match REMOVED.iter().find(|(old, _)| *old == name) {
        Some((_, reason)) => format!("topology '{name}' was removed: {reason}"),
        None => format!("this QEMU has no topology '{name}'"),
    }
}

/// The topology, and the processor module for a clock topology, the
/// catalogue now names for a machine's stored `name`, the `board` a version 2
/// machine stored and its `population`. A topology it still lists is
/// unchanged; a removed one is refused with the reason.
pub(crate) fn current_topology<'a>(
    machines: &Machines,
    name: &'a str,
    board: Option<&str>,
    population: &[u32],
) -> Result<(&'a str, Option<&'static str>)> {
    if let Some((_, reason)) = REMOVED.iter().find(|(old, _)| *old == name) {
        return Err(format!("topology '{name}' was removed: {reason}").into());
    }
    let listed = |name: &str| machines.offerings.iter().any(|o| o.topology == name);
    if listed(name) {
        return Ok((name, None));
    }
    let Some(found) = RENAMED
        .iter()
        .filter(|rename| rename.old == name && listed(rename.new))
        .find(|rename| match (rename.board, board) {
            (Some(want), Some(have)) => want == have,
            (Some(_), None) => false,
            // The IP53 default: no board stored, or the IP53 board.
            (None, have) => have.is_none_or(|have| have == "ip53"),
        })
    else {
        return Ok((name, None));
    };
    // A one-processor Tezro is the IP53 board with a single CPU; the topology
    // that fits two or four is tezro-ip53.
    if found.new == "tezro-ip53" && population == [1] {
        return Ok(("tezro-one-processor", None));
    }
    Ok((found.new, found.processor))
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
    let mut notes = renew(machines, machine)?;
    let topology = machine
        .get("topology")
        .and_then(toml::Value::as_str)
        .ok_or("no machine topology")?
        .to_string();
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
    notes.extend(note.map(String::from));
    Ok(notes)
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
    machines: &Machines,
    file: &MachineFile,
    offering: &Offering,
    init: &MachineInit<'_>,
) -> Result<()> {
    crate::settings::validate(machines, offering, &file.machine.inputs)?;
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
