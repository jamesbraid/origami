//! Graphics pipes, chosen from the catalogue QEMU reports in
//! `query-sgi-machines`. The launcher holds no graphics knowledge of its
//! own: each topology's record names its slots, the families and products
//! each takes, every setting's values and default, and the reasons a
//! combination is refused. A machine stores each pipe it has, with every
//! setting resolved, and each stored pipe becomes one `-device`.

use crate::{Machines, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A topology's graphics record.
#[derive(Clone, Debug, Deserialize)]
pub struct Graphics {
    #[serde(rename = "max-pipes")]
    pub max_pipes: u32,
    /// Whether the launcher offers the topology without graphics.
    pub headless: bool,
    #[serde(rename = "headless-refused")]
    pub headless_refused: Option<String>,
    pub slots: Vec<Slot>,
    /// Machine properties a machine with any pipe adds to `-M`.
    #[serde(rename = "machine-properties")]
    pub machine_properties: Option<MachineProperties>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MachineProperties {
    pub properties: Vec<Property>,
    pub reason: String,
}

/// The `-M` properties `topology` needs once it has a pipe.
pub fn machine_properties<'a>(
    machines: &'a Machines,
    topology: &str,
    pipes: &[Pipe],
) -> &'a [Property] {
    if pipes.is_empty() {
        return &[];
    }
    machines
        .topologies
        .iter()
        .find(|t| t.name == topology)
        .and_then(|t| t.graphics.as_ref())
        .and_then(|g| g.machine_properties.as_ref())
        .map_or(&[], |m| m.properties.as_slice())
}

