//! The serial lines a machine's console can use.

use crate::{Offering, Result};

/// The serial lines a console can use, as (line index, name, whether it is
/// the primary console). The machine list gives the lines in launch order,
/// node 0 first; lines of other nodes are not offered.
fn console_ports(offering: &Offering) -> impl Iterator<Item = (usize, &str, bool)> {
    offering
        .consoles
        .iter()
        .enumerate()
        .take_while(|(_, line)| line.node == 0)
        .filter(|(_, line)| matches!(line.kind.as_str(), "console" | "second-port"))
        .filter_map(|(index, line)| {
            let name = line.name.as_deref()?;
            Some((index, name, line.kind == "console"))
        })
}

/// A serial line a console can use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsolePort {
    pub name: String,
    /// Whether it is the machine's default console.
    pub primary: bool,
}

/// The lines a console choice can name, in launch order.
pub fn console_choices(offering: &Offering) -> Vec<ConsolePort> {
    console_ports(offering)
        .map(|(_, name, primary)| ConsolePort {
            name: name.into(),
            primary,
        })
        .collect()
}

/// The machine's default console: the machine list's line of kind console.
pub fn default_console(offering: &Offering) -> Result<&str> {
    console_ports(offering)
        .find(|(_, _, primary)| *primary)
        .map(|(_, name, _)| name)
        .ok_or_else(|| format!("{} lists no node 0 console", offering.topology).into())
}

/// The line index of a console port, by default the machine list's.
pub fn console_line(offering: &Offering, port: Option<&str>) -> Result<usize> {
    let port = match port {
        Some(port) => port,
        None => default_console(offering)?,
    };
    console_ports(offering)
        .find(|(_, name, _)| *name == port)
        .map(|(index, _, _)| index)
        .ok_or_else(|| {
            let names: Vec<_> = console_ports(offering).map(|(_, name, _)| name).collect();
            format!(
                "{} has no {port} console; choose {}",
                offering.topology,
                names.join(" or ")
            )
            .into()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{preset, test_machines, Console};

    #[test]
    fn console_line_follows_the_machine_list_and_choice() {
        let machines = test_machines();
        // Fuel's machine list entry gives l1 (its console), then IOC3 ports A and B.
        let fuel = preset(&machines, "fuel-1").unwrap();
        assert_eq!(default_console(fuel).unwrap(), "l1");
        assert_eq!(
            console_ports(fuel).collect::<Vec<_>>(),
            [(0, "l1", true), (1, "ioc3_a", false), (2, "ioc3_b", false)]
        );
        assert_eq!(console_line(fuel, Some("ioc3_a")).unwrap(), 1);

        // Only node 0 lines of console kinds are offered.
        let mut origin2000 = preset(&machines, "origin2000-8").unwrap().clone();
        origin2000.consoles.push(Console {
            kind: "second-port".into(),
            node: 1,
            name: Some("node1_ioc3_a".into()),
        });
        assert_eq!(
            console_ports(&origin2000).collect::<Vec<_>>(),
            [(0, "ioc3_a", true), (1, "ioc3_b", false)]
        );
        let error = console_line(&origin2000, Some("elsc"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("ioc3_a or ioc3_b"), "{error}");
    }
}
