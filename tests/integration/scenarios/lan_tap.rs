//! A guest on a bridge: DHCP from the peer, ping, NFS mount, checksums.
#![cfg(target_os = "linux")]

use serde_json::json;

use super::{check, nfs, serial};
use crate::lab::linux::{LinuxLab, PEER, PREFIX};
use crate::support::guest::Guest;
use crate::support::product::Product;
use crate::support::{disk, secs};

const MAC: &str = "08:00:69:e1:03:10";

#[test]
#[ignore = "boots IRIX on a real bridge; needs ORIGAMI_PRODUCT, ORIGAMI_WORK, root for ip"]
fn lan_tap() {
    let _one_at_a_time = serial();
    let product = Product::from_env("lan-tap");
    let machine = disk::prepared_machine(&disk::cache_dir()).expect("prepared disk");
    let lab = LinuxLab::start(1, &product.work).expect("lab");
    let mut g = Guest::create(&product, &machine, "tapguest", MAC).expect("guest");
    let result = (|| {
        g.network(&["--mode", "tap", "--interface", &lab.taps[0], "--mac", MAC])?;
        g.start()?;
        let address = g.address(PREFIX, secs(300))?;
        let leases = lab.leases();
        if !leases.contains(&address) {
            return Err(
                format!("the peer's DHCP server never leased {address}: {leases:?}").into(),
            );
        }
        g.must(&format!("ping -c 3 {PEER}"), secs(120))?;
        let dump = lab.capture(&product.evidence().join("tap-mount-packets.txt"), 80);
        let mut data = nfs::roundtrip(&mut g, PEER, &lab.export)?;
        drop(dump);
        data["address"] = json!(address);
        Ok(data)
    })();
    check(&product, "lan-tap", result);
}
