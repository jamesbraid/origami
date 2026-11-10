//! The machine inputs `--set NAME[.SLOT]=VALUE` gives. The machine list says,
//! for each offering, which settings it takes, what kind of value each is,
//! which slots a list setting has, where QEMU applies it and what it defaults
//! to. Nothing here knows a machine: a setting is checked and written to QEMU
//! from the list's own record of it.
//!
//! QEMU judges the rest. A text setting's pattern is not matched here, and
//! the properties the identity interface owns (`serial`, `hostid`) pass
//! through with only their shape checked.

use crate::{Machines, Offering, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// What a setting is, written once for the whole machine list.
#[derive(Clone, Debug, Deserialize)]
pub struct Definition {
    pub name: String,
    pub kind: Kind,
    pub target: Target,
    #[serde(rename = "min-length", default)]
    pub min_length: Option<usize>,
    #[serde(rename = "max-length", default)]
    pub max_length: Option<usize>,
    #[serde(default)]
    pub digits: Option<usize>,
    #[serde(default)]
    pub minimum: Option<u64>,
    #[serde(default)]
    pub maximum: Option<u64>,
    #[serde(default)]
    pub values: Vec<String>,
    /// A file's exact size in bytes; absent for any size.
    #[serde(default)]
    pub size: Option<u64>,
    /// Whether an empty value passes through, meaning "no record".
    #[serde(rename = "allow-empty", default)]
    pub allow_empty: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Hex,
    Integer,
    Boolean,
    Enum,
    File,
}

/// Where and how QEMU is given a setting.
#[derive(Clone, Debug, Deserialize)]
pub struct Target {
    pub kind: TargetKind,
    /// Absent for a `serial-lines` target, whose lines each carry their own.
    #[serde(default)]
    pub property: Option<String>,
    /// The QOM type, for a `global` target.
    #[serde(default)]
    pub device: Option<String>,
    /// Absent for a `serial-lines` target.
    #[serde(default)]
    pub form: Option<Form>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TargetKind {
    /// A `-M` property.
    Machine,
    /// A `-global DEVICE.PROPERTY=` setting.
    Global,
    /// A `-cpu` property of the offering's processor part.
    Cpu,
    /// A boolean that backs the offering's serial lines naming it. This
    /// Origami launches no peer for such a line, so it refuses the setting.
    SerialLines,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Form {
    /// `PROPERTY=VALUE`.
    Plain,
    /// `PROPERTY.KEY=VALUE`, with every position below the highest key given
    /// filled with an empty entry, which QEMU takes as "no record".
    List,
    /// `PROPERTY.I=KEY=VALUE`, I counting from 0 over the keys given, in the
    /// order of the setting's slot keys.
    KeyedList,
}

/// One setting an offering takes.
#[derive(Clone, Debug, Deserialize)]
pub struct OfferingSetting {
    pub name: String,
    #[serde(default)]
    pub slots: Option<Slots>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(rename = "default-provenance", default)]
    pub default_provenance: Option<String>,
    /// Names accepted in place of a number; a list joined by commas is or-ed.
    #[serde(default)]
    pub choices: Vec<Choice>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Slots {
    pub keys: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Choice {
    pub name: String,
    pub value: u64,
}

/// Settings that go together.
#[derive(Clone, Debug, Deserialize)]
pub struct Group {
    pub name: String,
    pub members: Vec<String>,
    pub rule: Rule,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    /// Every member must be given.
    All,
    /// None, or every member.
    AllOrNone,
}

/// What a launch gives QEMU for a machine's inputs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rendered {
    /// `,NAME=VALUE` pieces for the `-M` value.
    pub machine: String,
    /// `,NAME=VALUE` pieces for the `-cpu` value.
    pub cpu: String,
    /// `DEVICE.PROPERTY=VALUE` for each `-global`.
    pub globals: Vec<String>,
}

/// The identity interface's own properties, which every offering accepts
/// without listing them. The others, `emu-id` and `mac`, have options.
const IDENTITY: &[&str] = &["serial", "hostid"];

/// A key split into its setting name and slot.
fn split(key: &str) -> (&str, Option<&str>) {
    match key.split_once('.') {
        Some((name, slot)) => (name, Some(slot)),
        None => (key, None),
    }
}

impl Offering {
    /// The setting named `name`, if the offering takes it.
    pub fn setting(&self, name: &str) -> Option<&OfferingSetting> {
        self.settings.iter().find(|setting| setting.name == name)
    }
}

fn definition<'a>(machines: &'a Machines, name: &str) -> Result<&'a Definition> {
    machines
        .setting_definitions
        .iter()
        .find(|definition| definition.name == name)
        .ok_or_else(|| format!("the machine list does not define the setting {name}").into())
}

/// The property and form a setting is written with, or why it cannot be: a
/// `serial-lines` setting backs lines to a companion process this Origami
/// does not launch, so taking it would silently do nothing.
fn property_and_form<'a>(name: &str, target: &'a Target) -> Result<(&'a str, Form)> {
    match (target.kind, target.property.as_deref(), target.form) {
        (TargetKind::SerialLines, ..) => Err(format!(
            "{name} backs serial lines for a companion process, which this Origami does not launch"
        )
        .into()),
        (_, Some(property), Some(form)) => Ok((property, form)),
        _ => Err(format!("the machine list gives the setting {name} no property or form").into()),
    }
}

