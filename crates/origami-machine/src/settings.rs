//! The machine inputs `--set NAME[.SLOT]=VALUE` gives. The machine list says,
//! for each offering, which settings it takes, what kind of value each is,
//! which slots a list setting has and what it defaults to. Nothing here knows
//! a machine: a setting is checked from the list's own record of it. How the
//! engine is given a setting is in the definition's engine data.
//!
//! The engine judges the rest. A text setting's pattern is not matched here,
//! and the properties the identity interface owns (`serial`, `hostid`) pass
//! through with only their shape checked.

use crate::{Machines, Offering, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

/// What a setting is, written once for the whole machine list.
#[derive(Clone, Debug, Deserialize)]
pub struct Definition {
    pub name: String,
    pub kind: Kind,
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
    /// Why no engine takes the setting, when none does.
    #[serde(default)]
    pub refused: Option<String>,
    /// What each engine needs to apply the setting, keyed by engine id.
    #[serde(default)]
    pub engine: BTreeMap<String, serde_json::Value>,
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

/// The identity interface's own properties, which every offering accepts
/// without listing them. The others, `emu-id` and `mac`, have options.
pub const IDENTITY: &[&str] = &["serial", "hostid"];

/// A key split into its setting name and slot.
pub fn split(key: &str) -> (&str, Option<&str>) {
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

pub fn definition<'a>(machines: &'a Machines, name: &str) -> Result<&'a Definition> {
    machines
        .setting_definitions
        .iter()
        .find(|definition| definition.name == name)
        .ok_or_else(|| format!("the machine list does not define the setting {name}").into())
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
pub fn integer(key: &str, setting: &OfferingSetting, value: &str) -> Result<u64> {
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
        if name == "emu-id" {
            return Err("set the machine's emu-id with --emu-id when creating it".into());
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
        if let Some(reason) = &definition.refused {
            return Err(reason.clone().into());
        }
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
            let path = dunce::canonicalize(&value)?;
            Ok((key, path.display().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{preset, test_machines};
    use std::path::Path;

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
}
