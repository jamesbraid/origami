//! What `origami machines` prints: a table of presets, or one preset's
//! graphics and processor choices. Everything but the machine names comes
//! from the machine list QEMU reports.

use crate::graphics::{self, Pipe, Product, Slot};
use crate::{Machines, Offering, Result};

/// Where the output wraps.
const WIDTH: usize = 80;

/// Machine names by topology, in the order the table lists families. The
/// machine list carries no product names, so these are the one thing here
/// QEMU does not supply; a topology missing here keeps its own name.
const NAMES: &[(&str, &str)] = &[
    ("origin200", "Origin 200"),
    ("origin200-dual", "Origin 200 dual"),
    ("origin200-gigachannel", "Origin 200"),
    ("origin2000-deskside", "Origin 2000 deskside"),
    ("origin2000-rack", "Origin 2000 rack"),
    ("onyx2-deskside", "Onyx2 deskside"),
    ("onyx2-rack", "Onyx2 rack"),
    ("origin300", "Origin 300"),
    ("onyx350-compat", "Onyx 350"),
    ("octane", "Octane"),
    ("octane2", "Octane2"),
    ("fuel", "Fuel"),
    ("tezro-tower-multi", "Tezro"),
];

pub(crate) fn machine_name(topology: &str) -> &str {
    NAMES
        .iter()
        .find(|(name, _)| *name == topology)
        .map_or(topology, |(_, human)| human)
}

/// The presets in family order, a family keeping the order the presets are
/// defined in.
fn ordered(machines: &Machines) -> Vec<(String, &Offering)> {
    let mut presets = crate::presets(machines);
    let rank = |offering: &Offering| {
        NAMES
            .iter()
            .position(|(name, _)| *name == offering.topology)
            .unwrap_or(NAMES.len())
    };
    presets.sort_by_key(|(_, offering)| rank(offering));
    presets
}

/// The pipes a preset fits when created without graphics options.
fn default_pipes(machines: &Machines, name: &str, offering: &Offering) -> Result<Vec<Pipe>> {
    let request = crate::profiles::graphics_request(name, None, None, Default::default());
    graphics::select(machines, &offering.topology, offering.nodes, &request)
}

fn find_slot<'a>(machines: &'a Machines, offering: &Offering, name: &str) -> Option<&'a Slot> {
    graphics::record(machines, &offering.topology)?
        .slots
        .iter()
        .find(|slot| slot.name == name)
}

fn find_product<'a>(
    machines: &'a Machines,
    offering: &Offering,
    pipe: &Pipe,
) -> Option<&'a Product> {
    let slot = find_slot(machines, offering, &pipe.slot)?;
    let family = slot.families.iter().find(|f| f.family == pipe.family)?;
    family.products.iter().find(|p| p.product == pipe.product)
}

/// A pipe's product named briefly: the name in its last brackets where it
/// has some ("Solid IMPACT (SI)" is SI), else the whole name. A pipe in a
/// GIGAchannel position says so.
fn short_name(machines: &Machines, offering: &Offering, pipe: &Pipe) -> String {
    let name = find_product(machines, offering, pipe).map_or(pipe.product.as_str(), |p| &p.name);
    let short = match name
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" ("))
    {
        Some((_, inner)) => inner,
        None => name,
    };
    let gigachannel = find_slot(machines, offering, &pipe.slot)
        .is_some_and(|slot| slot.description.starts_with("GIGAchannel"));
    if gigachannel {
        format!("{short} (GIGAchannel)")
    } else {
        short.to_string()
    }
}

/// `text` wrapped to the output width, later lines indented by `indent`.
fn wrap(text: &str, indent: usize) -> String {
    let lead = text.len() - text.trim_start().len();
    let mut out = " ".repeat(lead);
    let mut line = lead;
    let mut first = true;
    for word in text.split_whitespace() {
        if first {
            first = false;
        } else if line + 1 + word.len() > WIDTH {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            line = indent;
        } else {
            out.push(' ');
            line += 1;
        }
        out.push_str(word);
        line += word.len();
    }
    out
}

