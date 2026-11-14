//! The Linux lab: a bridge on the host, a tap device per guest, and a peer in
//! its own network namespace running real dnsmasq (DHCP), rpcbind and unfs3
//! (NFSv3).
//!
//! ```text
//! host: br 10.77.0.1/24 -- tap0, tap1 (one per guest, owned by the caller)
//!         |
//!        veth -- peer namespace 10.77.0.2/24: dnsmasq, rpcbind, unfsd
//! ```
//!
//! The peer sits in its own namespace, so guests reach it across the bridge
//! as they would a separate machine. The host plays the host: user-mode
//! networking leaves from it, over the bridge.
//!
//! Setup needs root for `ip`, so it runs `sudo -n ip ...` unless the test
//! already is root (a container). Every object carries the test's pid in its
//! name, which lets a later run find and remove what an earlier, killed one
//! left behind.

use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use crate::lab::{dnsmasq_config, find_tool};
use crate::support::cleanup::{self, Registration};
use crate::support::product::run_limited;
use crate::support::{pause, secs, Result};

pub const HOST: &str = "10.77.0.1";
pub const PEER: &str = "10.77.0.2";
pub const PREFIX: &str = "10.77.0.";

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

fn privileged(program: &str) -> Command {
    if is_root() {
        Command::new(program)
    } else {
        let mut c = Command::new("sudo");
        c.args(["-n", program]);
        c
    }
}

fn run(cmd: Command) -> Result<String> {
    let shown = format!("{cmd:?}");
    let out = run_limited(cmd, secs(60))?;
    if out.code != 0 {
        return Err(format!("{shown} failed ({}): {}", out.code, out.text()).into());
    }
    Ok(out.stdout)
}

fn ip(args: &[&str]) -> Result<String> {
    let mut c = privileged("ip");
    c.args(args);
    run(c)
}

/// A command in the lab's namespace. `nsenter` rather than `ip netns exec`:
/// the latter also remounts /sys, which a rootless container may not do.
fn in_ns(ns: &str, program: &str) -> Command {
    let mut c = privileged("nsenter");
    c.arg(format!("--net=/run/netns/{ns}"))
        .arg("--")
        .arg(program);
    c
}

fn ip_ignoring_errors(args: &[&str]) {
    let _ = ip(args);
}

/// What `ip` objects of one lab are called, from the id (the test's pid).
struct Names {
    ns: String,
    bridge: String,
    veth: String,
    veth_peer: String,
}

impl Names {
    fn new(id: u32) -> Names {
        Names {
            ns: format!("oit{id}"),
            bridge: format!("oitb{id}"),
            veth: format!("oitv{id}"),
            veth_peer: format!("oitp{id}"),
        }
    }
    fn tap(id: u32, n: usize) -> String {
        format!("oitt{id}x{n}")
    }
}

