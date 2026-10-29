//! Host folders shared with the guest over NFS.
//!
//! A machine with shares runs behind a switch in the process that waits on
//! QEMU (see [`origami_net`]): the guest's NIC and the machine's own
//! network each connect to it as QEMU stream netdevs, and a userspace
//! stack on the same network serves the shares.

use crate::{MachineFile, Result, Share};
use origami_net::{StackConfig, Switch, SwitchConfig};
use origami_nfs::{LogLevel, Options, Server};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The stack's address on a user network, beside slirp's host at 192.0.2.2.
const USER_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 4);
/// And on the private network, beside Instigator's host at 10.98.0.2.
const PRIVATE_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 98, 0, 4);
const FIXED_MAC: [u8; 6] = [0x08, 0x00, 0x69, 0x00, 0x00, 0x04];

/// Where the stack sits on the machine's network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    pub ip: Ipv4Addr,
    pub prefix: u8,
    pub mac: [u8; 6],
}

/// The loopback ports the switch listens on, as QEMU is told them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ports {
    /// The guest NIC's stream.
    pub guest: String,
    /// The machine's network, behind a hub.
    pub network: String,
}

impl Ports {
    /// Stand-ins for `show-command`, which starts no switch.
    pub fn placeholder() -> Ports {
        Ports {
            guest: "<guest-port>".into(),
            network: "<network-port>".into(),
        }
    }
}

