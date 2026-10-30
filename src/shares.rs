//! Host folders shared with the guest over NFS.
//!
//! A machine with shares runs behind a switch in the process that waits on
//! QEMU (see [`origami_net`]): the guest's NIC and the machine's own
//! network each connect to it as QEMU stream netdevs, and a userspace
//! stack on the same network serves the shares.
//!
//! A private network is the exception. Its only host is Instigator's
//! install server, or, when none is listening, this process: the stack
//! then is the network, answers BOOTP and DHCP as Instigator does, and
//! serves any shares at the host's own address, 10.98.0.2, and at 10.98.0.4,
//! where a user network has them.

use crate::{
    addressing::{self, Addresses},
    MachineFile, Result, Share,
};
use origami_net::{Addressing, Client, Endpoint, StackConfig, Switch, SwitchConfig};
use origami_nfs::{LogLevel, Options, Server};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const FIXED_MAC: [u8; 6] = [0x08, 0x00, 0x69, 0x00, 0x00, 0x04];
const PRIVATE_MAC: [u8; 6] = [0x08, 0x00, 0x69, 0x00, 0x00, 0x02];

/// Where the stack sits on the machine's network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    pub ip: Ipv4Addr,
    pub prefix: u8,
    pub mac: [u8; 6],
}

/// Where the switch listens, as the `addr.` options QEMU's stream netdev
/// takes: loopback TCP, or a Unix socket where the platform has them.
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
            guest: tcp_options("<guest-port>"),
            network: tcp_options("<network-port>"),
        }
    }
}

fn tcp_options(port: &str) -> String {
    format!("addr.type=inet,addr.host=127.0.0.1,addr.port={port}")
}

fn options(endpoint: &Endpoint) -> String {
    match endpoint {
        Endpoint::Tcp(addr) => tcp_options(&addr.port().to_string()),
        Endpoint::Unix(path) => {
            format!("addr.type=unix,addr.path={}", crate::qemu_path_option(path))
        }
    }
}

