//! The same server from user-mode networking, through the product's NAT.
#![cfg(target_os = "linux")]

use std::path::Path;

use serde_json::{json, Value};

use super::{check, nfs::cksum_fields, serial};
use crate::lab::linux::{LinuxLab, PEER};
use crate::support::guest::Guest;
use crate::support::product::Product;
use crate::support::{disk, pause, posix_cksum, random_bytes, secs, tail, Result};

const MAC: &str = "08:00:69:e1:03:20";

fn mounted_roundtrip(g: &mut Guest, export: &Path) -> Result<Value> {
    let data = random_bytes(1 << 20);
    std::fs::write(export.join("data.bin"), &data)?;
    let got = cksum_fields(&g.must("cksum /mnt/x/data.bin", secs(300))?);
    let (rc, _) = g.sh("dd if=/mnt/x/data.bin of=/mnt/x/copy.bin bs=8k", secs(300))?;
    g.sh("sync", secs(120))?;
    let copied = std::fs::read(export.join("copy.bin")).ok();
    Ok(json!({
        "read_ok": got == posix_cksum(&data),
        "write_ok": rc == 0 && copied.as_deref() == Some(&data[..]),
    }))
}

#[test]
#[ignore = "boots IRIX on user networking against a real peer; needs ORIGAMI_PRODUCT, ORIGAMI_WORK, root for ip"]
fn nat_user() {
    let _one_at_a_time = serial();
    let product = Product::from_env("nat-user");
    let machine = disk::prepared_machine(&disk::cache_dir()).expect("prepared disk");
    // No taps: the host holds the bridge address and NAT leaves from it.
    let lab = LinuxLab::start(0, &product.work).expect("lab");
    let mut g = Guest::create(&product, &machine, "natguest", MAC).expect("guest");
    let result = (|| -> Result<Value> {
        g.network(&["--mode", "user"])?;
        g.start()?;
        let address = g.address("10.98.0.", secs(300))?;
        let (ping_rc, ping) = g.sh(&format!("ping -c 3 {PEER}"), secs(120))?;
        let mut results = json!({"address": address, "ping_rc": ping_rc, "ping": tail(&ping, 300)});
        for (label, export) in [
            ("secure_export", &lab.export),
            ("insecure_export", &lab.export_open),
        ] {
            let dump = lab.capture(
                &product.evidence().join(format!("nat-{label}-packets.txt")),
                80,
            );
            let (rc, out) = g.sh(
                &format!("mkdir -p /mnt/x; mount {PEER}:{} /mnt/x", export.display()),
                secs(240),
            )?;
            pause(secs(2));
            drop(dump);
            let mut entry = json!({"mount_rc": rc, "output": tail(&out, 400)});
            if rc == 0 {
                let round = mounted_roundtrip(&mut g, export);
                let _ = g.sh("umount /mnt/x", secs(120));
                let round = round?;
                entry["read_ok"] = round["read_ok"].clone();
                entry["write_ok"] = round["write_ok"].clone();
            }
            results[label] = entry;
        }
        if ping_rc != 0 {
            return Err(format!("ping through NAT failed: {ping}").into());
        }
        let open = &results["insecure_export"];
        if open["mount_rc"] != 0 || open["read_ok"] != true || open["write_ok"] != true {
            return Err(format!(
                "NFS from the export that takes unprivileged ports failed: {open}"
            )
            .into());
        }
        // A server that insists on a reserved client port refuses NAT'd
        // requests, whose source port is the host's. Record which it was.
        let secure = &results["secure_export"];
        if secure["mount_rc"] != 0 {
            if !secure["output"]
                .as_str()
                .unwrap_or("")
                .contains("Permission denied")
            {
                return Err(format!(
                    "the secure export failed, but not by refusing the port: {secure}"
                )
                .into());
            }
            results["nat_limit"] = json!(
                "the peer refuses mounts from unprivileged source ports, and NAT gives every guest packet one"
            );
        }
        Ok(results)
    })();
    check(&product, "nat-user", result);
}