/// The names `--set` accepts on `offering`, sorted.
pub fn names(offering: &Offering) -> Vec<String> {
    let mut names: Vec<String> = offering
        .settings
        .iter()
        .map(|setting| setting.name.clone())
        .chain(IDENTITY.iter().map(|name| name.to_string()))
        .collect();
    names.sort();
    names
}

/// A value that stays one option value: QEMU reads a comma as the next
/// option and the first `=` of a keyed entry as its key.
fn plain(key: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c == ',' || c == '=' || c.is_whitespace())
    {
        return Err(format!("invalid {key} value: {value:?}").into());
    }
    Ok(())
}

fn number(key: &str, text: &str) -> Result<u64> {
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|_| {
        format!("{key} must be a decimal or 0x hexadecimal number, not {text:?}").into()
    })
}

/// The number an integer setting's `value` stands for: a number, or the
/// choices named, or-ed.
fn integer(key: &str, setting: &OfferingSetting, value: &str) -> Result<u64> {
    if setting.choices.is_empty() {
        return number(key, value);
    }
    let mut total = 0;
    for part in value.split(',') {
        total |= match setting.choices.iter().find(|choice| choice.name == part) {
            Some(choice) => choice.value,
            None if part.chars().next().is_some_and(|c| c.is_ascii_digit()) => number(key, part)?,
            None => {
                let names: Vec<_> = setting.choices.iter().map(|c| c.name.as_str()).collect();
                return Err(format!(
                    "{key} is a number or names joined by commas from {}, not {part:?}",
                    names.join(", ")
                )
                .into());
            }
        };
    }
    Ok(total)
}

fn check_value(
    key: &str,
    definition: &Definition,
    setting: &OfferingSetting,
    value: &str,
) -> Result<()> {
    if value.is_empty() {
        return if definition.allow_empty {
            Ok(())
        } else {
            Err(format!("{key} needs a value").into())
        };
    }
    match definition.kind {
        Kind::Text => {
            plain(key, value)?;
            if definition
                .min_length
                .is_some_and(|least| value.len() < least)
                || definition.max_length.is_some_and(|most| value.len() > most)
            {
                return Err(format!(
                    "{key} must be {} to {} characters",
                    definition.min_length.unwrap_or(0),
                    definition
                        .max_length
                        .map_or("any number of".into(), |m| m.to_string())
                )
                .into());
            }
        }
        Kind::Hex => {
            let digits = definition.digits;
            if digits.is_some_and(|digits| value.len() != digits)
                || !value.chars().all(|c| c.is_ascii_hexdigit())
            {
                return Err(format!(
                    "{key} must be {} hexadecimal digits",
                    digits.map_or("a number of".into(), |d| d.to_string())
                )
                .into());
            }
        }
        Kind::Integer => {
            let number = integer(key, setting, value)?;
            if definition.minimum.is_some_and(|least| number < least)
                || definition.maximum.is_some_and(|most| number > most)
            {
                return Err(format!(
                    "{key} must be {} to {}",
                    definition.minimum.unwrap_or(0),
                    definition.maximum.unwrap_or(u64::MAX)
                )
                .into());
            }
        }
        Kind::Boolean => {
            if !["on", "off", "true", "false", "yes", "no"].contains(&value) {
                return Err(format!("{key} must be on or off, not {value:?}").into());
            }
        }
        Kind::Enum => {
            if !definition.values.iter().any(|allowed| allowed == value) {
                return Err(format!(
                    "{key} must be one of {}, not {value:?}",
                    definition.values.join(", ")
                )
                .into());
            }
        }
        Kind::File => {
            if value.contains('=') {
                return Err(format!("invalid {key} file name: {value:?}").into());
            }
            let metadata = std::fs::metadata(value)
                .map_err(|error| format!("{key}: cannot read {value}: {error}"))?;
            if !metadata.is_file() {
                return Err(format!("{key}: {value} is not a file").into());
            }
            if definition.size.is_some_and(|size| metadata.len() != size) {
                return Err(format!(
                    "{key}: {value} must be {} bytes, not {}",
                    definition.size.unwrap_or_default(),
                    metadata.len()
                )
                .into());
            }
        }
    }
    Ok(())
}