fn rule_args(id: u32, action: &str) -> Vec<String> {
    let bridge = Names::new(id).bridge;
    [
        "-w",
        action,
        "FORWARD",
        "-i",
        &bridge,
        "-o",
        &bridge,
        "-m",
        "comment",
        "--comment",
        &format!("origami-it-{id}"),
        "-j",
        "ACCEPT",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Where the peer's launcher records the pid of its session, which is also
/// its process group: every daemon below it is in that group.
fn peer_pidfile(id: u32) -> String {
    format!("/run/oit{id}.pid")
}

/// Remove everything a lab with this id made. Idempotent; never fails.
fn teardown(id: u32) {
    let n = Names::new(id);
    // Peer services first: a namespace with live processes still counts as in
    // use. The daemons are found by their process group. `ip netns pids`, below,
    // cannot see a daemon that dropped its privileges (rpcbind does) unless the
    // caller is real root, so it is only the second line.
    if let Some(group) = fs::read_to_string(peer_pidfile(id))
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
        .filter(|g| *g > 1)
    {
        for signal in ["-TERM", "-KILL"] {
            let mut kill = privileged("kill");
            kill.args([signal, "--", &format!("-{group}")]);
            let _ = run_limited(kill, secs(10));
            pause(std::time::Duration::from_millis(500));
        }
    }
    // /run belongs to root, so a plain remove fails for the caller.
    let mut rm = privileged("rm");
    rm.args(["-f", &peer_pidfile(id)]);
    let _ = run_limited(rm, secs(10));
    for round in 0..20 {
        let pids = ip(&["netns", "pids", &n.ns]).unwrap_or_default();
        let pids: Vec<&str> = pids.split_whitespace().collect();
        if pids.is_empty() {
            break;
        }
        let mut kill = privileged("kill");
        kill.arg(if round < 10 { "-TERM" } else { "-KILL" })
            .args(&pids);
        let _ = run_limited(kill, secs(10));
        pause(std::time::Duration::from_millis(500));
    }
    ip_ignoring_errors(&["netns", "delete", &n.ns]);
    if let Ok(links) = ip(&["-o", "link", "show"]) {
        for name in link_names(&links) {
            if let Some(owner) = lab_id_of(&name) {
                if owner == id {
                    ip_ignoring_errors(&["link", "delete", &name]);
                }
            }
        }
    }
    // Bridged frames reach the FORWARD chain when br_netfilter is loaded
    // (Docker loads it) and the chain's policy is DROP.
    let mut ipt = privileged("iptables");
    ipt.args(rule_args(id, "-D"));
    let _ = run_limited(ipt, secs(10));
}

fn link_names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .map(|n| {
            n.trim_end_matches(':')
                .split('@')
                .next()
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

/// The pid in the name of a lab's link, bridge, veth or tap.
fn lab_id_of(name: &str) -> Option<u32> {
    let rest = ["oitb", "oitv", "oitp", "oitt"]
        .iter()
        .find_map(|p| name.strip_prefix(p))?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Remove labs whose test process is gone: what a kill -9, an OOM or a lost
/// connection leaves behind. A lab whose process is alive is not touched.
pub fn cleanup_stale() {
    let mut ids = std::collections::BTreeSet::new();
    if let Ok(links) = ip(&["-o", "link", "show"]) {
        ids.extend(link_names(&links).iter().filter_map(|n| lab_id_of(n)));
    }
    if let Ok(list) = ip(&["netns", "list"]) {
        for line in list.lines() {
            let name = line.split_whitespace().next().unwrap_or("");
            if let Some(d) = name
                .strip_prefix("oit")
                .filter(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
            {
                ids.extend(d.parse::<u32>());
            }
        }
    }
    for id in ids {
        if id != std::process::id() && !Path::new(&format!("/proc/{id}")).exists() {
            eprintln!("removing the stale lab of pid {id}");
            teardown(id);
        }
    }
}

pub struct LinuxLab {
    id: u32,
    pub taps: Vec<String>,
    /// DHCP leases, peer logs.
    pub logs: PathBuf,
    /// Exported to guests; the peer refuses clients on unprivileged ports.
    pub export: PathBuf,
    /// Exported with `insecure`, which user-mode NAT's source ports need.
    pub export_open: PathBuf,
    peer: Option<Child>,
    _registration: Registration,
}

impl LinuxLab {
    /// What a lab needs from the host, checked early with a plain message.
    pub fn probe() -> Result<()> {
        for tool in [
            "ip", "dnsmasq", "rpcbind", "unfsd", "unshare", "nsenter", "ss",
        ] {
            find_tool(tool)?;
        }
        if !Path::new("/dev/net/tun").exists() {
            return Err("/dev/net/tun is missing; guests cannot open tap devices".into());
        }
        if !is_root() {
            run({
                let mut c = Command::new("sudo");
                c.args(["-n", "true"]);
                c
            })
            .map_err(|e| format!("passwordless sudo is needed for `ip`: {e}"))?;
        }
        Ok(())
    }

    pub fn start(taps: usize, work: &Path) -> Result<LinuxLab> {
        Self::probe()?;
        cleanup_stale();
        if let Ok(addrs) = ip(&["-o", "-4", "addr", "show"]) {
            if let Some(line) = addrs.lines().find(|l| l.contains(&format!(" {PREFIX}"))) {
                return Err(format!("{PREFIX}0/24 is already in use here: {line}").into());
            }
        }
        let id = std::process::id();
        let n = Names::new(id);
        let logs = work.join("lab");
        let export = work.join("export");
        let export_open = work.join("export-open");
        for d in [&logs, &export, &export_open] {
            fs::create_dir_all(d)?;
        }
        if work.to_string_lossy().contains(char::is_whitespace) {
            return Err(format!(
                "{} contains a space, which the NFS export list cannot hold",
                work.display()
            )
            .into());
        }

        // From here on a partial setup is undone by the registered cleanup.
        let registration = cleanup::register("lab", move || teardown(id));
        let mut lab = LinuxLab {
            id,
            taps: Vec::new(),
            logs: logs.clone(),
            export: export.clone(),
            export_open: export_open.clone(),
            peer: None,
            _registration: registration,
        };

        ip(&[
            "link",
            "add",
            &n.bridge,
            "type",
            "bridge",
            "stp_state",
            "0",
            "forward_delay",
            "0",
        ])?;
        ip(&["addr", "add", &format!("{HOST}/24"), "dev", &n.bridge])?;
        ip(&["link", "set", &n.bridge, "up"])?;
        // Position 1: ahead of Docker's DROP policy rules. Best effort; the
        // chain may not exist on a host without iptables.
        let mut ipt_args = rule_args(id, "-I");
        ipt_args.insert(3, "1".into());
        let mut ipt = privileged("iptables");
        ipt.args(ipt_args);
        let _ = run_limited(ipt, secs(10));

        // SAFETY: getuid and getgid have no preconditions.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        for i in 0..taps {
            let tap = Names::tap(id, i);
            ip(&[
                "tuntap",
                "add",
                "dev",
                &tap,
                "mode",
                "tap",
                "user",
                &uid.to_string(),
                "group",
                &gid.to_string(),
            ])?;
            ip(&["link", "set", &tap, "master", &n.bridge, "up"])?;
            lab.taps.push(tap);
        }

        ip(&["netns", "add", &n.ns])?;
        ip(&[
            "link",
            "add",
            &n.veth,
            "type",
            "veth",
            "peer",
            "name",
            &n.veth_peer,
        ])?;
        ip(&["link", "set", &n.veth, "master", &n.bridge, "up"])?;
        ip(&["link", "set", &n.veth_peer, "netns", &n.ns])?;
        let peer_ip = |args: &[&str]| -> Result<String> {
            let mut c = in_ns(&n.ns, "ip");
            c.args(args);
            run(c)
        };
        peer_ip(&["link", "set", "lo", "up"])?;
        peer_ip(&["addr", "add", &format!("{PEER}/24"), "dev", &n.veth_peer])?;
        peer_ip(&["link", "set", &n.veth_peer, "up"])?;
        lab.start_peer(&n)?;
        Ok(lab)
    }

    fn start_peer(&mut self, n: &Names) -> Result<()> {
        fs::write(
            self.logs.join("exports"),
            format!(
                "{} (rw,no_root_squash)\n{} (rw,no_root_squash,insecure)\n",
                self.export.display(),
                self.export_open.display()
            ),
        )?;
        let leases = self.logs.join("dnsmasq.leases");
        let mut conf = dnsmasq_config(
            &n.veth_peer,
            "10.77.0.100,10.77.0.150",
            PEER,
            &leases.to_string_lossy(),
            &self.logs.join("dnsmasq-dhcp.log").to_string_lossy(),
        );
        conf.push_str("pid-file=\n");
        fs::write(self.logs.join("dnsmasq.conf"), conf)?;

        // The three daemons share a private /run: rpcbind and the NFS server
        // find each other through /run/rpcbind.sock, and a host's own
        // rpcbind (Debian starts one when the package is installed) must keep
        // its socket.
        let script = r#"echo $$ > "$7" && mount -t tmpfs tmpfs /run && mkdir -p /run/rpcbind \
            && { "$1" -f -w >"$4/rpcbind.log" 2>&1 & } && sleep 1 \
            && { "$2" -k -C "$5" >"$4/dnsmasq.log" 2>&1 & } \
            && exec "$3" -d -e "$6" >"$4/unfsd.log" 2>&1"#;
        // setsid: the launcher leads a new session, so one signal to its group
        // reaches every daemon.
        let mut cmd = in_ns(&n.ns, "setsid");
        cmd.args(["unshare", "--mount", "sh", "-c", script, "sh"])
            .arg(find_tool("rpcbind")?)
            .arg(find_tool("dnsmasq")?)
            .arg(find_tool("unfsd")?)
            .arg(&self.logs)
            .arg(self.logs.join("dnsmasq.conf"))
            .arg(self.logs.join("exports"))
            .arg(peer_pidfile(self.id))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.peer = Some(cmd.spawn()?);

        // DHCP (67), portmap (111) and NFS (2049) must listen before a guest asks.
        let end = Instant::now() + secs(30);
        loop {
            let mut ss = in_ns(&n.ns, "ss");
            ss.arg("-lntu");
            let listening = run(ss).unwrap_or_default();
            if [":67 ", ":111 ", ":2049 "]
                .iter()
                .all(|p| listening.contains(p))
            {
                return Ok(());
            }
            if Instant::now() > end {
                let logs: String = ["rpcbind", "dnsmasq", "unfsd"]
                    .iter()
                    .map(|l| {
                        format!(
                            "--- {l}.log\n{}",
                            fs::read_to_string(self.logs.join(format!("{l}.log")))
                                .unwrap_or_default()
                        )
                    })
                    .collect();
                return Err(format!(
                    "peer services did not start listening; sockets:\n{listening}\n{logs}"
                )
                .into());
            }
            pause(secs(1));
        }
    }

    pub fn bridge(&self) -> String {
        Names::new(self.id).bridge
    }

    pub fn leases(&self) -> String {
        fs::read_to_string(self.logs.join("dnsmasq.leases")).unwrap_or_default()
    }

    /// Check from the host that the peer answers on the NFS and portmap
    /// ports, across the bridge, as a guest's mount would reach them.
    pub fn peer_answers(&self) -> Result<()> {
        for port in [111u16, 2049] {
            let addr = (PEER, port).to_socket_addrs()?.next().ok_or("no address")?;
            TcpStream::connect_timeout(&addr, secs(5))
                .map_err(|e| format!("{PEER}:{port}: {e}"))?;
        }
        Ok(())
    }

    /// Record the first `count` packets to the peer's portmapper and NFS
    /// port, as evidence of the source ports guests' requests carry. Best
    /// effort: no capture is better than a failed test.
    pub fn capture(&self, file: &Path, count: u32) -> Option<Capture> {
        let out = fs::File::create(file).ok()?;
        let mut cmd = privileged("tcpdump");
        cmd.args([
            "-nn",
            "-U",
            "-c",
            &count.to_string(),
            "-i",
            &self.bridge(),
            "dst",
            PEER,
            "and",
            "(port",
            "111",
            "or",
            "port",
            "2049)",
        ])
        .stdin(Stdio::null())
        .stdout(out.try_clone().ok()?)
        .stderr(out);
        let child = cmd.spawn().ok()?;
        pause(secs(1));
        Some(Capture(child))
    }
}

pub struct Capture(Child);

impl Drop for Capture {
    fn drop(&mut self) {
        // sudo relays the signal to tcpdump.
        // SAFETY: signalling a child this test started.
        unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) };
        let _ = self.0.wait();
    }
}

impl Drop for LinuxLab {
    fn drop(&mut self) {
        teardown(self.id);
        if let Some(mut peer) = self.peer.take() {
            let _ = peer.wait();
        }
        // The daemons wrote their logs, the leases and the exported files as
        // root. Open them to the caller so a job can upload them.
        if !is_root() {
            for dir in [&self.logs, &self.export, &self.export_open] {
                let mut chmod = privileged("chmod");
                chmod.arg("-R").arg("a+rX").arg(dir);
                let _ = run_limited(chmod, secs(30));
            }
        }
    }
}
