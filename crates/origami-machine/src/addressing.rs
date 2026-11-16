//! The addresses Origami gives a guest, one scheme for every network.
//!
//! QEMU's user network, the hosted private network and Instigator's install
//! network share one /24: the host and DHCP server at .2, slirp's DNS at
//! .3, the share stack at .4 and the guest at .65. The default is
//! 10.98.0.0/24 and a machine can choose another private /24. IRIX's DHCP
//! client refuses an offer from a server other than the one in its cached
//! lease, so a guest moved between modes keeps its network only if the
//! server looks the same in each.

use crate::{MachineFile, Result};
use std::net::Ipv4Addr;

/// The subnet a machine uses unless it names another.
pub const DEFAULT_SUBNET: &str = "10.98.0.0/24";
/// Every subnet is a /24.
pub const PREFIX: u8 = 24;

/// The addresses on one machine's user, private and install networks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Addresses {
    network: [u8; 3],
}

impl Default for Addresses {
    fn default() -> Addresses {
        Addresses {
            network: [10, 98, 0],
        }
    }
}

impl Addresses {
    /// Reads `A.B.C.0/24` where the network is in a private range.
    pub fn parse(text: &str) -> Result<Addresses> {
        let bad = |why: &str| {
            format!("subnet '{text}' {why}; use a private /24 such as {DEFAULT_SUBNET}")
        };
        let (address, prefix) = text
            .split_once('/')
            .ok_or_else(|| bad("must look like A.B.C.0/24"))?;
        let address: Ipv4Addr = address
            .parse()
            .map_err(|_| bad("must look like A.B.C.0/24"))?;
        if prefix != "24" {
            return Err(bad("must be a /24").into());
        }
        let [a, b, c, d] = address.octets();
        if d != 0 {
            return Err(bad("must end in .0").into());
        }
        if !address.is_private() {
            return Err(bad("must be in 10.0.0.0/8, 172.16.0.0/12 or 192.168.0.0/16").into());
        }
        Ok(Addresses { network: [a, b, c] })
    }

    /// The machine's subnet: its own setting or the default.
    pub fn of(file: &MachineFile) -> Result<Addresses> {
        match file.network.subnet.as_deref() {
            Some(text) => Addresses::parse(text),
            None => Ok(Addresses::default()),
        }
    }

    fn at(&self, last: u8) -> Ipv4Addr {
        let [a, b, c] = self.network;
        Ipv4Addr::new(a, b, c, last)
    }

    /// The network in the form Instigator's configuration takes.
    pub fn cidr(&self) -> String {
        format!("{}/{PREFIX}", self.at(0))
    }

    /// The host: slirp's host address in user mode, and the private
    /// network's DHCP server, whose address also serves shares there.
    pub fn host(&self) -> Ipv4Addr {
        self.at(2)
    }

    /// What slirp's resolver answers on: the host plus one.
    pub fn dns(&self) -> Ipv4Addr {
        self.at(3)
    }

    /// The share stack in every mode.
    pub fn shares(&self) -> Ipv4Addr {
        self.at(4)
    }

    /// Where the guest gets its address: slirp's `dhcpstart`, and the one
    /// address the private network leases.
    pub fn guest(&self) -> Ipv4Addr {
        self.at(65)
    }

    /// The `-netdev user` options that set the network up.
    pub fn user_netdev(&self) -> String {
        format!(
            "user,id=net0,net={},host={},dhcpstart={}",
            self.cidr(),
            self.host(),
            self.guest()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_10_98_0_0() {
        let a = Addresses::default();
        assert_eq!(a, Addresses::parse(DEFAULT_SUBNET).unwrap());
        assert_eq!(
            [a.host(), a.dns(), a.shares(), a.guest()].map(|ip| ip.to_string()),
            ["10.98.0.2", "10.98.0.3", "10.98.0.4", "10.98.0.65"]
        );
        assert_eq!(a.cidr(), DEFAULT_SUBNET);
        assert_eq!(
            a.user_netdev(),
            "user,id=net0,net=10.98.0.0/24,host=10.98.0.2,dhcpstart=10.98.0.65"
        );
    }

    #[test]
    fn every_address_follows_the_subnet() {
        for (text, host, guest) in [
            ("10.42.0.0/24", "10.42.0.2", "10.42.0.65"),
            ("172.16.7.0/24", "172.16.7.2", "172.16.7.65"),
            ("172.31.255.0/24", "172.31.255.2", "172.31.255.65"),
            ("192.168.50.0/24", "192.168.50.2", "192.168.50.65"),
        ] {
            let a = Addresses::parse(text).unwrap();
            assert_eq!(a.cidr(), text);
            assert_eq!(a.host().to_string(), host);
            assert_eq!(a.guest().to_string(), guest);
            let mask = u32::MAX << (32 - PREFIX);
            for ip in [a.host(), a.dns(), a.shares(), a.guest()] {
                assert_eq!(u32::from(ip) & mask, u32::from(a.at(0)), "{text} {ip}");
            }
            assert_eq!(u32::from(a.dns()), u32::from(a.host()) + 1);
            assert!(a.user_netdev().contains(&format!("net={text},host={host}")));
        }
    }

    #[test]
    fn only_private_slash_24_networks_are_accepted() {
        for text in [
            "10.42.0.0",
            "10.42.0.0/16",
            "10.42.0.0/25",
            "10.42.0.5/24",
            "10.42.0.0/x",
            "nope/24",
            "8.8.8.0/24",
            "192.0.2.0/24",
            "172.32.0.0/24",
            "172.15.0.0/24",
            "169.254.1.0/24",
            "127.0.0.0/24",
            "",
        ] {
            let error = Addresses::parse(text).unwrap_err().to_string();
            assert!(
                error.contains(&format!("subnet '{text}'")),
                "{text}: {error}"
            );
            assert!(error.contains("such as 10.98.0.0/24"), "{text}: {error}");
        }
        assert!(Addresses::parse("10.42.0.0/16")
            .unwrap_err()
            .to_string()
            .contains("must be a /24"));
        assert!(Addresses::parse("8.8.8.0/24")
            .unwrap_err()
            .to_string()
            .contains("172.16.0.0/12"));
    }
}