/// The QEMU options that put the machine's network (`net0`) and the guest
/// NIC (`share-guest`) on either side of the switch.
pub fn netdev_arguments(ports: &Ports) -> Vec<String> {
    let stream = |id: &str, port: &str| {
        format!(
            "stream,id={id},addr.type=inet,addr.host=127.0.0.1,addr.port={port},server=off,reconnect-ms=1000"
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

/// Where the stack lives for this machine's network mode.
pub fn address(file: &MachineFile) -> Result<Address> {
    let (ip, prefix) = match file.network.mode.as_str() {
        "user" => (USER_ADDRESS, 24),
        "private" => (PRIVATE_ADDRESS, 24),
        "tap" | "vmnet" => {
            let text = file.network.share_address.as_deref().ok_or_else(|| {
                format!(
                    "shares on a {} network need an address on it: \
                     origami network-set DIR --mode {} --share-address ADDRESS/PREFIX",
                    file.network.mode, file.network.mode
                )
            })?;
            parse_address(text)?
        }
        _ => return Err(NEEDS_NETWORK.into()),
    };
    // On a shared LAN the stack's MAC must differ per machine. The
    // machine's own is unique, so borrow its last three bytes.
    let mac = match (file.network.mode.as_str(), &file.identity) {
        ("tap" | "vmnet", Some(identity)) => {
            let bytes: Vec<u8> = identity
                .mac
                .split(':')
                .filter_map(|b| u8::from_str_radix(b, 16).ok())
                .collect();
            let mut mac = [0x08, 0x00, 0x69, 0, 0, 0];
            if bytes.len() == 6 {
                mac[3] = bytes[3] ^ 0x80;
                mac[4] = bytes[4];
                mac[5] = bytes[5];
            }
            mac
        }
        _ => FIXED_MAC,
    };
    Ok(Address { ip, prefix, mac })
}

const NEEDS_NETWORK: &str = "shares need user networking: origami network-set DIR --mode user";

fn parse_address(text: &str) -> Result<(Ipv4Addr, u8)> {
    let bad = || format!("share address '{text}' must look like 192.168.1.50/24");
    let (ip, prefix) = text.split_once('/').ok_or_else(bad)?;
    let prefix: u8 = prefix.parse().map_err(|_| bad())?;
    if !(1..=30).contains(&prefix) {
        return Err(bad().into());
    }
    Ok((ip.parse().map_err(|_| bad())?, prefix))
}

/// The commands that mount a share in IRIX.
pub fn mount_lines(file: &MachineFile, share: &Share) -> Result<[String; 2]> {
    let address = address(file)?;
    Ok([
        format!("mkdir -p /mnt/{}", share.name),
        format!("mount {}:/{} /mnt/{}", address.ip, share.name, share.name),
    ])
}

/// Checks the machine's shares and the network they need.
pub fn validate(file: &MachineFile) -> Result<()> {
    if file.network.share_address.is_some()
        && !matches!(file.network.mode.as_str(), "tap" | "vmnet")
    {
        return Err("--share-address applies to tap and vmnet networks".into());
    }
    if file.share.is_empty() {
        return Ok(());
    }
    let mut names = std::collections::HashSet::new();
    for share in &file.share {
        check_name(&share.name)?;
        if !names.insert(&share.name) {
            return Err(format!("duplicate share name: {}", share.name).into());
        }
        if !share.path.is_absolute() {
            return Err(format!("share {} needs an absolute path", share.name).into());
        }
    }
    address(file).map(|_| ())
}

/// A share name is a single path component the guest can type.
pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        || name.starts_with('.')
    {
        return Err(
            "share name must use letters, digits, hyphens, underscores or dots, and not start with a dot"
                .into(),
        );
    }
    Ok(())
}

/// A running switch and stack for one machine. Dropping it stops them.
pub struct Running {
    switch: Switch,
}

impl Running {
    pub fn ports(&self) -> Ports {
        Ports {
            guest: self.switch.guest_addr().port().to_string(),
            network: self.switch.network_addr().port().to_string(),
        }
    }
}

/// Starts the switch for a machine with shares, or returns None for one
/// without.
pub fn start(dir: &Path, file: &MachineFile) -> Result<Option<Running>> {
    if file.share.is_empty() {
        return Ok(None);
    }
    let address = address(file)?;
    let mut shares = Vec::new();
    for share in &file.share {
        if !share.path.is_dir() {
            return Err(format!(
                "share {}: {} is not a directory",
                share.name,
                share.path.display()
            )
            .into());
        }
        // Windows hosts have no inode numbers, so handles map paths
        // through a table that persists in the machine directory.
        let path_table =
            cfg!(windows).then(|| dir.join(format!("state/share-{}.handles", share.name)));
        shares.push(origami_nfs::Share::open(
            &share.name,
            &share.path,
            Options {
                path_table,
                // Where the host keeps no file birth times, as on an NFS
                // mount, handles come from this table instead of inodes.
                table_fallback: Some(dir.join(format!("state/share-{}.handles", share.name))),
                ..Options::default()
            },
        )?);
    }
    let log_path: PathBuf = dir.join("logs/shares.log");
    fs::create_dir_all(dir.join("logs"))?;
    let logger: origami_nfs::Logger = Arc::new(move |level, message| {
        if level >= LogLevel::Info {
            if let Ok(mut log) = OpenOptions::new().create(true).append(true).open(&log_path) {
                let _ = writeln!(log, "{message}");
            }
        }
    });
    let server = Server::new(Arc::new(origami_nfs::Shares::new(shares))).with_logger(logger);
    let loopback = |port| SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let switch = Switch::start(
        SwitchConfig {
            stack: StackConfig {
                mac: address.mac,
                ip: address.ip,
                prefix: address.prefix,
            },
            guest: loopback(0),
            network: loopback(0),
        },
        Arc::new(server),
    )?;
    Ok(Some(Running { switch }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Identity, Machine, Network};

    fn file(mode: &str) -> MachineFile {
        let machines = crate::test_machines();
        let offer = crate::preset(&machines, "origin200-1").unwrap();
        MachineFile {
            format: crate::MACHINE_FORMAT,
            machine: Machine {
                topology: None,
                population: vec![],
                inputs: Default::default(),
                model: offer.product.clone(),
                nodes: offer.nodes,
                cpus_per_node: offer.cpus_per_node[0],
                memory_per_node: format!("{}MiB", offer.memory.default),
                console: None,
                processor: None,
            },
            identity: None,
            network: Network {
                mode: mode.into(),
                ..Network::default()
            },
            pipe: vec![],
            drive: vec![],
            share: vec![Share {
                name: "src".into(),
                path: std::env::temp_dir(),
            }],
            qemu: Default::default(),
        }
    }

    #[test]
    fn the_address_follows_the_network_mode() {
        let a = address(&file("user")).unwrap();
        assert_eq!((a.ip.to_string(), a.prefix), ("192.0.2.4".into(), 24));
        assert_eq!(a.mac, [0x08, 0x00, 0x69, 0, 0, 4]);
        assert_eq!(
            address(&file("private")).unwrap().ip.to_string(),
            "10.98.0.4"
        );
        let none = address(&file("none")).unwrap_err().to_string();
        assert_eq!(
            none,
            "shares need user networking: origami network-set DIR --mode user"
        );
        let tap = address(&file("tap")).unwrap_err().to_string();
        assert!(tap.contains("--share-address"), "{tap}");
        let mut f = file("tap");
        f.network.share_address = Some("192.168.1.50/24".into());
        f.identity = Some(Identity {
            mac: "08:00:69:12:34:56".into(),
        });
        let a = address(&f).unwrap();
        assert_eq!((a.ip.to_string(), a.prefix), ("192.168.1.50".into(), 24));
        assert_eq!(a.mac, [0x08, 0x00, 0x69, 0x92, 0x34, 0x56]);
    }

    #[test]
    fn bad_addresses_and_names_are_refused() {
        for text in [
            "192.168.1.50",
            "192.168.1.50/0",
            "192.168.1.50/31",
            "nope/24",
            "1.2.3.4/x",
        ] {
            assert!(parse_address(text).is_err(), "{text}");
        }
        for name in ["", ".hidden", "a/b", "a b", "naïve"] {
            assert!(check_name(name).is_err(), "{name}");
        }
        for name in ["src", "my-code_2.0"] {
            check_name(name).unwrap();
        }
        let mut f = file("user");
        f.share.push(f.share[0].clone());
        assert!(validate(&f).unwrap_err().to_string().contains("duplicate"));
        f.share.pop();
        f.share[0].path = "relative".into();
        assert!(validate(&f).unwrap_err().to_string().contains("absolute"));
        let mut f = file("user");
        f.network.share_address = Some("192.168.1.50/24".into());
        assert!(validate(&f).is_err());
    }

    #[test]
    fn the_mount_lines_name_the_stack_address() {
        let f = file("user");
        assert_eq!(
            mount_lines(&f, &f.share[0]).unwrap(),
            ["mkdir -p /mnt/src", "mount 192.0.2.4:/src /mnt/src"]
        );
    }
}
