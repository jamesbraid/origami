//! The graphics QEMU is given: the `-M` properties a pipe needs and one
//! `-device` for each pipe.

use origami_machine::graphics::{record, Pipe, Property};
use origami_machine::{Machines, Result};

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
        // Every pipe is named: the display arguments and screendump pick a
        // head by device id.
        device.push_str(&format!(",id={}", pipe.slot));
        args.extend(["-device".into(), device]);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_machines;
    use origami_machine::graphics::{select, Request};

    fn request<'a>(product: Option<&'a str>, slot: Option<&'a str>) -> Request<'a> {
        Request {
            product,
            slot,
            settings: Default::default(),
            pipes: None,
        }
    }

    #[test]
    fn each_pipe_is_a_device_and_no_pipe_may_turn_off_the_default_vga() {
        let machines = test_machines();
        let select =
            |topology: &str, nodes, r: Request<'_>| select(&machines, topology, nodes, &r).unwrap();
        let args = |topology: &str, pipes: &[Pipe]| arguments(&machines, topology, pipes).unwrap();

        let pipes = select("onyx2-deskside", 2, request(None, None));
        assert_eq!(
            args("onyx2-deskside", &pipes),
            [
                "-device",
                "sgi-kona,slot=io4,product=ir2,geometry=ge14-4,rm-type=rm7,rm-count=2,\
                 texture-mb=64,display=dg5-8,id=io4"
            ]
        );
        let pipes = select("octane", 1, request(Some("none"), None));
        assert_eq!(args("octane", &pipes), ["-vga", "none"]);
        assert!(args("origin200", &[]).is_empty());
        let pipes = select("origin200", 1, request(Some("rad4"), None));
        assert_eq!(
            args("origin200", &pipes),
            [
                "-device",
                "psitech-rad4,bus=pci,addr=5,product=rad4,id=pci-5"
            ]
        );
        let pipes = select("origin2000-rack", 4, request(Some("si"), Some("io3")));
        assert_eq!(
            args("origin2000-rack", &pipes),
            ["-device", "sgi-mgras,slot=io3,product=si,id=io3"]
        );
    }

    #[test]
    fn a_pipe_in_a_several_pipe_machine_is_named_for_the_display_options() {
        let machines = test_machines();
        let request = Request {
            pipes: Some(4),
            ..Default::default()
        };
        let pipes = select(&machines, "onyx3400-ir", 4, &request).unwrap();
        let args = arguments(&machines, "onyx3400-ir", &pipes).unwrap();
        assert_eq!(args.len(), 8);
        assert!(args[7].starts_with("sgi-kona,slot=gbrick1-pipe1-pipe,product=ir3,"));
    }
}
