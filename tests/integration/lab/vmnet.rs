//! The macOS lab: socket_vmnet's shared network, with a real dnsmasq (DHCP
//! and TFTP) on the host side of it and a packet capture of the BOOTP and
//! DHCP exchange.
//!
//! socket_vmnet must already be listening (the job starts it). The host is the
//! peer: vmnet gives it the gateway address on a bridge interface, and guests
//! reach it there. Both daemons need root, so they run under `sudo -n`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use regex::Regex;

use crate::lab::find_tool;
use crate::support::cleanup::{self, Registration};
use crate::support::product::run_limited;
use crate::support::{pause, secs, Result};

pub const POOL_PREFIX: &str = "192.168.105.";

pub struct VmnetLab {
    pub socket: PathBuf,
    pub gateway: String,
    pub bridge: String,
    pub logs: PathBuf,
    daemons: Vec<Child>,
    _registration: Registration,
}

/// The socket_vmnet socket the job started, from `ORIGAMI_VMNET_SOCKET`.
pub fn socket_from_env() -> PathBuf {
    match std::env::var_os("ORIGAMI_VMNET_SOCKET") {
        Some(s) if !s.is_empty() => PathBuf::from(s),
        _ => panic!("ORIGAMI_VMNET_SOCKET is not set; it names the socket socket_vmnet listens on"),
    }
}

pub fn gateway_from_env() -> String {
    std::env::var("ORIGAMI_VMNET_GATEWAY").unwrap_or_else(|_| "192.168.105.1".into())
}

/// The host interface that carries the vmnet gateway address.
fn bridge_for(gateway: &str) -> Result<String> {
    let out = run_limited(Command::new("ifconfig"), secs(30))?.stdout;
    let header = Regex::new(r"^(\w+): flags")?;
    let inet = Regex::new(&format!(r"\binet {}\b", regex::escape(gateway)))?;
    let mut name = None;
    for line in out.lines() {
        if let Some(h) = header.captures(line) {
            name = Some(h[1].to_string());
        } else if inet.is_match(line) {
            return name.ok_or_else(|| "ifconfig listed an address before any interface".into());
        }
    }
    Err(format!("no host interface has {gateway}; is socket_vmnet running?").into())
}

fn terminate(pids: &[u32]) {
    for &pid in pids {
        // sudo relays the signal to its command.
        // SAFETY: signalling processes this test started.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
}

impl VmnetLab {
    /// Start dnsmasq on the vmnet bridge, serving DHCP and a small TFTP root
    /// holding `unix`, and capture the DHCP exchange to `logs/bootp.txt`.
    pub fn start(work: &Path) -> Result<VmnetLab> {
        let socket = socket_from_env();
        if !socket.exists() {
            return Err(format!(
                "{} does not exist; start socket_vmnet first",
                socket.display()
            )
            .into());
        }
        let gateway = gateway_from_env();
        let bridge = bridge_for(&gateway)?;
        println!("bridge carrying {gateway}: {bridge}");
        let logs = work.join("lab");
        let tftp = work.join("tftp");
        fs::create_dir_all(&logs)?;
        fs::create_dir_all(&tftp)?;
        // The boot file stays under one TFTP block so a logged transfer is a whole one.
        fs::write(tftp.join("unix"), b"origami vmnet check\n".repeat(10))?;

        // launchd holds the DHCP port for macOS's bootpd, which ignores the
        // PROM's BOOTP request. Take it over so dnsmasq can serve the bridge.
        let mut bootout = Command::new("sudo");
        bootout.args(["-n", "launchctl", "bootout", "system/com.apple.bootpd"]);
        let _ = run_limited(bootout, secs(30));

        let dnsmasq_log = fs::File::create(logs.join("dnsmasq.log"))?;
        let mut dnsmasq = Command::new("sudo");
        dnsmasq
            .args(["-n"])
            .arg(find_tool("dnsmasq")?)
            .args(["--keep-in-foreground", "--conf-file=/dev/null", "--port=0"])
            .arg(format!("--interface={bridge}"))
            .arg("--bind-interfaces")
            .arg(format!("--listen-address={gateway}"))
            .arg(format!(
                "--dhcp-range={POOL_PREFIX}100,{POOL_PREFIX}119,255.255.255.0,1h"
            ))
            .args([
                "--bootp-dynamic",
                "--dhcp-broadcast",
                "--dhcp-boot=/unix",
                "--enable-tftp",
            ])
            .arg(format!("--tftp-root={}", tftp.display()))
            .args(["--log-dhcp", "--log-facility=-", "--user=root"])
            .arg(format!(
                "--dhcp-leasefile={}",
                logs.join("dnsmasq.leases").display()
            ))
            .arg(format!("--pid-file={}", logs.join("dnsmasq.pid").display()))
            .stdin(Stdio::null())
            .stdout(dnsmasq_log.try_clone()?)
            .stderr(dnsmasq_log);
        let mut dnsmasq = dnsmasq.spawn()?;
        pause(secs(3));
        if let Some(status) = dnsmasq.try_wait()? {
            let text = fs::read_to_string(logs.join("dnsmasq.log")).unwrap_or_default();
            return Err(format!("dnsmasq exited {status}: {text}").into());
        }
        let dump_out = fs::File::create(logs.join("bootp.txt"))?;
        let mut dump = Command::new("sudo");
        dump.args([
            "-n",
            "tcpdump",
            "-i",
            &bridge,
            "-l",
            "-n",
            "-e",
            "-vv",
            "udp port 67 or udp port 68",
        ])
        .stdin(Stdio::null())
        .stdout(dump_out.try_clone()?)
        .stderr(dump_out);
        let dump = dump.spawn()?;
        pause(secs(2));

        let pids = vec![dnsmasq.id(), dump.id()];
        let registration = cleanup::register("vmnet lab", move || terminate(&pids));
        Ok(VmnetLab {
            socket,
            gateway,
            bridge,
            logs,
            daemons: vec![dnsmasq, dump],
            _registration: registration,
        })
    }

    pub fn dnsmasq_log(&self) -> String {
        fs::read_to_string(self.logs.join("dnsmasq.log")).unwrap_or_default()
    }

    pub fn leases(&self) -> String {
        fs::read_to_string(self.logs.join("dnsmasq.leases")).unwrap_or_default()
    }

    pub fn capture(&self) -> String {
        fs::read_to_string(self.logs.join("bootp.txt")).unwrap_or_default()
    }
}

impl Drop for VmnetLab {
    fn drop(&mut self) {
        terminate(&self.daemons.iter().map(Child::id).collect::<Vec<_>>());
        for d in &mut self.daemons {
            let _ = d.wait();
        }
    }
}
