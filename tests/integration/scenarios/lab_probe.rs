//! The Linux lab on its own, with no guest: do the bridge, the namespace and
//! the peer services come up, and does the peer answer across the bridge?
//! Quick, so a job runs it first and a missing tool or privilege shows up
//! in seconds rather than after a guest boots.
#![cfg(target_os = "linux")]

use super::serial;
use crate::lab::linux::LinuxLab;
use crate::support::product::Product;

#[test]
#[ignore = "builds the network lab; needs ORIGAMI_PRODUCT, ORIGAMI_WORK, root for ip"]
fn lab_probe() {
    let _one_at_a_time = serial();
    let product = Product::from_env("lab-probe");
    let lab = LinuxLab::start(2, &product.work).expect("lab");
    lab.peer_answers()
        .expect("the peer answers on the portmap and NFS ports");
    assert_eq!(lab.taps.len(), 2);
}
