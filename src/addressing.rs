//! The addresses Origami gives a guest, one scheme for every network.
//!
//! QEMU's user network, the hosted private network and Instigator's install
//! network all use 10.98.0.0/24: the host and DHCP server at 10.98.0.2, the
//! guest at 10.98.0.65. IRIX's DHCP client refuses an offer from a server
//! other than the one in its cached lease, so a guest that moves between
//! modes keeps its network only if the server looks the same in each.

use std::net::Ipv4Addr;

/// The network, its mask length and its mask in the form Instigator's
/// configuration takes.
pub const NETWORK: Ipv4Addr = Ipv4Addr::new(10, 98, 0, 0);
pub const PREFIX: u8 = 24;
pub const NETWORK_CIDR: &str = "10.98.0.0/24";
/// The host: slirp's host address in user mode, and the private network's
/// DHCP server, whose address also serves shares there.
pub const HOST: Ipv4Addr = Ipv4Addr::new(10, 98, 0, 2);
/// Where the guest gets its address: slirp's `dhcpstart`, and the one
/// address the private network leases.
pub const GUEST: Ipv4Addr = Ipv4Addr::new(10, 98, 0, 65);
/// The share stack in every mode.
pub const SHARES: Ipv4Addr = Ipv4Addr::new(10, 98, 0, 4);
/// What slirp's resolver answers on: the host plus one.
pub const DNS: Ipv4Addr = Ipv4Addr::new(10, 98, 0, 3);

/// The `-netdev user` options that set the network up.
pub fn user_netdev() -> String {
    format!("user,id=net0,net={NETWORK_CIDR},host={HOST},dhcpstart={GUEST}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_addresses_share_one_network() {
        let mask = u32::MAX << (32 - PREFIX);
        for ip in [HOST, GUEST, SHARES, DNS] {
            assert_eq!(u32::from(ip) & mask, u32::from(NETWORK), "{ip}");
        }
        assert_eq!(NETWORK_CIDR, format!("{NETWORK}/{PREFIX}"));
        assert_eq!(DNS, Ipv4Addr::from(u32::from(HOST) + 1));
    }

    #[test]
    fn the_user_network_names_them() {
        assert_eq!(
            user_netdev(),
            "user,id=net0,net=10.98.0.0/24,host=10.98.0.2,dhcpstart=10.98.0.65"
        );
    }
}
