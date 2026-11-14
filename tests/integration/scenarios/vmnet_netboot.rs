//! An Octane2 PROM netboots over socket_vmnet: its BOOTP request reaches a
//! real dnsmasq on the vmnet bridge, the PROM reports the address dnsmasq
//! gave it, and dnsmasq sends the boot file back to that address.
#![cfg(target_os = "macos")]

use std::path::Path;
use std::time::Instant;

use regex::Regex;
use serde_json::{json, Value};

use super::{check, serial};
use crate::lab::vmnet::{VmnetLab, POOL_PREFIX};
use crate::support::console::Console;
use crate::support::guest::mac_of;
use crate::support::product::Product;
use crate::support::{pause, secs, tail, Result};

/// What the logs fail to show; an empty list means the guest used vmnet.
fn evidence_failures(
    mac: &str,
    console: &str,
    dnsmasq: &str,
    capture: &str,
) -> Result<Vec<String>> {
    let pool = format!("{}1[01]\\d", regex::escape(POOL_PREFIX));
    let leased = Regex::new(&format!(r"Setting \$netaddr to ({pool})\b"))?;
    let Some(address) = leased.captures(console).map(|c| c[1].to_string()) else {
        return Ok(vec![
            "the PROM never reported an address from the server".into()
        ]);
    };
    let (mac_rx, addr_rx) = (regex::escape(mac), regex::escape(&address));
    let mut failures = Vec::new();
    if !console.contains("Obtaining /unix from server") {
        failures.push("the PROM did not start the TFTP download".to_string());
    }
    if !Regex::new(&format!(r"BOOTP\(\S+\) {addr_rx} {mac_rx}"))?.is_match(dnsmasq) {
        failures.push(format!(
            "dnsmasq did not log answering {mac} with {address}"
        ));
    }
    if !Regex::new(&format!(
        r"(?m)dnsmasq-tftp\[\d+\]: sent \S.*/unix to {addr_rx}$"
    ))?
    .is_match(dnsmasq)
    {
        failures.push(format!(
            "dnsmasq did not log sending the boot file to {address}"
        ));
    }
    if !capture.contains(&format!("Request from {mac}")) {
        failures.push("tcpdump on the vmnet bridge saw no request from the guest".into());
    }
    if !Regex::new(&format!(r"Reply.*\n(?:.*\n)*?\s+Your-IP {addr_rx}\n"))?.is_match(capture) {
        failures.push("tcpdump on the vmnet bridge saw no reply carrying the address".into());
    }
    Ok(failures)
}

fn wait_stopped(product: &Product, machine: &Path) {
    let end = Instant::now() + secs(60);
    while Instant::now() < end && !product.stopped(machine) {
        pause(secs(1));
    }
}

fn body(product: &Product, machine: &Path, lab: &VmnetLab) -> Result<Value> {
    let run = |log: &str| -> Result<Console> {
        product.origami_ok([
            "run".as_ref(),
            machine.as_os_str(),
            "--display".as_ref(),
            "none".as_ref(),
            "--background".as_ref(),
        ])?;
        Console::connect(machine, Some(&product.evidence().join(log)))
    };

    // A first start makes the PROM write its default environment, which
    // `nvram` can then change; the serial console is off until then.
    let mut con = run("first-start-console.log")?;
    let seen = con.expect("Re-Initializing PDS", secs(120)).is_ok();
    println!("first start reached the PROM: {seen}");
    pause(secs(5));
    con.close();
    product.origami_ok(["stop".as_ref(), machine.as_os_str()])?;
    wait_stopped(product, machine);
    product.origami_ok([
        "nvram".as_ref(),
        machine.as_os_str(),
        "set".as_ref(),
        "console=d".as_ref(),
    ])?;

    let console_log = product.evidence().join("netboot-console.log");
    let mut con = run("netboot-console.log")?;
    // With no disk the power-on diagnostics fail and wait for a key.
    if con.expect("Press any key to continue", secs(240)).is_ok() {
        con.send(" ")?;
    }
    con.expect("Command Monitor", secs(120))
        .map_err(|_| "the PROM never offered its System Maintenance Menu")?;
    con.send("5\r")?;
    let _ = con.expect(">> ", secs(30));
    // BOOTP replies go to the address the PROM already holds, so give it a
    // placeholder on this network; the server's address replaces it.
    con.send(&format!("setenv netaddr {POOL_PREFIX}77\r"))?;
    pause(secs(2));
    con.send(&format!("boot -f bootp(){}:/unix\r", lab.gateway))?;
    // The PROM cannot run the file; it gives up after reading it.
    let _ = con.expect("(?i)(unable to load|no server)", secs(90));
    let _ = con.drain(secs(3));
    con.close();

    let mac = mac_of(machine)?;
    let text =
        String::from_utf8_lossy(&std::fs::read(&console_log).unwrap_or_default()).replace('\r', "");
    pause(secs(1));
    let failures = evidence_failures(&mac, &text, &lab.dnsmasq_log(), &lab.capture())?;
    println!("{}", tail(&text, 3000));
    if failures.is_empty() {
        Ok(
            json!({"mac": mac, "result": "the guest's BOOTP request crossed vmnet and its TFTP download came back"}),
        )
    } else {
        Err(failures.join("; ").into())
    }
}

#[test]
#[ignore = "netboots an Octane2 PROM over socket_vmnet; needs ORIGAMI_PRODUCT, ORIGAMI_WORK, ORIGAMI_VMNET_SOCKET and sudo"]
fn vmnet_netboot() {
    let _one_at_a_time = serial();
    let product = Product::from_env("vmnet-netboot");
    let machine = product.machines().join("octane2");
    product
        .origami_ok([
            "create".as_ref(),
            machine.as_os_str(),
            "--preset".as_ref(),
            "octane2-vpro".as_ref(),
        ])
        .expect("create");
    let lab = VmnetLab::start(&product.work).expect("lab");
    let socket = lab.socket.to_string_lossy().into_owned();
    product
        .origami_ok([
            "network-set".as_ref(),
            machine.as_os_str(),
            "--mode".as_ref(),
            "vmnet".as_ref(),
            "--endpoint".as_ref(),
            socket.as_ref(),
        ])
        .expect("network-set");
    let guard = crate::support::guest::Guest::adopt(&product, "octane2", "", machine.clone());
    let result = body(&product, &machine, &lab);
    let _guard = guard;
    check(&product, "vmnet-netboot", result);
}
