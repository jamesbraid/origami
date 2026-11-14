//! Network modes and shares as the machine's QEMU receives them: the fake
//! records its command line and attaches to each stream netdev, so a mode
//! that asks QEMU to dial a port is seen to reach it.

use crate::support::{
    accept_within, create, fake_argv, fake_log, has_option, ok, option_value, wait_until, Running,
};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn machine(root: &Path) -> PathBuf {
    create(root, "machine", "origin200-1", &[])
}

fn start(root: &Path, machine: &Path) -> Running {
    let guard = Running::new(root, machine);
    ok(
        root,
        &[
            "run",
            machine.to_str().unwrap(),
            "--background",
            "--display",
            "none",
        ],
    );
    guard
}

fn netdev<'a>(argv: &'a [String], kind: &str) -> &'a str {
    option_value(argv, "-netdev", kind).unwrap_or_else(|| panic!("no -netdev {kind} in {argv:?}"))
}

#[test]
fn user_networking_forwards_ports_on_loopback() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = machine(root);
    let path = machine.to_str().unwrap();
    ok(
        root,
        &[
            "network-forward-add",
            path,
            "web",
            "--protocol",
            "tcp",
            "--host-port",
            "8080",
            "--guest-port",
            "80",
        ],
    );
    let _guard = start(root, &machine);
    let argv = fake_argv(&machine);
    let user = netdev(&argv, "user,");
    assert!(user.contains("hostfwd=tcp:127.0.0.1:8080-:80"), "{user}");
    assert!(argv.iter().any(|arg| arg.starts_with("nic,")), "{argv:?}");
}

#[test]
fn a_machine_with_no_network_gets_no_netdev() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = machine(root);
    ok(
        root,
        &["network-set", machine.to_str().unwrap(), "--mode", "none"],
    );
    let _guard = start(root, &machine);
    let argv = fake_argv(&machine);
    assert!(!argv.iter().any(|arg| arg == "-netdev"), "{argv:?}");
}

#[test]
fn a_private_network_dials_the_endpoint_over_tcp() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = machine(root);
    let peer = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = peer.local_addr().unwrap().port();
    ok(
        root,
        &[
            "network-set",
            machine.to_str().unwrap(),
            "--mode",
            "private",
            "--endpoint",
            &format!("tcp:127.0.0.1:{port}"),
        ],
    );
    let _guard = start(root, &machine);
    let argv = fake_argv(&machine);
    assert_eq!(
        netdev(&argv, "stream,"),
        format!("stream,id=net0,server=off,addr.type=inet,addr.host=127.0.0.1,addr.port={port},reconnect-ms=1000")
    );
    // QEMU got through to the peer.
    let _connection = accept_within(&peer, Duration::from_secs(20));
    wait_until("the netdev to attach", Duration::from_secs(10), || {
        fake_log(&machine).contains(&"netdev net0 connected".to_string())
    });
}

#[cfg(unix)]
#[test]
fn a_private_network_dials_a_unix_socket_endpoint() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = machine(root);
    let socket = root.join("peer.sock");
    let peer = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    ok(
        root,
        &[
            "network-set",
            machine.to_str().unwrap(),
            "--mode",
            "private",
            "--endpoint",
            socket.to_str().unwrap(),
        ],
    );
    let _guard = start(root, &machine);
    let argv = fake_argv(&machine);
    let stream = netdev(&argv, "stream,");
    assert!(stream.contains("addr.type=unix,addr.path="), "{stream}");
    peer.set_nonblocking(true).unwrap();
    let mut _connection = None;
    wait_until("QEMU to dial the socket", Duration::from_secs(20), || {
        _connection = peer.accept().ok();
        _connection.is_some()
    });
    wait_until("the netdev to attach", Duration::from_secs(10), || {
        fake_log(&machine).contains(&"netdev net0 connected".to_string())
    });
}

#[cfg(target_os = "linux")]
#[test]
fn a_tap_network_names_the_host_interface() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = machine(root);
    ok(
        root,
        &[
            "network-set",
            machine.to_str().unwrap(),
            "--mode",
            "tap",
            "--interface",
            "tap9",
        ],
    );
    let _guard = start(root, &machine);
    let argv = fake_argv(&machine);
    let tap = netdev(&argv, "tap,");
    assert!(tap.contains("ifname=tap9"), "{tap}");
}

#[cfg(target_os = "macos")]
#[test]
fn a_vmnet_network_dials_the_socket_vmnet_socket() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = machine(root);
    let socket = root.join("vmnet.sock");
    let _peer = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    ok(
        root,
        &[
            "network-set",
            machine.to_str().unwrap(),
            "--mode",
            "vmnet",
            "--endpoint",
            socket.to_str().unwrap(),
        ],
    );
    let _guard = start(root, &machine);
    let argv = fake_argv(&machine);
    assert!(
        netdev(&argv, "stream,").contains("addr.type=unix"),
        "{argv:?}"
    );
}

#[test]
fn a_share_puts_a_switch_between_the_guest_and_its_network() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let machine = machine(root);
    let path = machine.to_str().unwrap();
    let code = root.join("code");
    fs::create_dir(&code).unwrap();
    ok(root, &["share", path, "src", code.to_str().unwrap()]);
    let _guard = start(root, &machine);
    let argv = fake_argv(&machine);
    assert!(
        has_option(
            &argv,
            "-netdev",
            "hubport,id=share-hub-net,hubid=1,netdev=net0"
        ),
        "{argv:?}"
    );
    // The guest NIC sits on the switch, and QEMU reaches both sides of it.
    assert!(
        argv.iter()
            .any(|arg| arg == "nic,model=sgi-ioc3-eth,netdev=share-guest"),
        "{argv:?}"
    );
    wait_until(
        "both stream netdevs to attach",
        Duration::from_secs(20),
        || {
            let log = fake_log(&machine);
            log.contains(&"netdev share-guest connected".to_string())
                && log.contains(&"netdev share-net connected".to_string())
        },
    );
}
