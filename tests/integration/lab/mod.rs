//! Virtual LANs with real peer services for guests to talk to.
#![allow(dead_code)]

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod vmnet;

use std::path::PathBuf;

use crate::support::Result;

/// A program the lab runs, found on PATH or in the sbin directories that a
/// user's PATH often lacks.
pub fn find_tool(name: &str) -> Result<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for extra in [
        "/usr/local/sbin",
        "/usr/sbin",
        "/sbin",
        "/opt/homebrew/sbin",
        "/opt/homebrew/bin",
    ] {
        dirs.push(extra.into());
    }
    dirs.into_iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| format!("{name} is not installed; the integration job installs it").into())
}

/// dnsmasq's configuration for a DHCP server on `interface`.
pub fn dnsmasq_config(
    interface: &str,
    range: &str,
    router: &str,
    leases: &str,
    log: &str,
) -> String {
    format!(
        "user=root\nport=0\nno-ping\nbind-interfaces\ninterface={interface}\nexcept-interface=lo\n\
         dhcp-range={range},255.255.255.0,1h\ndhcp-option=option:router,{router}\n\
         dhcp-leasefile={leases}\nlog-dhcp\nlog-facility={log}\n"
    )
}
