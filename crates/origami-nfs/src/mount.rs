//! MOUNT versions 1 and 3 (RFC 1094 appendix A, RFC 1813 appendix I).

use std::net::SocketAddr;

use crate::nfs2::{ftype, status_for, NFS_OK, NF_DIR};
use crate::rpc::{LogLevel, Server};
use crate::xdr::{Reader, Writer};
use crate::Error;

impl Server {
    pub(crate) fn mount_call(
        &self,
        caller: SocketAddr,
        vers: u32,
        proc_: u32,
        r: &mut Reader,
    ) -> Option<Vec<u8>> {
        match proc_ {
            0 => Some(Vec::new()),
            1 => Some(self.mount_mnt(caller, r, vers)),
            3 => {
                // UMNT: the server keeps no mount list.
                r.str();
                Some(Vec::new())
            }
            4 => Some(Vec::new()), // UMNTALL
            5 => Some(self.mount_export()),
            _ => None,
        }
    }

    /// Answers a mount path with its root handle. Version 1 replies with
    /// NFSv2 status values, version 3 with mountstat3; the two agree on
    /// every status sent here.
    fn mount_mnt(&self, caller: SocketAddr, r: &mut Reader, vers: u32) -> Vec<u8> {
        let path = r.str();
        let mut w = Writer::new();
        if r.err {
            return Vec::new();
        }
        let root = match self.fs.mount_root(path) {
            Ok(n) if ftype(n.mode()) != NF_DIR => Err(Error::NotDir),
            other => other,
        };
        let root = match root {
            Ok(n) => n,
            Err(e) => {
                self.log(LogLevel::Warn, || {
                    format!(
                        "nfs: mount {:?} from {caller}: {e}",
                        String::from_utf8_lossy(path)
                    )
                });
                w.u32(status_for(Some(e)));
                return w.b;
            }
        };
        self.log(LogLevel::Info, || {
            format!(
                "nfs: mount {:?} v{vers} from {caller}",
                String::from_utf8_lossy(path)
            )
        });
        w.u32(NFS_OK);
        if vers == 1 {
            w.fixed(&self.handle(&root));
            return w.b;
        }
        w.opaque(&self.handle(&root));
        w.u32(1); // one auth flavor:
        w.u32(1); // AUTH_UNIX
        w.b
    }

    /// Lists the exports, each open to every client: an empty group list.
    fn mount_export(&self) -> Vec<u8> {
        let mut w = Writer::new();
        for p in self.fs.exports() {
            w.bool(true);
            w.opaque(&p);
            w.bool(false); // no groups
        }
        w.bool(false);
        w.b
    }
}