#[derive(Clone, Debug, Deserialize)]
pub struct Slot {
    pub name: String,
    pub description: String,
    /// The `-device` properties that put a pipe here.
    pub placement: Vec<Property>,
    /// The pipe the machine fits when it is given no graphics device.
    pub default: Option<DefaultPipe>,
    pub changeable: bool,
    pub requires: Option<Requirement>,
    pub families: Vec<Family>,
    #[serde(default)]
    pub refusals: Vec<Refusal>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Requirement {
    #[serde(rename = "min-nodes")]
    pub min_nodes: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Family {
    pub family: String,
    /// The `-device` type that builds the family.
    pub device: String,
    pub name: String,
    pub products: Vec<Product>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Product {
    pub product: String,
    pub name: String,
    pub description: String,
    pub fidelity: String,
    #[serde(default)]
    pub limitations: Vec<String>,
    pub settings: Vec<Setting>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Setting {
    pub name: String,
    pub description: String,
    pub values: Vec<Value>,
    pub default: String,
    pub changeable: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Value {
    pub value: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DefaultPipe {
    pub family: String,
    pub product: String,
    pub settings: Vec<Property>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Refusal {
    pub family: String,
    pub product: Option<String>,
    #[serde(default)]
    pub settings: Vec<Property>,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Property {
    pub name: String,
    pub value: String,
}

/// One pipe as machine.toml stores it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pipe {
    pub slot: String,
    pub family: String,
    pub product: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, String>,
}

/// What a user asked for: a product or `none`, a slot, and settings. Any
/// left out come from the slot's catalogue default.
#[derive(Clone, Debug, Default)]
pub struct Request<'a> {
    pub product: Option<&'a str>,
    pub slot: Option<&'a str>,
    pub settings: BTreeMap<String, String>,
}

/// The graphics record of `topology`; None where it has no graphics slot.
pub fn record<'a>(machines: &'a Machines, topology: &str) -> Option<&'a Graphics> {
    machines
        .topologies
        .iter()
        .find(|t| t.name == topology)
        .and_then(|t| t.graphics.as_ref())
}

/// The slots a machine of `nodes` nodes has.
pub fn slots(graphics: &Graphics, nodes: u32) -> impl Iterator<Item = &Slot> {
    graphics.slots.iter().filter(move |slot| {
        slot.requires
            .as_ref()
            .and_then(|r| r.min_nodes)
            .is_none_or(|min| nodes >= min)
    })
}

impl Slot {
    fn offer(&self, product: &str) -> Option<(&Family, &Product)> {
        self.families.iter().find_map(|family| {
            family
                .products
                .iter()
                .find(|p| p.product == product)
                .map(|p| (family, p))
        })
    }

    fn products(&self) -> Vec<&str> {
        self.families
            .iter()
            .flat_map(|f| f.products.iter().map(|p| p.product.as_str()))
            .collect()
    }

    /// The first refusal matching `family`, `product` and `settings`.
    fn refusal(
        &self,
        family: &str,
        product: &str,
        settings: &BTreeMap<String, String>,
    ) -> Option<&Refusal> {
        self.refusals.iter().find(|r| {
            r.family == family
                && r.product.as_deref().is_none_or(|p| p == product)
                && r.settings
                    .iter()
                    .all(|s| settings.get(&s.name) == Some(&s.value))
        })
    }
}

/// The family and name of `product` wherever the catalogue offers it.
fn catalogue_product<'a>(
    machines: &'a Machines,
    product: &str,
) -> Option<(&'a Family, &'a Product)> {
    machines
        .topologies
        .iter()
        .filter_map(|t| t.graphics.as_ref())
        .flat_map(|g| &g.slots)
        .find_map(|slot| slot.offer(product))
}

fn slot_names(graphics: &Graphics, nodes: u32) -> String {
    slots(graphics, nodes)
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The pipes `request` selects on `topology` with `nodes` nodes.
pub fn select(
    machines: &Machines,
    topology: &str,
    nodes: u32,
    request: &Request<'_>,
) -> Result<Vec<Pipe>> {
    let Some(graphics) = record(machines, topology) else {
        if request.product.is_none_or(|p| p == "none")
            && request.slot.is_none()
            && request.settings.is_empty()
        {
            return Ok(vec![]);
        }
        return Err(format!("{topology} has no graphics slots").into());
    };
    if request.product == Some("none") {
        if request.slot.is_some() || !request.settings.is_empty() {
            return Err("--graphics none takes no slot or settings".into());
        }
        return headless(topology, graphics);
    }
    let slot = match (request.slot, request.product) {
        (Some(name), _) => slots(graphics, nodes)
            .find(|s| s.name == name)
            .ok_or_else(|| {
                format!(
                    "{topology} has no graphics slot {name}; its slots are {}",
                    slot_names(graphics, nodes)
                )
            })?,
        (None, product) => {
            let default = slots(graphics, nodes).find(|s| s.default.is_some());
            match product {
                None => match default {
                    Some(slot) => slot,
                    None if request.settings.is_empty() => return Ok(vec![]),
                    None => {
                        return Err(format!(
                            "{topology} has no default pipe; choose one with --graphics"
                        )
                        .into())
                    }
                },
                Some(product) => default
                    .filter(|s| s.offer(product).is_some())
                    .or_else(|| slots(graphics, nodes).find(|s| s.offer(product).is_some()))
                    .ok_or_else(|| {
                        refused_everywhere(machines, topology, graphics, nodes, product)
                    })?,
            }
        }
    };
    let product = match (request.product, &slot.default) {
        (Some(product), _) => product,
        (None, Some(default)) => default.product.as_str(),
        (None, None) => {
            return Err(format!(
                "{topology}'s {} has no default pipe; choose one with --graphics",
                slot.name
            )
            .into())
        }
    };
    let base = slot
        .default
        .as_ref()
        .filter(|d| d.product == product)
        .map(|d| {
            d.settings
                .iter()
                .map(|s| (s.name.clone(), s.value.clone()))
                .collect()
        });
    Ok(vec![resolve(
        machines,
        topology,
        slot,
        product,
        base,
        &request.settings,
    )?])
}

fn headless(topology: &str, graphics: &Graphics) -> Result<Vec<Pipe>> {
    if graphics.headless {
        return Ok(vec![]);
    }
    Err(format!(
        "{topology} is not offered without graphics: {}",
        graphics
            .headless_refused
            .as_deref()
            .unwrap_or("its catalogue record says so")
    )
    .into())
}

/// The error for `product`, which no slot of `topology` offers.
fn refused_everywhere(
    machines: &Machines,
    topology: &str,
    graphics: &Graphics,
    nodes: u32,
    product: &str,
) -> Box<dyn std::error::Error> {
    if let Some((family, named)) = catalogue_product(machines, product) {
        for slot in slots(graphics, nodes) {
            if let Some(refusal) = slot.refusal(&family.family, product, &BTreeMap::new()) {
                return format!(
                    "{topology} does not take {} ({product}) in {}: {}",
                    named.name, slot.name, refusal.reason
                )
                .into();
            }
        }
    }
    let mut offered: Vec<&str> = slots(graphics, nodes).flat_map(|s| s.products()).collect();
    offered.sort();
    offered.dedup();
    format!(
        "{topology} offers no graphics product {product}; choose {}",
        offered.join(", ")
    )
    .into()
}

/// The pipe of `product` in `slot` with `settings` over `base`, or over
/// the catalogue's defaults when there is no base.
fn resolve(
    machines: &Machines,
    topology: &str,
    slot: &Slot,
    product: &str,
    base: Option<BTreeMap<String, String>>,
    settings: &BTreeMap<String, String>,
) -> Result<Pipe> {
    let Some((family, offered)) = slot.offer(product) else {
        if let Some((family, named)) = catalogue_product(machines, product) {
            if let Some(refusal) = slot.refusal(&family.family, product, settings) {
                return Err(format!(
                    "{topology} does not take {} ({product}) in {}: {}",
                    named.name, slot.name, refusal.reason
                )
                .into());
            }
        }
        return Err(format!(
            "{} of {topology} offers no graphics product {product}; choose {}",
            slot.name,
            slot.products().join(", ")
        )
        .into());
    };
    let mut resolved = base.unwrap_or_else(|| {
        offered
            .settings
            .iter()
            .map(|s| (s.name.clone(), s.default.clone()))
            .collect()
    });
    for (name, value) in settings {
        if !offered.settings.iter().any(|s| &s.name == name) {
            let names: Vec<_> = offered.settings.iter().map(|s| s.name.as_str()).collect();
            return Err(if names.is_empty() {
                format!("{product} takes no graphics settings")
            } else {
                format!(
                    "{product} has no graphics setting {name}; it takes {}",
                    names.join(", ")
                )
            }
            .into());
        }
        resolved.insert(name.clone(), value.clone());
    }
    if let Some(refusal) = slot.refusal(&family.family, product, &resolved) {
        let choice: Vec<_> = refusal
            .settings
            .iter()
            .map(|s| format!(" with {}={}", s.name, s.value))
            .collect();
        return Err(format!(
            "{topology} does not take {} ({product}){} in {}: {}",
            offered.name,
            choice.concat(),
            slot.name,
            refusal.reason
        )
        .into());
    }
    for setting in &offered.settings {
        let value = resolved.get(&setting.name).ok_or_else(|| {
            format!(
                "the pipe in {} gives no {} value; it is offered {}",
                slot.name,
                setting.name,
                values(setting)
            )
        })?;
        if !setting.values.iter().any(|v| &v.value == value) {
            return Err(format!(
                "{product} in {} of {topology} does not offer {}={value}; choose {}",
                slot.name,
                setting.name,
                values(setting)
            )
            .into());
        }
    }
    Ok(Pipe {
        slot: slot.name.clone(),
        family: family.family.clone(),
        product: product.into(),
        settings: resolved,
    })
}

fn values(setting: &Setting) -> String {
    setting
        .values
        .iter()
        .map(|v| v.value.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Check stored `pipes` against the catalogue: each must be one QEMU builds
/// on this topology, with every setting the product has.
pub fn validate(machines: &Machines, topology: &str, nodes: u32, pipes: &[Pipe]) -> Result<()> {
    let Some(graphics) = record(machines, topology) else {
        return match pipes.first() {
            Some(_) => Err(format!("{topology} has no graphics slots").into()),
            None => Ok(()),
        };
    };
    if pipes.is_empty() {
        return headless(topology, graphics).map(|_| ());
    }
    if pipes.len() > graphics.max_pipes as usize {
        return Err(format!(
            "{topology} builds at most {} graphics pipe(s)",
            graphics.max_pipes
        )
        .into());
    }
    for (index, pipe) in pipes.iter().enumerate() {
        if pipes[..index].iter().any(|p| p.slot == pipe.slot) {
            return Err(format!("two graphics pipes in {}", pipe.slot).into());
        }
        let request = Request {
            product: Some(&pipe.product),
            slot: Some(&pipe.slot),
            settings: pipe.settings.clone(),
        };
        let resolved = select(machines, topology, nodes, &request)?;
        if let Some(resolved) = resolved.first().filter(|r| *r != pipe) {
            let names: Vec<_> = resolved.settings.keys().map(String::as_str).collect();
            return Err(format!(
                "the {} pipe in {} must list exactly the settings {}",
                pipe.product,
                pipe.slot,
                names.join(", ")
            )
            .into());
        }
    }
    Ok(())
}

/// The graphics arguments for `pipes`: one `-device` each, or `-vga none`
/// for a machine with no pipe whose topology would fit a default one.
pub fn arguments(machines: &Machines, topology: &str, pipes: &[Pipe]) -> Result<Vec<String>> {
    let Some(graphics) = record(machines, topology) else {
        return Ok(vec![]);
    };
    if pipes.is_empty() {
        return Ok(if graphics.slots.iter().any(|s| s.default.is_some()) {
            vec!["-vga".into(), "none".into()]
        } else {
            vec![]
        });
    }
    let mut args = vec![];
    for pipe in pipes {
        let (slot, family, product) = graphics
            .slots
            .iter()
            .find(|s| s.name == pipe.slot)
            .and_then(|slot| slot.offer(&pipe.product).map(|(f, p)| (slot, f, p)))
            .ok_or_else(|| {
                format!(
                    "{topology} does not offer {} in {}",
                    pipe.product, pipe.slot
                )
            })?;
        let mut device = family.device.clone();
        for property in &slot.placement {
            device.push_str(&format!(",{}={}", property.name, property.value));
        }
        device.push_str(&format!(",product={}", pipe.product));
        for setting in &product.settings {
            if let Some(value) = pipe.settings.get(&setting.name) {
                device.push_str(&format!(",{}={value}", setting.name));
            }
        }
        args.extend(["-device".into(), device]);
    }
    Ok(args)
}

/// Refuse changing `old` pipes to `new` where the catalogue says the slot
/// or setting cannot change between boots.
pub fn check_change(machines: &Machines, topology: &str, old: &[Pipe], new: &[Pipe]) -> Result<()> {
    let Some(graphics) = record(machines, topology) else {
        return Ok(());
    };
    let refuse = |what: String| -> Result<()> {
        Err(format!(
            "{what} cannot change after the machine is created; create a new machine \
             with the graphics you want"
        )
        .into())
    };
    for slot in &graphics.slots {
        let before = old.iter().find(|p| p.slot == slot.name);
        let after = new.iter().find(|p| p.slot == slot.name);
        match (before, after) {
            (None, None) => (),
            (Some(a), Some(b)) if a.family == b.family && a.product == b.product => {
                let Some((_, product)) = slot.offer(&a.product) else {
                    continue;
                };
                for setting in &product.settings {
                    if a.settings.get(&setting.name) != b.settings.get(&setting.name)
                        && !setting.changeable
                    {
                        refuse(format!("{} of the pipe in {}", setting.name, slot.name))?;
                    }
                }
            }
            _ if !slot.changeable => refuse(format!("the pipe in {}", slot.name))?,
            _ => (),
        }
    }
    Ok(())
}

/// A pipe in words: "Solid IMPACT (SI) (si) in widget-11".
pub fn describe(machines: &Machines, topology: &str, pipe: &Pipe) -> String {
    let name = record(machines, topology)
        .and_then(|g| g.slots.iter().find(|s| s.name == pipe.slot))
        .and_then(|s| s.offer(&pipe.product))
        .map_or(pipe.product.as_str(), |(_, p)| p.name.as_str());
    format!("{name} ({}) in {}", pipe.product, pipe.slot)
}

/// What `show` prints for a machine's graphics.
pub fn show_lines(machines: &Machines, topology: &str, pipes: &[Pipe]) -> Vec<String> {
    if pipes.is_empty() {
        return vec!["graphics: none".into()];
    }
    let mut lines = vec![];
    for pipe in pipes {
        let slot =
            record(machines, topology).and_then(|g| g.slots.iter().find(|s| s.name == pipe.slot));
        let mut line = format!("graphics: {}", describe(machines, topology, pipe));
        if let Some(slot) = slot {
            line.push_str(&format!(", {}", slot.description));
        }
        lines.push(line);
        let product = slot.and_then(|s| s.offer(&pipe.product)).map(|(_, p)| p);
        for setting in product.map_or(&[][..], |p| &p.settings) {
            if let Some(value) = pipe.settings.get(&setting.name) {
                lines.push(format!("  {}: {value}", setting.name));
            }
        }
        if let Some(product) = product {
            lines.push(format!("  fidelity: {}", product.fidelity));
            for limitation in &product.limitations {
                lines.push(format!("  limitation: {limitation}"));
            }
        }
    }
    lines
}

/// The name of `family` wherever the catalogue offers it.
pub fn family_name<'a>(machines: &'a Machines, family: &str) -> Option<&'a str> {
    machines
        .topologies
        .iter()
        .filter_map(|t| t.graphics.as_ref())
        .flat_map(|g| &g.slots)
        .flat_map(|s| &s.families)
        .find(|f| f.family == family)
        .map(|f| f.name.as_str())
}

/// The name of `product` wherever the catalogue offers it.
pub fn product_name<'a>(machines: &'a Machines, product: &str) -> Option<&'a str> {
    catalogue_product(machines, product).map(|(_, p)| p.name.as_str())
}

/// The pipes for a format 3 machine's `graphics` value, and what changes
/// for its guest, following the upgrade table of the graphics pipes
/// design. Settings not named take the slot's catalogue defaults.
pub fn from_legacy(
    machines: &Machines,
    topology: &str,
    nodes: u32,
    graphics: &str,
) -> Result<(Vec<Pipe>, Option<&'static str>)> {
    let pick = |product: Option<&str>, slot: Option<&str>| {
        select(
            machines,
            topology,
            nodes,
            &Request {
                product,
                slot,
                settings: BTreeMap::new(),
            },
        )
    };
    let infinite_reality = |slot: &str| -> Result<(Vec<Pipe>, Option<&'static str>)> {
        let reason = record(machines, topology)
            .and_then(|g| g.slots.iter().find(|s| s.name == slot))
            .and_then(|s| s.refusals.iter().find(|r| r.family == "kona"))
            .map_or_else(String::new, |r| format!(": {}", r.reason));
        Err(format!(
            "topology '{topology}' does not take an InfiniteReality pipe in {slot}{reason}"
        )
        .into())
    };
    Ok(match (topology, graphics) {
        ("fuel", "none") => (
            pick(None, None)?,
            Some("the Fuel gains the VPro V12 at widget 13 that its old topology left out"),
        ),
        (_, "none") => (vec![], None),
        ("origin200", "rad4") => (pick(Some("rad4"), Some("pci-5"))?, None),
        ("origin200-dual", "rad4") => (
            pick(Some("rad4"), Some("m0-pci-5"))?,
            Some("the RAD4 moves from module 1's PCI bus to module 0's, device 5"),
        ),
        ("origin200-gigachannel", "si" | "esi") => (
            pick(Some(graphics), Some("io6"))?,
            Some(
                "the IMPACT board moves from io3 to io6, Crossbow widget 15, where \
                 GIGAchannel machines carry graphics",
            ),
        ),
        ("origin2000-deskside" | "origin2000-rack", "si" | "esi") => {
            (pick(Some(graphics), Some("io3"))?, None)
        }
        ("origin200-gigachannel", "infinite-reality") => infinite_reality("io6")?,
        ("origin2000-deskside" | "origin2000-rack", "infinite-reality") => infinite_reality("io3")?,
        ("onyx2-deskside", "infinite-reality") => (
            pick(None, None)?,
            Some(
                "the InfiniteReality2 pipe now answers with its boards configured, so hinv \
                 and gfxinfo list its raster managers, texture and display boards",
            ),
        ),
        ("onyx2-rack", "infinite-reality") => (pick(None, None)?, None),
        ("octane" | "octane2", "si") => (pick(Some("si"), Some("widget-11"))?, None),
        _ => {
            return Err(format!(
                "no earlier Origami stored graphics '{graphics}' for topology '{topology}'"
            )
            .into())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(
        product: Option<&'a str>,
        slot: Option<&'a str>,
        settings: &[(&str, &str)],
    ) -> Request<'a> {
        Request {
            product,
            slot,
            settings: settings
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn the_catalogue_record_gives_slots_defaults_and_reasons() {
        let machines = crate::test_machines();
        let onyx2 = record(&machines, "onyx2-deskside").unwrap();
        assert!(!onyx2.headless);
        assert!(onyx2
            .headless_refused
            .as_deref()
            .unwrap()
            .contains("origin2000-deskside"));
        let io4 = &onyx2.slots[0];
        assert_eq!(io4.placement[0].name, "slot");
        assert!(!io4.changeable);
        let default = io4.default.as_ref().unwrap();
        assert_eq!(
            (default.family.as_str(), default.product.as_str()),
            ("kona", "ir2")
        );
        let (family, ir2) = io4.offer("ir2").unwrap();
        assert_eq!(family.device, "sgi-kona");
        let rm_count = ir2.settings.iter().find(|s| s.name == "rm-count").unwrap();
        assert_eq!(values(rm_count), "1, 2");
        assert!(ir2.settings.iter().all(|s| !s.changeable));
        // Slots past the first node board need a second.
        let rack = record(&machines, "origin2000-rack").unwrap();
        assert!(slots(rack, 1).all(|s| s.name != "io7"));
        assert!(slots(rack, 2).any(|s| s.name == "io7"));
        assert!(record(&machines, "origin300").is_none());
    }

    #[test]
    fn selection_takes_the_catalogue_default_and_quotes_its_refusals() {
        let machines = crate::test_machines();
        let select = |topology: &str, nodes, r: Request<'_>| select(&machines, topology, nodes, &r);
        let args = |topology: &str, pipes: &[Pipe]| arguments(&machines, topology, pipes).unwrap();

        let pipes = select("onyx2-deskside", 2, request(None, None, &[])).unwrap();
        assert_eq!(
            args("onyx2-deskside", &pipes),
            [
                "-device",
                "sgi-kona,slot=io4,product=ir2,geometry=ge14-4,rm-type=rm7,rm-count=2,\
                 texture-mb=64,display=dg5-8"
            ]
        );
        let pipes = select(
            "onyx2-deskside",
            2,
            request(
                Some("ir2"),
                None,
                &[("rm-count", "1"), ("display", "dg5-2")],
            ),
        )
        .unwrap();
        assert_eq!(pipes[0].settings["rm-count"], "1");
        assert_eq!(pipes[0].settings["geometry"], "ge14-4");

        let pipes = select("octane", 1, request(Some("none"), None, &[])).unwrap();
        assert_eq!(args("octane", &pipes), ["-vga", "none"]);
        assert!(args("origin200", &[]).is_empty());
        let pipes = select("origin200", 1, request(Some("rad4"), None, &[])).unwrap();
        assert_eq!(
            args("origin200", &pipes),
            ["-device", "psitech-rad4,bus=pci,addr=5,product=rad4"]
        );
        let pipes = select("origin2000-rack", 4, request(Some("si"), Some("io3"), &[])).unwrap();
        assert_eq!(
            args("origin2000-rack", &pipes),
            ["-device", "sgi-mgras,slot=io3,product=si"]
        );

        for (topology, r, expected) in [
            (
                "onyx2-deskside",
                request(Some("ir2"), None, &[("rm-count", "4")]),
                "onyx2-deskside does not take InfiniteReality2 (ir2) with rm-count=4 in io4: \
                 a pipe with no Ktown is an Onyx2 deskside pipe, which holds at most two raster \
                 managers",
            ),
            (
                "onyx2-deskside",
                request(Some("ir3"), None, &[]),
                "onyx2-deskside does not take InfiniteReality3 (ir3) in io4: no manual or \
                 capture places an InfiniteReality3 in an Onyx2 deskside",
            ),
            (
                "octane",
                request(Some("v12"), None, &[]),
                "octane does not take VPro V12 (v12) in widget-11: VPro V12 needs Crossbow 1.4",
            ),
            (
                "fuel",
                request(Some("none"), None, &[]),
                "fuel is not offered without graphics: every Fuel has its VPro graphics slot",
            ),
            (
                "octane2",
                request(Some("v8"), None, &[]),
                "octane2 offers no graphics product v8",
            ),
            (
                "origin2000-rack",
                request(Some("si"), Some("io9"), &[]),
                "has no graphics slot io9",
            ),
            (
                "onyx2-rack",
                request(Some("ir3"), None, &[("display", "dg5-2-gvo")]),
                "choose dg5-2, dg5-8",
            ),
            (
                "origin300",
                request(Some("si"), None, &[]),
                "origin300 has no graphics slots",
            ),
        ] {
            let error = select(topology, 4, r).unwrap_err().to_string();
            assert!(error.contains(expected), "{topology}: {error}");
        }
    }

    #[test]
    fn upgrades_follow_the_design_table() {
        let machines = crate::test_machines();
        let pipe = |slot: &str, family: &str, product: &str| {
            (slot.to_string(), family.to_string(), product.to_string())
        };
        for (topology, nodes, value, expected) in [
            ("origin200", 1, "none", Ok(None)),
            ("origin200-dual", 2, "none", Ok(None)),
            (
                "origin200",
                1,
                "rad4",
                Ok(Some(pipe("pci-5", "psitech", "rad4"))),
            ),
            (
                "origin200-dual",
                2,
                "rad4",
                Ok(Some(pipe("m0-pci-5", "psitech", "rad4"))),
            ),
            ("origin200-gigachannel", 1, "none", Ok(None)),
            (
                "origin200-gigachannel",
                1,
                "si",
                Ok(Some(pipe("io6", "mgras", "si"))),
            ),
            (
                "origin200-gigachannel",
                1,
                "esi",
                Ok(Some(pipe("io6", "mgras", "esi"))),
            ),
            (
                "origin200-gigachannel",
                1,
                "infinite-reality",
                Err(
                    "topology 'origin200-gigachannel' does not take an InfiniteReality pipe in \
                     io6: SGI did not offer InfiniteReality on the Origin 200, and the known \
                     GIGAchannel pipe is an owner conversion the model does not build",
                ),
            ),
            ("origin2000-deskside", 1, "none", Ok(None)),
            ("origin2000-rack", 4, "none", Ok(None)),
            (
                "origin2000-deskside",
                1,
                "si",
                Ok(Some(pipe("io3", "mgras", "si"))),
            ),
            (
                "origin2000-rack",
                4,
                "esi",
                Ok(Some(pipe("io3", "mgras", "esi"))),
            ),
            (
                "origin2000-deskside",
                1,
                "infinite-reality",
                Err(
                    "topology 'origin2000-deskside' does not take an InfiniteReality pipe in \
                     io3: an Origin 2000 with an InfiniteReality pipe is an Onyx2, so use \
                     onyx2-deskside",
                ),
            ),
            (
                "origin2000-rack",
                4,
                "infinite-reality",
                Err(
                    "topology 'origin2000-rack' does not take an InfiniteReality pipe in io3: \
                     an Origin 2000 with an InfiniteReality pipe is an Onyx2, so use onyx2-rack",
                ),
            ),
            (
                "onyx2-deskside",
                2,
                "infinite-reality",
                Ok(Some(pipe("io4", "kona", "ir2"))),
            ),
            (
                "onyx2-rack",
                2,
                "infinite-reality",
                Ok(Some(pipe("io3", "kona", "ir3"))),
            ),
            ("octane", 1, "none", Ok(None)),
            (
                "octane",
                1,
                "si",
                Ok(Some(pipe("widget-11", "mgras", "si"))),
            ),
            ("octane2", 1, "none", Ok(None)),
            (
                "octane2",
                1,
                "si",
                Ok(Some(pipe("widget-11", "mgras", "si"))),
            ),
            (
                "fuel",
                1,
                "none",
                Ok(Some(pipe("widget-13", "odyssey", "v12"))),
            ),
            ("origin300", 1, "none", Ok(None)),
        ] {
            let result = from_legacy(&machines, topology, nodes, value);
            match (result, expected) {
                (Ok((pipes, _)), Ok(expected)) => {
                    let got = pipes
                        .first()
                        .map(|p| (p.slot.clone(), p.family.clone(), p.product.clone()));
                    assert_eq!(got, expected, "{topology} {value}");
                    validate(&machines, topology, nodes, &pipes).unwrap();
                }
                (Err(error), Err(expected)) => {
                    assert_eq!(error.to_string(), expected, "{topology} {value}")
                }
                (result, expected) => panic!("{topology} {value}: {result:?} against {expected:?}"),
            }
        }
        // Settings come resolved from the catalogue default.
        let (pipes, note) =
            from_legacy(&machines, "onyx2-deskside", 2, "infinite-reality").unwrap();
        assert_eq!(pipes[0].settings["rm-count"], "2");
        assert_eq!(pipes[0].settings["display"], "dg5-8");
        assert!(note.is_some());
    }

    #[test]
    fn graphics_cannot_change_after_create() {
        let machines = crate::test_machines();
        let pick = |r: Request<'_>| select(&machines, "onyx2-deskside", 2, &r).unwrap();
        let default = pick(request(None, None, &[]));
        let one_rm = pick(request(Some("ir2"), None, &[("rm-count", "1")]));
        let ir2e = pick(request(Some("ir2e"), None, &[]));
        assert!(check_change(&machines, "onyx2-deskside", &default, &default).is_ok());
        for (new, what) in [
            (&one_rm, "rm-count of the pipe in io4"),
            (&ir2e, "the pipe in io4"),
        ] {
            let error = check_change(&machines, "onyx2-deskside", &default, new)
                .unwrap_err()
                .to_string();
            assert_eq!(
                error,
                format!(
                    "{what} cannot change after the machine is created; create a new machine \
                     with the graphics you want"
                )
            );
        }
        let si = select(&machines, "octane", 1, &request(Some("si"), None, &[])).unwrap();
        assert!(check_change(&machines, "octane", &si, &[]).is_err());
    }
}
