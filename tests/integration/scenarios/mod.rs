//! One module per scenario. Each test owns its lab and guests, so that a
//! failure unwinds through their `Drop`s: the guests' failure capture runs
//! first, then the guests stop, then the lab goes.

pub mod lab_probe;
pub mod lan_tap;
pub mod nat_user;
pub mod two_guests;
pub mod vmnet_lan;
pub mod vmnet_netboot;
pub mod windows;

use std::sync::{Mutex, MutexGuard};

use serde_json::Value;

use crate::support::product::Product;
use crate::support::Result;

static SERIAL: Mutex<()> = Mutex::new(());

/// Scenarios share a fixed subnet and the host's memory, so one runs at a
/// time even if the harness is given more threads.
pub fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write what a scenario measured next to its logs, and fail the test, by
/// panicking in the caller's scope, when it did not pass. Panicking there
/// is what lets the guests' failure capture see the machines still running.
pub fn check(product: &Product, name: &str, result: Result<Value>) {
    match result {
        Ok(data) => {
            let doc = serde_json::json!({"name": name, "status": "pass", "data": data});
            let _ = std::fs::write(product.evidence().join("result.json"), doc.to_string());
        }
        Err(e) => {
            let doc = serde_json::json!({"name": name, "status": "fail", "error": e.to_string()});
            let _ = std::fs::write(product.evidence().join("result.json"), doc.to_string());
            panic!("{name}: {e}");
        }
    }
}

#[cfg(target_os = "linux")]
pub mod nfs {
    use std::path::Path;

    use serde_json::{json, Value};

    use crate::support::guest::Guest;
    use crate::support::{posix_cksum, random_bytes, secs, Result};

    /// The first two fields of `cksum`'s output: checksum and length.
    pub fn cksum_fields(output: &str) -> String {
        output
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Mount `server:export` on the guest, read a peer file and write files
    /// back, checking checksums on both sides. `export` is also the peer's
    /// local path to the same directory.
    pub fn roundtrip(g: &mut Guest, server: &str, export: &Path) -> Result<Value> {
        let data = random_bytes(1 << 20);
        std::fs::write(export.join("data.bin"), &data)?;
        let want = posix_cksum(&data);
        g.must("mkdir -p /mnt/x", secs(120))?;
        g.must(
            &format!("mount {server}:{} /mnt/x", export.display()),
            secs(180),
        )?;
        let got = cksum_fields(&g.must("cksum /mnt/x/data.bin", secs(120))?);
        if got != want {
            return Err(format!("read: guest {got} peer {want}").into());
        }
        g.must("dd if=/mnt/x/data.bin of=/mnt/x/copy.bin bs=8k", secs(300))?;
        g.must("sync", secs(120))?;
        if std::fs::read(export.join("copy.bin"))? != data {
            return Err("the peer's copy of the written file differs".into());
        }
        // A file made on the guest.
        g.must("dd if=/unix bs=32k count=64 of=/mnt/x/guest.bin", secs(300))?;
        g.must("sync", secs(120))?;
        let local =
            cksum_fields(&g.must("dd if=/unix bs=32k count=64 2>/dev/null | cksum", secs(120))?);
        let peer = posix_cksum(&std::fs::read(export.join("guest.bin"))?);
        if local != peer {
            return Err(format!("write: guest {local} peer {peer}").into());
        }
        g.must("umount /mnt/x", secs(120))?;
        Ok(json!({"read_checksum": want, "write_checksum": peer}))
    }
}
