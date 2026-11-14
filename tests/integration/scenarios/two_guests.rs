//! Two guests on one bridge: distinct identities, a shared export, and
//! guest-to-guest NFS.
#![cfg(target_os = "linux")]

use serde_json::{json, Value};

use super::{check, nfs::cksum_fields, serial};
use crate::lab::linux::{LinuxLab, PEER, PREFIX};
use crate::support::guest::Guest;
use crate::support::product::Product;
use crate::support::{disk, pause, posix_cksum, random_bytes, secs, Result};

const MAC_A: &str = "08:00:69:e1:03:31";
const MAC_B: &str = "08:00:69:e1:03:32";

fn body(
    product: &Product,
    machine: &std::path::Path,
    lab: &LinuxLab,
    a: &mut Guest,
    b: &mut Guest,
) -> Result<Value> {
    let mut results = json!({});
    a.network(&["--mode", "tap", "--interface", &lab.taps[0], "--mac", MAC_A])?;
    b.network(&["--mode", "tap", "--interface", &lab.taps[1], "--mac", MAC_B])?;

    // A new machine may not take a MAC a sibling already uses.
    let dup_dir = product.machines().join("dupmac");
    let dup = product.origami([
        "create".as_ref(),
        dup_dir.as_os_str(),
        "--preset".as_ref(),
        "origin200-1".as_ref(),
        "--mac".as_ref(),
        MAC_A.as_ref(),
    ])?;
    if dup.code == 0 || !dup.stderr.contains("collides") {
        return Err(format!(
            "creating a machine with a used MAC was not refused: {}",
            dup.text()
        )
        .into());
    }
    results["duplicate_mac_refused"] = json!(dup.stderr.trim());

    // A copied directory is a clone with the same MAC. Record whether
    // validate, which `run` also performs, notices.
    {
        let clone = Guest::create(product, machine, "clone", MAC_A)?;
        let n = product.origami([
            "network-set".as_ref(),
            clone.dir.as_os_str(),
            "--mode".as_ref(),
            "tap".as_ref(),
            "--interface".as_ref(),
            lab.taps[1].as_ref(),
        ])?;
        let v = product.origami(["validate".as_ref(), clone.dir.as_os_str()])?;
        results["clone_with_same_mac"] = json!({
            "network_set": [n.code, n.text()],
            "validate": [v.code, v.text()],
        });
        let dir = clone.dir.clone();
        drop(clone);
        std::fs::remove_dir_all(dir)?;
    }

    a.start()?;
    b.start()?;
    let (addr_a, addr_b) = (a.address(PREFIX, secs(300))?, b.address(PREFIX, secs(300))?);
    if addr_a == addr_b {
        return Err(format!("both guests got {addr_a}").into());
    }
    results["addresses"] = json!([addr_a, addr_b]);
    let leases = lab.leases();
    for (g, addr) in [(&*a, &addr_a), (&*b, &addr_b)] {
        if !leases.contains(&format!("{} {addr} ", g.mac)) {
            return Err(format!("no lease of {addr} to {}: {leases:?}", g.mac).into());
        }
    }
    results["leases"] = json!(leases.trim().lines().collect::<Vec<_>>());
    for g in [&mut *a, &mut *b] {
        g.must(&format!("ping -c 2 {PEER}"), secs(120))?;
    }
    a.must(&format!("ping -c 2 {addr_b}"), secs(120))?;
    for g in [&mut *a, &mut *b] {
        g.must("mkdir -p /mnt/x", secs(120))?;
        g.must(
            &format!("mount {PEER}:{} /mnt/x", lab.export.display()),
            secs(180),
        )?;
    }
    let data = random_bytes(256 << 10);
    std::fs::write(lab.export.join("shared.bin"), &data)?;
    for g in [&mut *a, &mut *b] {
        let got = cksum_fields(&g.must("cksum /mnt/x/shared.bin", secs(120))?);
        if got != posix_cksum(&data) {
            return Err(
                format!("{} read {got}, the peer has {}", g.name, posix_cksum(&data)).into(),
            );
        }
    }

    // Guest to guest: A exports a directory, B mounts it.
    a.must(
        "mkdir -p /export/a && echo 'from guest a' > /export/a/hello.txt && chmod 777 /export/a",
        secs(120),
    )?;
    a.must("echo '/export/a -rw' >> /etc/exports", secs(120))?;
    a.must(
        "chkconfig nfs on; /etc/init.d/nfs start; exportfs -a; exportfs",
        secs(300),
    )?;
    pause(secs(15));
    let out = b.must(
        &format!("mkdir -p /mnt/a && mount {addr_a}:/export/a /mnt/a && cat /mnt/a/hello.txt"),
        secs(300),
    )?;
    if !out.contains("from guest a") {
        return Err(format!("guest B read {out:?} from guest A's export").into());
    }
    b.must("echo 'from guest b' > /mnt/a/reply.txt; sync", secs(120))?;
    if !a
        .must("cat /export/a/reply.txt", secs(120))?
        .contains("from guest b")
    {
        return Err("guest A does not see guest B's file".into());
    }
    results["guest_to_guest_nfs"] = json!("ok");
    Ok(results)
}

#[test]
#[ignore = "boots two IRIX guests on a real bridge; needs ORIGAMI_PRODUCT, ORIGAMI_WORK, root for ip"]
fn two_guests() {
    let _one_at_a_time = serial();
    let product = Product::from_env("two-guests");
    let machine = disk::prepared_machine(&disk::cache_dir()).expect("prepared disk");
    let lab = LinuxLab::start(2, &product.work).expect("lab");
    let mut a = Guest::create(&product, &machine, "guesta", MAC_A).expect("guest a");
    let mut b = Guest::create(&product, &machine, "guestb", MAC_B).expect("guest b");
    let result = body(&product, &machine, &lab, &mut a, &mut b);
    check(&product, "two-guests", result);
}
