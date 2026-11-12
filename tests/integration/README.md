# Integration tests

Real IRIX 6.5.30 guests, run by the real `origami` and its bundled QEMU, on a
virtual LAN with real peer services. They answer questions the unit tests
cannot, such as whether a guest on a bridge can mount an NFS export from a
server on the LAN.

## Run them

You need a runnable product tree (`bin/origami`, `libexec/origami/`), such as
`out/linux/run` from [a Linux build](../../docs/development.md#linux), and
rootless Podman or Docker. No host root is used.

```sh
tests/integration/run.py --list
tests/integration/run.py --product out/linux/run --work /scratch/it-work \
    --cache /scratch/irix-cache --evidence /scratch/it-evidence --jobs 2 [scenario ...]
```

`--engine "podman --url ..."` picks the container engine. Each scenario
runs in its own container, at most `--jobs` at a time, and each guest is a
copy-on-write overlay of a cached installed disk that is never written.
Each scenario may run two guests, and CI builders share a host, so keep
`--jobs` small.

## How it fits together

- **Scenario**: `scenarios/<name>.py` defines `SCENARIO`
  (`harness/scenario.py`): a name, the `ImageSpec` it needs, how many tap
  devices, a timeout and a `run(ctx)` function that returns a dict of what it
  measured. Raising fails it. Add a file and it is picked up.
- **Image store**: `harness/images.py` lists installed images. `--cache` holds
  one directory per image, named from the machine, profile and a hash of
  `install.py`, and the install runs once, in a container with network access
  for the public media. Every scenario and later run reuses it. `install.py`
  drives the Origin 200 PROM and installer; another machine family needs its
  own driver steps there.
- **Lab**: each scenario gets a fresh container with no outside network, its
  own namespace, a bridge, `tap0..` and a peer in a second namespace running
  dnsmasq (DHCP), rpcbind and unfs3 (NFSv3). It needs `NET_ADMIN`, `NET_RAW`,
  `SYS_ADMIN` (to make the peer's namespace) and `/dev/net/tun`.
- **Results**: `--evidence DIR` gets `DIR/<scenario>/result.json` (status,
  duration, error, data), the guests' serial logs, DHCP and packet captures
  beside it, and `DIR/summary.json`.
- **Timeouts and teardown**: `timeout` inside the container enforces each
  scenario's limit and the install's. The scenario shuts guests down in a
  `finally`, and the container takes anything left with it.

Before a scenario's guests start, one boot removes the install network's DHCP
lease from the overlay. IRIX otherwise keeps the old address on a new network
and its DHCP client loops on the new server's offers.

## Scenarios

- `lan-tap`: `network-set --mode tap` joins the guest to the bridge. IRIX
  takes an address from the peer's DHCP server, pings it, mounts its export
  with a plain `mount`, and reads and writes files whose checksums match on
  both sides.
- `nat-user`: the same server from QEMU's user networking, through NAT. Ping
  and NFS work against an export that accepts any client port. NAT sends
  from an unprivileged source port, so an export that wants a reserved one
  (unfs3's default, like Linux's `secure`) refuses the mount with
  `Permission denied`. The test asserts both outcomes.
- `two-guests`: two machines on one bridge get different MACs and
  addresses, both mount the peer's export, and one mounts a directory the
  other exports. Creating a machine with a used MAC is refused.

`.github/workflows/network-integration.yml` runs all of them nightly.