/// Refuse inputs `offering` does not take, a value of the wrong kind, a slot
/// the setting lacks, and a required setting or group left incomplete.
pub fn validate(
    machines: &Machines,
    offering: &Offering,
    inputs: &BTreeMap<String, String>,
) -> Result<()> {
    for (key, value) in inputs {
        let (name, slot) = split(key);
        if name == "mac" {
            return Err("set the machine MAC with --mac".into());
        }
        if IDENTITY.contains(&name) {
            if slot.is_some() {
                return Err(format!("{name} takes no slot").into());
            }
            plain(key, value)?;
            continue;
        }
        let Some(setting) = offering.setting(name) else {
            return Err(format!(
                "{} does not take {name}; it takes {}",
                offering.topology,
                names(offering).join(", ")
            )
            .into());
        };
        match (&setting.slots, slot) {
            (Some(slots), Some(slot)) if slots.keys.iter().any(|k| k == slot) => (),
            (Some(slots), Some(slot)) => {
                return Err(format!(
                    "{name} has no slot {slot} on {}; it has {}",
                    offering.topology,
                    slots.keys.join(", ")
                )
                .into())
            }
            (Some(slots), None) => {
                return Err(format!(
                    "{name} needs a slot, such as --set {name}.{}=VALUE; it has {}",
                    slots.keys.first().map_or("", String::as_str),
                    slots.keys.join(", ")
                )
                .into())
            }
            (None, Some(_)) => return Err(format!("{name} takes no slot").into()),
            (None, None) => (),
        }
        let definition = definition(machines, name)?;
        property_and_form(name, &definition.target)?;
        check_value(key, definition, setting, value)?;
    }
    let given = |name: &str| inputs.keys().any(|key| split(key).0 == name);
    for setting in &offering.settings {
        if setting.required && setting.default.is_none() && !given(&setting.name) {
            return Err(format!(
                "{} requires --set {}=VALUE",
                offering.topology, setting.name
            )
            .into());
        }
    }
    for group in &offering.setting_groups {
        let count = group.members.iter().filter(|name| given(name)).count();
        let complete = count == group.members.len();
        let bad = match group.rule {
            Rule::All => !complete,
            Rule::AllOrNone => count != 0 && !complete,
        };
        if bad {
            return Err(format!(
                "{} takes {} together{}",
                offering.topology,
                group.members.join(", "),
                match group.rule {
                    Rule::All => ": give every one",
                    Rule::AllOrNone => " or not at all",
                }
            )
            .into());
        }
    }
    Ok(())
}

/// `inputs` with each file named by an absolute path, so a machine does not
/// depend on the directory it was created from.
pub fn resolve(
    machines: &Machines,
    offering: &Offering,
    inputs: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    validate(machines, offering, &inputs)?;
    inputs
        .into_iter()
        .map(|(key, value)| {
            let is_file = definition(machines, split(&key).0)
                .is_ok_and(|definition| definition.kind == Kind::File);
            if !is_file || value.is_empty() {
                return Ok((key, value));
            }
            let path = Path::new(&value).canonicalize()?;
            Ok((key, path.display().to_string()))
        })
        .collect()
}