/// The QEMU options that put the machine's network (`net0`) and the guest
/// NIC (`share-guest`) on either side of the switch.
pub fn netdev_arguments(ports: &Ports) -> Vec<String> {
    let stream =
        |id: &str, addr: &str| format!("stream,id={id},{addr},server=off,reconnect-ms=1000");
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
    let subnet = Addresses::of(file)?;
    let (ip, prefix) = match file.network.mode.as_str() {
        // Beside slirp's host.
        "user" => (subnet.shares(), addressing::PREFIX),
        // The stack is the host Instigator would be.
        "private" => (subnet.host(), addressing::PREFIX),
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
    if file.network.mode == "private" {
        return Ok(Address {
            ip,
            prefix,
            mac: PRIVATE_MAC,
        });
    }
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
    /// Whether the stack is the private network itself, with the NIC
    /// connected straight to it.
    hosted: bool,
}

impl Running {
    /// The ports for the hub wiring around the machine's own network.
    pub fn ports(&self) -> Ports {
        Ports {
            guest: options(self.switch.guest_endpoint()),
            network: options(self.switch.network_endpoint()),
        }
    }

    /// For a hosted private network, the endpoint the NIC connects to.
    pub fn hosted_endpoint(&self) -> Option<String> {
        self.hosted
            .then(|| format!("tcp:{}", self.switch.guest_addr()))
    }
}

/// Starts the switch for a machine that needs one: a machine with shares,
/// or a private network with no server listening. Returns None for any
/// other.
pub fn start(dir: &Path, file: &MachineFile) -> Result<Option<Running>> {
    let private = file.network.mode == "private";
    let mut guest = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    if private {
        // Hosting means listening where the NIC will connect. Whatever
        // listens there already, Instigator's install server, keeps the
        // machine, and shares are its to serve.
        if let Some(endpoint) = file.network.endpoint.as_deref() {
            match crate::tcp_endpoint(endpoint)? {
                Some(address) => guest = address.into(),
                None if resolve_missing(dir, endpoint) => {}
                None => return Ok(None),
            }
        }
    } else if file.share.is_empty() {
        return Ok(None);
    }
    let address = address(file)?;
    let server = if file.share.is_empty() {
        None
    } else {
        Some(Arc::new(server(dir, file)?))
    };
    let addressing = if private {
        let identity = file
            .identity
            .as_ref()
            .ok_or("private networking needs the machine's MAC; supply --mac")?;
        let mut mac = [0u8; 6];
        for (byte, text) in mac.iter_mut().zip(identity.mac.split(':')) {
            *byte = u8::from_str_radix(text, 16)?;
        }
        Some(Addressing {
            client: Client {
                mac,
                ip: Addresses::of(file)?.guest(),
                hostname: crate::runtime::dhcp_hostname(dir).map(str::to_string),
            },
            dhcp: true,
            server_id: None,
        })
    } else {
        None
    };
    let mut cfg = SwitchConfig::new(StackConfig {
        mac: address.mac,
        ip: address.ip,
        prefix: address.prefix,
        // The same share address as in a user network, so one mount line
        // works in every mode.
        aliases: if private {
            vec![Addresses::of(file)?.shares()]
        } else {
            vec![]
        },
        addressing,
    });
    cfg.guest = guest;
    // Behind slirp the switch carries a guest's whole traffic, and QEMU
    // sends each frame with a call of its own, which a Unix socket makes
    // cheaper than loopback TCP. A hosted private network keeps the
    // address Instigator's contract names.
    if !private {
        cfg.socket_dir = origami_net::socket_dir();
    }
    match Switch::start(cfg, server) {
        Ok(switch) => Ok(Some(Running {
            switch,
            hosted: private,
        })),
        Err(error) if private && error.kind() == io::ErrorKind::AddrInUse => {
            if !file.share.is_empty() {
                eprintln!(
                    "a server is listening at the private network endpoint; \
                     it serves the shares, if it was started with them"
                );
            }
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

/// Whether a Unix-socket endpoint names a socket nobody has made.
fn resolve_missing(dir: &Path, endpoint: &str) -> bool {
    !crate::resolve(dir, endpoint).exists()
}

fn server(dir: &Path, file: &MachineFile) -> Result<Server> {
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
    Ok(Server::new(Arc::new(origami_nfs::Shares::new(shares))).with_logger(logger))
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
        assert_eq!((a.ip.to_string(), a.prefix), ("10.98.0.4".into(), 24));
        assert_eq!(a.mac, [0x08, 0x00, 0x69, 0, 0, 4]);
        let private = address(&file("private")).unwrap();
        assert_eq!(private.ip.to_string(), "10.98.0.2");
        assert_eq!(private.mac, [0x08, 0x00, 0x69, 0, 0, 2]);
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
    fn every_network_uses_the_one_scheme() {
        let subnet = Addresses::default();
        assert_eq!(address(&file("user")).unwrap().ip, subnet.shares());
        assert_eq!(address(&file("private")).unwrap().ip, subnet.host());
        assert_eq!(subnet.guest().to_string(), "10.98.0.65");
    }

    #[test]
    fn the_share_addresses_follow_the_subnet() {
        let mut user = file("user");
        user.network.subnet = Some("172.20.5.0/24".into());
        let a = address(&user).unwrap();
        assert_eq!((a.ip.to_string(), a.prefix), ("172.20.5.4".into(), 24));
        assert_eq!(
            mount_lines(&user, &user.share[0]).unwrap()[1],
            "mount 172.20.5.4:/src /mnt/src"
        );
        let mut private = file("private");
        private.network.subnet = Some("172.20.5.0/24".into());
        assert_eq!(address(&private).unwrap().ip.to_string(), "172.20.5.2");
        assert_eq!(
            mount_lines(&private, &private.share[0]).unwrap()[1],
            "mount 172.20.5.2:/src /mnt/src"
        );
        private.network.subnet = Some("8.8.8.0/24".into());
        assert!(address(&private).is_err());
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
            ["mkdir -p /mnt/src", "mount 10.98.0.4:/src /mnt/src"]
        );
    }

    #[test]
    fn a_private_network_without_a_server_is_hosted() {
        use std::io::{Read, Write};
        let mut f = file("private");
        f.share.clear();
        f.identity = Some(Identity {
            mac: "08:00:69:12:34:56".into(),
        });
        let dir = Path::new("/nonexistent/machine");
        let running = start(dir, &f).unwrap().expect("hosted");
        let endpoint = running.hosted_endpoint().unwrap();
        let address = endpoint.strip_prefix("tcp:").unwrap();
        // The NIC's end: ask who has 10.98.0.2 and hear the stack answer.
        let mut nic = std::net::TcpStream::connect(address).unwrap();
        let mut arp = vec![0xff; 6];
        arp.extend_from_slice(&[0x08, 0, 0x69, 0x12, 0x34, 0x56, 0x08, 0x06]);
        arp.extend_from_slice(&[0, 1, 8, 0, 6, 4, 0, 1, 0x08, 0, 0x69, 0x12, 0x34, 0x56]);
        arp.extend_from_slice(&[10, 98, 0, 65, 0, 0, 0, 0, 0, 0, 10, 98, 0, 2]);
        arp.resize(60, 0);
        let mut framed = (arp.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&arp);
        nic.write_all(&framed).unwrap();
        nic.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut head = [0u8; 4];
        nic.read_exact(&mut head).unwrap();
        let mut reply = vec![0u8; u32::from_be_bytes(head) as usize];
        nic.read_exact(&mut reply).unwrap();
        assert_eq!(reply[6..12], [0x08, 0x00, 0x69, 0, 0, 2]);
        assert_eq!(reply[20..22], [0, 2], "an ARP reply");
        assert_eq!(reply[28..32], [10, 98, 0, 2]);

        // The share address of the user network answers here too.
        let mut arp = arp.clone();
        arp[38..42].copy_from_slice(&[10, 98, 0, 4]);
        let mut framed = (arp.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&arp);
        nic.write_all(&framed).unwrap();
        let mut head = [0u8; 4];
        nic.read_exact(&mut head).unwrap();
        let mut reply = vec![0u8; u32::from_be_bytes(head) as usize];
        nic.read_exact(&mut reply).unwrap();
        assert_eq!(reply[20..22], [0, 2], "an ARP reply");
        assert_eq!(reply[28..32], [10, 98, 0, 4]);

        // A server already listening at the endpoint keeps the machine.
        let mut taken = f.clone();
        taken.network.endpoint = Some(endpoint);
        assert!(start(dir, &taken).unwrap().is_none());
    }
}
