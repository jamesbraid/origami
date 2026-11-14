//! What a Windows runner can check about networking. It has no bridge, no
//! network namespaces and no dnsmasq or unfs3, so peers cannot run there:
//! the guest gets user-mode networking and nothing else. A separate,
//! informational probe asks whether a TAP-Windows6 adapter works at all.

use std::time::Instant;

use serde_json::json;

use super::{check, serial};
use crate::support::guest::Guest;
use crate::support::product::Product;
use crate::support::{disk, pause, secs, Result};

const MAC: &str = "08:00:69:e1:03:50";
const GATEWAY: &str = "10.98.0.2";

/// User-mode networking, which every OS has: the guest takes the product's
/// DHCP lease and reaches the product's gateway.
#[test]
#[ignore = "boots IRIX on user networking; needs ORIGAMI_PRODUCT and ORIGAMI_WORK"]
fn user_mode() {
    let _one_at_a_time = serial();
    let product = Product::from_env("user-mode");
    let machine = disk::prepared_machine(&disk::cache_dir()).expect("prepared disk");
    let mut g = Guest::create(&product, &machine, "usermode", MAC).expect("guest");
    let result = (|| {
        g.network(&["--mode", "user"])?;
        g.start()?;
        let address = g.address("10.98.0.", secs(300))?;
        g.must(&format!("ping -c 3 {GATEWAY}"), secs(120))?;
        Ok(json!({"address": address}))
    })();
    check(&product, "user-mode", result);
}

/// Informational: is a TAP-Windows6 adapter usable on a hosted runner? The
/// job installs the driver and names the adapter in `ORIGAMI_TAP_ADAPTER`.
/// The test points a diskless machine at it and asks whether QEMU opens the
/// device and the PROM starts, which is all a tap netdev's start needs. No
/// peer answers on the other side.
#[test]
#[ignore = "probes a TAP-Windows6 adapter; needs ORIGAMI_PRODUCT, ORIGAMI_WORK and ORIGAMI_TAP_ADAPTER"]
fn tap_windows_probe() {
    let _one_at_a_time = serial();
    let product = Product::from_env("tap-windows-probe");
    let adapter =
        std::env::var("ORIGAMI_TAP_ADAPTER").expect("ORIGAMI_TAP_ADAPTER names the TAP adapter");
    let machine = product.machines().join("probe");
    let started = Instant::now();
    let probe = || -> Result<serde_json::Value> {
        product.origami_ok([
            "create".as_ref(),
            machine.as_os_str(),
            "--preset".as_ref(),
            "origin200-1".as_ref(),
        ])?;
        product.origami_ok([
            "network-set".as_ref(),
            machine.as_os_str(),
            "--mode".as_ref(),
            "tap".as_ref(),
            "--interface".as_ref(),
            adapter.as_ref(),
        ])?;
        let run = product.origami([
            "run".as_ref(),
            machine.as_os_str(),
            "--display".as_ref(),
            "none".as_ref(),
            "--background".as_ref(),
        ])?;
        if run.code != 0 {
            return Err(format!("QEMU would not start on the adapter: {}", run.text()).into());
        }
        // A tap netdev that cannot open its device ends QEMU at once.
        pause(secs(20));
        if product.stopped(&machine) {
            return Err("QEMU exited within 20 seconds of starting on the adapter".into());
        }
        Ok(json!({"adapter": adapter, "seconds": started.elapsed().as_secs()}))
    };
    let result = probe();
    let _ = product.origami(["stop".as_ref(), machine.as_os_str()]);
    check(&product, "tap-windows-probe", result);
}