fn plural(count: u32, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// One table of the presets: name, machine, CPUs and default graphics.
pub fn table(machines: &Machines) -> Result<String> {
    let mut rows = vec![];
    for (name, offering) in ordered(machines) {
        let pipes = default_pipes(machines, &name, offering)?;
        let graphics = if pipes.is_empty() {
            "none".to_string()
        } else {
            pipes
                .iter()
                .map(|pipe| short_name(machines, offering, pipe))
                .collect::<Vec<_>>()
                .join(", ")
        };
        rows.push((
            name,
            machine_name(&offering.topology),
            offering.smp.to_string(),
            graphics,
        ));
    }
    let width = |column: fn(&(String, &str, String, String)) -> usize| {
        rows.iter().map(column).max().unwrap_or(0)
    };
    let (preset, machine, cpus) = (
        width(|r| r.0.len()).max("PRESET".len()),
        width(|r| r.1.len()).max("MACHINE".len()),
        width(|r| r.2.len()).max("CPUS".len()),
    );
    let mut out = format!(
        "{:preset$}  {:machine$}  {:>cpus$}  GRAPHICS\n",
        "PRESET", "MACHINE", "CPUS"
    );
    for (name, machine_name, smp, graphics) in &rows {
        out.push_str(&format!(
            "{name:preset$}  {machine_name:machine$}  {smp:>cpus$}  {graphics}\n"
        ));
    }
    out.push_str("\nEverything here is experimental.\n");
    out.push_str("Details and graphics choices: origami machines PRESET\n");
    Ok(out)
}

/// The graphics refusals of a machine's slots, each once: what is refused
/// and why.
fn refusals(machines: &Machines, offering: &Offering) -> Vec<String> {
    let Some(record) = graphics::record(machines, &offering.topology) else {
        return vec![];
    };
    let mut refused: Vec<String> = vec![];
    for slot in graphics::slots(record, offering.nodes) {
        for refusal in &slot.refusals {
            let what = refusal
                .product
                .as_deref()
                .and_then(|product| graphics::product_name(machines, product))
                .or_else(|| graphics::family_name(machines, &refusal.family))
                .unwrap_or(&refusal.family);
            let settings: String = refusal
                .settings
                .iter()
                .map(|s| format!(" with {}={}", s.name, s.value))
                .collect();
            let line = format!("{what}{settings}: {}", refusal.reason);
            if !refused.contains(&line) {
                refused.push(line);
            }
        }
    }
    if !record.headless {
        if let Some(reason) = &record.headless_refused {
            refused.push(format!("No graphics: {reason}"));
        }
    }
    refused
}

/// A board's open settings as `name=a|b*|c`, the default starred. A slot
/// that fits the board by default may set its own default values.
fn settings(record: &graphics::Graphics, product: &Product) -> String {
    let fitted = record
        .slots
        .iter()
        .filter_map(|slot| slot.default.as_ref())
        .find(|default| default.product == product.product);
    product
        .settings
        .iter()
        .filter(|setting| setting.values.len() > 1)
        .map(|setting| {
            let chosen = fitted
                .and_then(|d| d.settings.iter().find(|s| s.name == setting.name))
                .map_or(setting.default.as_str(), |s| s.value.as_str());
            let values: Vec<_> = setting
                .values
                .iter()
                .map(|v| format!("{}{}", v.value, if v.value == chosen { "*" } else { "" }))
                .collect();
            format!("{}={}", setting.name, values.join("|"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One preset: its defaults, the graphics it takes and refuses, and the
/// processor modules it offers.
pub fn detail(machines: &Machines, name: &str) -> Result<String> {
    let offering = crate::preset(machines, name)?;
    detail_of(machines, name, offering)
}

fn detail_of(machines: &Machines, name: &str, offering: &Offering) -> Result<String> {
    let pipes = default_pipes(machines, name, offering)?;
    let mut header = format!(
        "{name}: {}, {}, {} MiB per node, ",
        machine_name(&offering.topology),
        plural(offering.smp, "CPU"),
        offering.memory.default
    );
    match pipes.as_slice() {
        [] => header.push_str("no graphics"),
        pipes => {
            let fitted: Vec<_> = pipes
                .iter()
                .map(|pipe| format!("{} in {}", short_name(machines, offering, pipe), pipe.slot))
                .collect();
            header.push_str(&fitted.join(", "));
        }
    }
    let mut out = wrap(&header, 4) + "\n";

    if let Some(record) = graphics::record(machines, &offering.topology) {
        let mut choices: Vec<(&str, &str, String)> = vec![];
        for slot in graphics::slots(record, offering.nodes) {
            for product in slot.families.iter().flat_map(|f| &f.products) {
                if !choices.iter().any(|(id, ..)| *id == product.product) {
                    choices.push((&product.product, &product.name, settings(record, product)));
                }
            }
        }
        if !choices.is_empty() || record.headless {
            out.push_str(
                "\nGraphics (create --graphics PRODUCT [--graphics-slot SLOT]; * = default):\n",
            );
            let id_width = choices.iter().map(|(id, ..)| id.len()).max().unwrap_or(0);
            let id_width = id_width.max("none".len());
            let marked = |chosen: bool| if chosen { " (default)" } else { "" };
            for (id, product_name, settings) in choices {
                let chosen = pipes.iter().any(|pipe| pipe.product == id);
                out.push_str(&format!(
                    "  {id:id_width$}  {product_name}{}\n",
                    marked(chosen)
                ));
                if !settings.is_empty() {
                    let indent = id_width + 4;
                    let line = format!("{}settings: {settings}", " ".repeat(indent));
                    out.push_str(&wrap(&line, indent + "settings: ".len()));
                    out.push('\n');
                }
            }
            if record.headless {
                out.push_str(&format!("  none{}\n", marked(pipes.is_empty())));
            }
        }
        let slots: Vec<_> = graphics::slots(record, offering.nodes)
            .map(|slot| slot.name.as_str())
            .collect();
        if !slots.is_empty() {
            out.push_str(&format!(
                "\n{}\n",
                wrap(&format!("Slots: {}", slots.join(", ")), 2)
            ));
        }
    }

    let refused = refusals(machines, offering);
    if !refused.is_empty() {
        out.push_str("\nNot offered:\n");
        for line in refused {
            out.push_str(&wrap(&format!("  {line}"), 4));
            out.push('\n');
        }
    }

    let modules = crate::profiles::processor_options(machines, &offering.topology);
    if !modules.is_empty() {
        let ids: Vec<_> = modules
            .iter()
            .enumerate()
            .map(|(index, module)| {
                let default = if index == 0 { " (default)" } else { "" };
                format!("{}{default}", module.assembly.id)
            })
            .collect();
        let line = format!(
            "Processor modules (create --processor ID): {}",
            ids.join(", ")
        );
        out.push_str(&format!("\n{}\n", wrap(&line, 2)));
    }
    Ok(out)
}

/// Every preset's detail, in table order, a blank line between.
pub fn all(machines: &Machines) -> Result<String> {
    let mut sections = vec![];
    for (name, offering) in ordered(machines) {
        sections.push(detail_of(machines, &name, offering)?);
    }
    Ok(sections.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_lists_each_preset_in_family_order() {
        assert_eq!(
            table(&crate::test_machines()).unwrap(),
            "\
PRESET         MACHINE           CPUS  GRAPHICS
origin200-1    Origin 200           1  none
origin2000-8   Origin 2000 rack     8  none
onyx350-vpro   Onyx 350             4  VPro V12
octane-impact  Octane               1  SI
fuel-1         Fuel                 1  VPro V12

Everything here is experimental.
Details and graphics choices: origami machines PRESET
"
        );
    }

    #[test]
    fn a_detail_view_lists_choices_slots_and_refusals_once() {
        let machines = crate::test_machines();
        assert_eq!(
            detail(&machines, "octane-impact").unwrap(),
            "\
octane-impact: Octane, 1 CPU, 128 MiB per node, SI in widget-11

Graphics (create --graphics PRODUCT [--graphics-slot SLOT]; * = default):
  si    Solid IMPACT (SI) (default)
  esi   ESI
  none

Slots: widget-11, widget-9

Not offered:
  VPro: VPro V12 needs Crossbow 1.4, the ER power supply 060-0035-00x and PROM
    4.5 (007-4210-003), and this Octane presents Crossbow revision code 4 and
    the SR supply
"
        );
        // An Origin 200 takes a processor module and is headless by default.
        let origin200 = detail(&machines, "origin200-1").unwrap();
        assert!(origin200.contains("  none (default)\n"), "{origin200}");
        assert!(
            origin200.contains("Processor modules (create --processor ID): r10000-180mhz-1mb"),
            "{origin200}"
        );
        // `--all` is every detail in table order.
        let all = all(&machines).unwrap();
        let first = all.find("origin200-1: ").unwrap();
        let last = all.find("fuel-1: ").unwrap();
        assert!(first < last && all.contains(&origin200), "{all}");
    }

    #[test]
    fn settings_list_their_values_with_the_default_starred() {
        let product: Product = serde_json::from_str(
            r#"{"product":"p","name":"P","description":"","fidelity":"","settings":[
              {"name":"rm-count","description":"","default":"2","changeable":true,
               "values":[{"value":"1","description":""},{"value":"2","description":""}]},
              {"name":"fixed","description":"","default":"a","changeable":false,
               "values":[{"value":"a","description":""}]}]}"#,
        )
        .unwrap();
        let record = crate::test_machines().topologies[0]
            .graphics
            .clone()
            .unwrap();
        assert_eq!(settings(&record, &product), "rm-count=1|2*");
    }
}
