//! An IRIX guest on socket_vmnet: DHCP from a real dnsmasq on the vmnet
//! bridge, and ping of the host.
#![cfg(target_os = "macos")]

use serde_json::json;

use super::{check, serial};
use crate::lab::vmnet::{VmnetLab, POOL_PREFIX};
use crate::support::guest::Guest;
use crate::support::product::Product;
use crate::support::{disk, secs};

const MAC: &str = "08:00:69:e1:03:40";

#[test]
#[ignore = "boots IRIX on socket_vmnet; needs ORIGAMI_PRODUCT, ORIGAMI_WORK, ORIGAMI_VMNET_SOCKET and sudo"]
fn vmnet_lan() {
    let _one_at_a_time = serial();
    let product = Product::from_env("vmnet-lan");
    let machine = disk::prepared_machine(&disk::cache_dir()).expect("prepared disk");
    let lab = VmnetLab::start(&product.work).expect("lab");
    let mut g = Guest::create(&product, &machine, "vmnetguest", MAC).expect("guest");
    let result = (|| {
        let socket = lab.socket.to_string_lossy().into_owned();
        g.network(&["--mode", "vmnet", "--endpoint", &socket, "--mac", MAC])?;
        g.start()?;
        let address = g.address(POOL_PREFIX, secs(300))?;
        let leases = lab.leases();
        if !leases.contains(&address) {
            return Err(format!("dnsmasq never leased {address}: {leases:?}").into());
        }
        g.must(&format!("ping -c 3 {}", lab.gateway), secs(120))?;
        Ok(json!({"address": address, "leases": leases.trim()}))
    })();
    check(&product, "vmnet-lan", result);
}