/// QEMU's spelling of the inputs: `-M` and `-cpu` pieces and `-global`
/// values. `inputs` must have passed [`validate`].
pub fn render(
    machines: &Machines,
    offering: &Offering,
    inputs: &BTreeMap<String, String>,
) -> Result<Rendered> {
    let mut out = Rendered::default();
    let names: std::collections::BTreeSet<&str> = inputs.keys().map(|key| split(key).0).collect();
    for name in names {
        let entries: Vec<(&str, &str)> = inputs
            .iter()
            .filter(|(key, _)| split(key).0 == name)
            .map(|(key, value)| (split(key).1.unwrap_or(""), value.as_str()))
            .collect();
        if IDENTITY.contains(&name) {
            out.machine.push_str(&format!(",{name}={}", entries[0].1));
            continue;
        }
        let setting = offering
            .setting(name)
            .ok_or_else(|| format!("{} does not take {name}", offering.topology))?;
        let definition = definition(machines, name)?;
        let target = &definition.target;
        let (base, form) = property_and_form(name, target)?;
        let property = match target.kind {
            TargetKind::Cpu => format!("{}-{base}", offering.processor.part.to_ascii_lowercase()),
            _ => base.to_string(),
        };
        let text = |value: &str| -> Result<String> {
            Ok(match definition.kind {
                Kind::Integer if !setting.choices.is_empty() && !value.is_empty() => {
                    integer(name, setting, value)?.to_string()
                }
                Kind::File => crate::qemu_path_option(Path::new(value)),
                _ => value.to_string(),
            })
        };
        let mut pairs: Vec<(String, String)> = vec![];
        match form {
            Form::Plain => pairs.push((property.clone(), text(entries[0].1)?)),
            Form::List => {
                let numbered: Option<Vec<(usize, &str)>> = entries
                    .iter()
                    .map(|(slot, value)| slot.parse().ok().map(|index| (index, *value)))
                    .collect();
                match numbered {
                    Some(numbered) => {
                        let last = numbered.iter().map(|(index, _)| *index).max().unwrap_or(0);
                        for index in 0..=last {
                            let value = numbered
                                .iter()
                                .find(|(at, _)| *at == index)
                                .map_or(Ok(String::new()), |(_, value)| text(value))?;
                            pairs.push((format!("{property}.{index}"), value));
                        }
                    }
                    None => {
                        for (slot, value) in &entries {
                            pairs.push((format!("{property}.{slot}"), text(value)?));
                        }
                    }
                }
            }
            Form::KeyedList => {
                let keys = setting.slots.as_ref().map_or(&[][..], |s| &s.keys[..]);
                let mut ordered = entries.clone();
                ordered.sort_by_key(|(slot, _)| keys.iter().position(|key| key == slot));
                for (index, (slot, value)) in ordered.iter().enumerate() {
                    pairs.push((
                        format!("{property}.{index}"),
                        format!("{slot}={}", text(value)?),
                    ));
                }
            }
        }
        for (property, value) in pairs {
            match target.kind {
                TargetKind::Machine => out.machine.push_str(&format!(",{property}={value}")),
                TargetKind::Cpu => out.cpu.push_str(&format!(",{property}={value}")),
                TargetKind::Global => out.globals.push(format!(
                    "{}.{property}={value}",
                    target.device.as_deref().unwrap_or_default()
                )),
                TargetKind::SerialLines => unreachable!("property_and_form refused it"),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{preset, test_machines};

    fn inputs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn file(dir: &Path, name: &str, size: usize) -> String {
        let path = dir.join(name);
        std::fs::write(&path, vec![0u8; size]).unwrap();
        path.display().to_string()
    }

    #[test]
    fn a_plain_machine_setting_is_checked_against_its_kind() {
        let machines = test_machines();
        let fuel = preset(&machines, "fuel-1").unwrap();
        for (set, wrong) in [
            (("board-id-word", "0x4000"), ("board-id-word", "high")),
            (("l1-revision", "1.2.3"), ("l1-revision", "1.2.3,x=1")),
            (("l1-type-code", "3"), ("l1-type-code", "256")),
        ] {
            validate(&machines, fuel, &inputs(&[set])).unwrap();
            assert!(
                validate(&machines, fuel, &inputs(&[wrong])).is_err(),
                "{wrong:?}"
            );
        }
        let rendered = render(&machines, fuel, &inputs(&[("board-id-word", "0x4000")])).unwrap();
        assert_eq!(rendered.machine, ",board-id-word=0x4000");
        assert!(rendered.cpu.is_empty() && rendered.globals.is_empty());
    }

    #[test]
    fn a_serial_lines_setting_is_refused_rather_than_ignored() {
        let machines = test_machines();
        let offering = machines
            .offerings
            .iter()
            .find(|o| o.setting("l2-controller").is_some())
            .expect("the fixture has an offering with an L2 link");
        let set = inputs(&[("l2-controller", "1")]);
        for error in [
            validate(&machines, offering, &set).unwrap_err(),
            render(&machines, offering, &set).unwrap_err(),
        ] {
            assert!(error.to_string().contains("companion process"), "{error}");
        }
    }

    #[test]
    fn a_cpu_setting_sets_the_property_of_the_offerings_part() {
        let machines = test_machines();
        for (name, part) in [("origin200-2", "r10000"), ("onyx350", "r16000")] {
            let offering = preset(&machines, name).unwrap();
            let rendered = render(&machines, offering, &inputs(&[("prid", "0xe24")])).unwrap();
            assert_eq!(rendered.cpu, format!(",{part}-prid=0xe24"), "{name}");
            assert!(rendered.machine.is_empty(), "{name}");
        }
        // An offering that lists no prid refuses it.
        let o2 = machines
            .offerings
            .iter()
            .find(|o| o.product == "o2")
            .unwrap();
        let error = validate(&machines, o2, &inputs(&[("prid", "1")])).unwrap_err();
        assert!(error.to_string().contains("does not take prid"), "{error}");
    }

    #[test]
    fn a_list_setting_takes_a_slot_and_an_empty_entry_fills_the_gap() {
        let machines = test_machines();
        let dir = tempfile::tempdir().unwrap();
        let record = file(dir.path(), "chassis.bin", 512);
        let origin300 = preset(&machines, "origin300-2").unwrap();
        let slotted = inputs(&[("chassis-eeprom.1", &record)]);
        validate(&machines, origin300, &slotted).unwrap();
        let rendered = render(&machines, origin300, &slotted).unwrap();
        assert_eq!(
            rendered.machine,
            format!(",chassis-eeprom.0=,chassis-eeprom.1={record}")
        );
        // No slot, or a slot the setting lacks, is refused with the slots.
        for wrong in ["chassis-eeprom", "chassis-eeprom.9", "chassis-eeprom.x"] {
            let error = validate(&machines, origin300, &inputs(&[(wrong, &record)]))
                .unwrap_err()
                .to_string();
            assert!(error.contains("0, 1"), "{wrong}: {error}");
        }
        // An empty value is a record only where the setting allows none.
        assert!(validate(&machines, origin300, &inputs(&[("chassis-eeprom.0", "")])).is_err());
        let spd = inputs(&[("spd-eeprom.0", ""), ("spd-eeprom.1", &record)]);
        validate(&machines, origin300, &spd).unwrap();
        assert_eq!(
            render(&machines, origin300, &spd).unwrap().machine,
            format!(",spd-eeprom.0=,spd-eeprom.1={record}")
        );
    }

    #[test]
    fn a_keyed_list_numbers_the_keys_given_in_slot_order() {
        let machines = test_machines();
        let dir = tempfile::tempdir().unwrap();
        let record = file(dir.path(), "board.bin", 512);
        let expansion = machines
            .offerings
            .iter()
            .find(|o| o.id == "origin300-pci-expansion.2")
            .unwrap();
        let given = inputs(&[
            ("controller-board-eeprom.1/2", &record),
            ("controller-board-eeprom.1/0", &record),
            ("controller-brick-serial.1", "EMU123"),
        ]);
        validate(&machines, expansion, &given).unwrap();
        assert_eq!(
            render(&machines, expansion, &given).unwrap().machine,
            format!(
                ",controller-board-eeprom.0=1/0={record},controller-board-eeprom.1=1/2={record},{}",
                "controller-brick-serial.0=1=EMU123"
            )
        );
    }

    #[test]
    fn a_global_setting_names_its_device() {
        let machines = test_machines();
        let origin300 = preset(&machines, "origin300-2").unwrap();
        let rendered = render(
            &machines,
            origin300,
            &inputs(&[("brick-serial", "EMU1"), ("debug-switches", "5")]),
        )
        .unwrap();
        assert_eq!(
            rendered.globals,
            ["sgi-l1.brick-serial=EMU1", "sgi-l1.debug-switches=5"]
        );
        assert!(rendered.machine.is_empty());
    }

    #[test]
    fn named_choices_are_or_ed_into_the_number() {
        let machines = test_machines();
        let origin200 = preset(&machines, "origin200-2").unwrap();
        let rendered = render(
            &machines,
            origin200,
            &inputs(&[("dip-switches", "pod,verbose")]),
        )
        .unwrap();
        assert_eq!(rendered.machine, ",elsc-dip-switches=20");
        let rendered = render(&machines, origin200, &inputs(&[("dip-switches", "3")])).unwrap();
        assert_eq!(rendered.machine, ",elsc-dip-switches=3");
        assert!(validate(&machines, origin200, &inputs(&[("dip-switches", "bogus")])).is_err());
        // The same name means another number on a machine with more nodes.
        let dual = preset(&machines, "origin200-dual").unwrap();
        let rendered = render(&machines, dual, &inputs(&[("dip-switches", "pod")])).unwrap();
        assert_eq!(rendered.machine, ",elsc-dip-switches=8");
    }

    #[test]
    fn a_group_and_a_required_setting_must_be_complete() {
        let machines = test_machines();
        let dir = tempfile::tempdir().unwrap();
        let record = file(dir.path(), "record.bin", 512);
        let fuel = preset(&machines, "fuel-1").unwrap();
        let error = validate(&machines, fuel, &inputs(&[("chassis-eeprom.0", &record)]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("chassis-eeprom, board-eeprom together or not at all"),
            "{error}"
        );
        validate(
            &machines,
            fuel,
            &inputs(&[("chassis-eeprom.0", &record), ("board-eeprom.0", &record)]),
        )
        .unwrap();
        let o2 = machines.offerings.iter().find(|o| o.id == "o2.1").unwrap();
        let error = validate(&machines, o2, &BTreeMap::new())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("requires --set password-clear-input=VALUE"),
            "{error}"
        );
    }

    #[test]
    fn a_file_must_exist_and_have_its_size_and_is_stored_absolute() {
        let machines = test_machines();
        let dir = tempfile::tempdir().unwrap();
        let fuel = preset(&machines, "fuel-1").unwrap();
        let good = file(dir.path(), "mac.bin", 512);
        let short = file(dir.path(), "short.bin", 16);
        validate(&machines, fuel, &inputs(&[("fuel-mac-eeprom", &good)])).unwrap();
        for wrong in [short.as_str(), "/nonexistent/mac.bin"] {
            assert!(validate(&machines, fuel, &inputs(&[("fuel-mac-eeprom", wrong)])).is_err());
        }
        let resolved = resolve(&machines, fuel, inputs(&[("fuel-mac-eeprom", &good)])).unwrap();
        assert!(Path::new(&resolved["fuel-mac-eeprom"]).is_absolute());
        // A comma in a path is doubled for QEMU.
        let odd = dir.path().join("a,b");
        std::fs::create_dir(&odd).unwrap();
        let comma = file(&odd, "mac.bin", 512);
        let rendered = render(&machines, fuel, &inputs(&[("fuel-mac-eeprom", &comma)])).unwrap();
        assert_eq!(
            rendered.machine,
            format!(",fuel-mac-eeprom={}", comma.replace(',', ",,"))
        );
    }

    #[test]
    fn identity_properties_pass_through_and_the_mac_has_its_own_option() {
        let machines = test_machines();
        let fuel = preset(&machines, "fuel-1").unwrap();
        let given = inputs(&[("serial", "M2100887"), ("hostid", "69100887")]);
        validate(&machines, fuel, &given).unwrap();
        assert_eq!(
            render(&machines, fuel, &given).unwrap().machine,
            ",hostid=69100887,serial=M2100887"
        );
        let error = validate(&machines, fuel, &inputs(&[("mac", "08:00:69:12:34:56")]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("--mac"), "{error}");
        let error = validate(&machines, fuel, &inputs(&[("no-such", "1")]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("hostid, "), "{error}");
        assert!(error.contains("board-id-word"), "{error}");
    }
}
