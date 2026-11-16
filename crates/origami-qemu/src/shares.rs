//! The QEMU options that put a machine's network and NIC on either side of
//! the share switch.

use origami_machine::shares::{Listen, Ports};

fn addr_options(listen: &Listen) -> String {
    match listen {
        Listen::Tcp(port) => format!("addr.type=inet,addr.host=127.0.0.1,addr.port={port}"),
        Listen::Unix(path) => {
            format!("addr.type=unix,addr.path={}", crate::qemu_path_option(path))
        }
    }
}

/// The QEMU options that put the machine's network (`net0`) and the guest
/// NIC (`share-guest`) on either side of the switch.
pub fn netdev_arguments(ports: &Ports) -> Vec<String> {
    let stream = |id: &str, addr: &Listen| {
        format!(
            "stream,id={id},{},server=off,reconnect-ms=1000",
            addr_options(addr)
        )
    };
    [
        "hubport,id=share-hub-net,hubid=1,netdev=net0".to_string(),
        stream("share-net", &ports.network),
        "hubport,id=share-hub-link,hubid=1,netdev=share-net".to_string(),
        stream("share-guest", &ports.guest),
    ]
    .into_iter()
    .flat_map(|option| ["-netdev".to_string(), option])
    .collect()
}
