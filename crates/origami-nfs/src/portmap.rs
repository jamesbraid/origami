//! Portmap version 2 (RFC 1833 section 3): NULL, GETPORT and DUMP.

use crate::rpc::{Server, PROG_MOUNT, PROG_NFS, PROG_PORTMAP};
use crate::xdr::{Reader, Writer};

// Transport numbers.
const IPPROTO_TCP: u32 = 6;
const IPPROTO_UDP: u32 = 17;

struct Mapping {
    prog: u32,
    vers: u32,
    prot: u32,
    port: u32,
}

impl Server {
    pub(crate) fn portmap_call(&self, proc_: u32, r: &mut Reader) -> Option<Vec<u8>> {
        match proc_ {
            0 => Some(Vec::new()),
            3 => Some(self.pmap_getport(r)),
            4 => Some(self.pmap_dump()),
            _ => None,
        }
    }

    /// Every registration, in the order DUMP reports them.
    fn mappings(&self) -> Vec<Mapping> {
        let Some(p) = self.ports() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (i, prot) in [IPPROTO_UDP, IPPROTO_TCP].into_iter().enumerate() {
            let ports = p[i];
            for (prog, vers, port) in [
                (PROG_PORTMAP, 2, ports.portmap),
                (PROG_MOUNT, 1, ports.mount),
                (PROG_MOUNT, 3, ports.mount),
                (PROG_NFS, 2, ports.nfs),
                (PROG_NFS, 3, ports.nfs),
            ] {
                if port != 0 {
                    out.push(Mapping {
                        prog,
                        vers,
                        prot,
                        port,
                    });
                }
            }
        }
        out
    }

    /// Answers the port for a program, version and transport, or 0 when
    /// nothing is registered for it.
    fn pmap_getport(&self, r: &mut Reader) -> Vec<u8> {
        let prog = r.u32();
        let vers = r.u32();
        let prot = r.u32();
        r.u32(); // port, ignored
        let port = self
            .mappings()
            .into_iter()
            .find(|m| m.prog == prog && m.vers == vers && m.prot == prot)
            .map_or(0, |m| m.port);
        let mut w = Writer::new();
        w.u32(port);
        w.b
    }

    fn pmap_dump(&self) -> Vec<u8> {
        let mut w = Writer::new();
        for m in self.mappings() {
            w.bool(true);
            w.u32(m.prog);
            w.u32(m.vers);
            w.u32(m.prot);
            w.u32(m.port);
        }
        w.bool(false);
        w.b
    }
}
