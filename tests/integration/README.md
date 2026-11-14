# Integration tests

IRIX guests run by the real `origami` and its bundled QEMU, on networks with
real peers: dnsmasq for DHCP and TFTP, unfs3 for NFS, a second guest. They answer
questions the unit and functional tests cannot, such as whether a guest on a
bridge can mount an export from a server on the LAN.

An integration test uses real external peers over real networks. The test that
installs IRIX from the public media is an acceptance test and lives in
[`tests/acceptance`](../acceptance/install.rs).

## Run them

They are `#[ignore]`d, so a plain `cargo test` skips them. Asking for them
without what they need fails; it never skips.

```sh
ORIGAMI_PRODUCT=/path/to/extracted/product \
ORIGAMI_WORK=/scratch/it \
    cargo test --test integration -- --ignored --test-threads=1 lab_probe lan_tap
```

| Variable | Meaning |
|---|---|
| `ORIGAMI_PRODUCT` | An extracted product tree: `bin/origami`, `libexec/origami/`. Required. |
| `ORIGAMI_WORK` | Scratch directory; each test uses a subdirectory. Required. No spaces: NFS export lists cannot hold them. |
| `ORIGAMI_DISK_CACHE` | Where the pinned IRIX disk is downloaded and unpacked. Default `$ORIGAMI_WORK/disks`. |
| `ORIGAMI_VMNET_SOCKET` | macOS: the socket `socket_vmnet` listens on. |
| `ORIGAMI_VMNET_GATEWAY` | macOS: the host address of the vmnet network. Default `192.168.105.1`. |
| `ORIGAMI_TAP_ADAPTER` | Windows: the TAP-Windows6 adapter's name, for `tap_windows_probe`. |

Each guest is a copy-on-write overlay of the pinned, prepared Origin 200 disk
(`support/disk.rs` names it and its SHA-256), which is never written. The
disk's DHCP lease and resolver file were removed when it was prepared, so it
takes an address on any network.

## Tests

| Test | OS | What it checks |
|---|---|---|
| `lab_probe` | Linux | The bridge, the peer namespace and the peer's services come up and answer. Seconds, no guest. |
| `lan_tap` | Linux | A guest on a tap device takes a DHCP lease from dnsmasq, pings the peer, mounts an unfs3 export with a plain `mount`, and reads and writes files whose checksums match on both sides. |
| `nat_user` | Linux | A user-mode guest reaches the same server through NAT. An export that wants a reserved source port refuses the mount with `Permission denied`; one marked `insecure` works. |
| `two_guests` | Linux | Two guests on one bridge get different MACs and leases, both mount the peer's export, and one mounts a directory the other exports. Creating a machine with a used MAC is refused. |
| `vmnet_lan` | macOS | A guest on `socket_vmnet` takes a lease from dnsmasq on the vmnet bridge and pings the host. |
| `vmnet_netboot` | macOS | An Octane2 PROM's BOOTP request crosses vmnet to dnsmasq, and the TFTP boot file comes back. |
| `user_mode` | any | A user-mode guest takes the product's lease and pings its gateway. Windows has no bridge or peers, so this is all it can check. |
| `tap_windows_probe` | Windows | Informational: does QEMU start on a TAP-Windows6 adapter? |

## The Linux lab

`lab/linux.rs` builds this with `ip`, using `sudo -n` unless the test is root:

```text
host: bridge 10.77.0.1/24 -- one tap per guest, owned by the caller
        |
       veth -- peer namespace 10.77.0.2/24: dnsmasq, rpcbind, unfsd
```

The peer is a separate namespace so guests reach it across the bridge, as they
would another machine. The three daemons run in a private mount namespace with
their own `/run`, so a host's rpcbind keeps its socket.

Every object has the test's pid in its name (`oitb<pid>` for the bridge, `oit<pid>`
for the namespace). A lab goes away three ways: its `Drop`; a SIGINT or
SIGTERM, which runs every registered cleanup (`support/cleanup.rs`) before the
process exits; and the next run, which removes labs whose pid is gone. A rootless container
works as a root-owned lab if it has `NET_ADMIN`, `NET_RAW`, `SYS_ADMIN` and `/dev/net/tun`.

## When a test fails

A guest that is still running when its test panics leaves, in
`$ORIGAMI_WORK/<test>/evidence/<guest>-failure/`: the sockets of this run's
processes, QEMU's own view of the guest (`info status`, `info network`,
`info registers`), what every thread is waiting on, and for the install test
a goroutine dump from Instigator. Only this run's pids are read or signalled.
`result.json` and the guests' serial logs are beside it.

`.github/workflows/integration-network.yml` runs the Linux, macOS and Windows
jobs daily when `main` has moved. `.github/workflows/acceptance-install.yml`
runs the install weekly.
