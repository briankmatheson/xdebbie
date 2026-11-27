# abe-backed disks

With [abe](https://github.com/briankmatheson/abe) on every host, a VM's
disks can be NVMe/TCP volumes instead of files: made on the host the VM
lands on, reachable from any host.

abe exports LVM volumes over NVMe/TCP. A VM created with `--storage abe`
gets its disks from the abe of the host it is scheduled on, so I/O stays
local, but they are ordinary NVMe-oF subsystems, which another host could
attach. That is the storage half of moving VMs without copying disks.
Live migration of abe-backed VMs is not supported yet: it is refused.

## What you need

- abe 0.5.0 or later (mutual TLS) and `nvme-cli` on each host. abe 0.4.0
  or later for thin clones; 0.7.2 or later to grow volumes.
- A volume group on the host, ideally with a thin pool.
- The mesh's WireGuard underlay up (`ip addr show wg-flock`).

The examples use box 0, underlay address `10.22.0.1`. On box N use
`10.22.N.1`.

## Set up abe on a host

1. Install abe and tell it which volume group to use and to listen on the
   underlay address only. It reuses the box's cluster certificate, so it
   admits only boxes enrolled in the cluster:

   ```bash
   sudo apt install ./abe-block-exporter_VERSION_amd64.deb nvme-cli
   sudo tee /etc/default/abe <<'EOF'
   ABE_VG=VG
   ABE_THIN_POOL=THIN_POOL
   ABE_TRADDR=10.22.0.1
   ABE_ADVERTISE=10.22.0.1
   ABE_LISTEN=10.22.0.1:443
   ABE_TLS_CERT=/etc/xdebbie/api-cert.pem
   ABE_TLS_KEY=/etc/xdebbie/api-key.pem
   ABE_TLS_CA=/etc/xdebbie/api-ca.pem
   EOF
   ```

2. Make abe wait for the underlay at boot (xdebbied creates `wg-flock`),
   start it, and load `nvme-tcp`:

   ```bash
   sudo mkdir -p /etc/systemd/system/abe.service.d
   sudo tee /etc/systemd/system/abe.service.d/underlay.conf <<'EOF'
   [Unit]
   After=xdebbied.service
   [Service]
   ExecStartPre=/usr/bin/timeout 90 /bin/sh -c 'until ip -4 addr show to 10.22.0.1/32 | grep -q inet; do sleep 1; done'
   EOF
   sudo systemctl daemon-reload
   sudo systemctl enable --now abe
   sudo modprobe nvme-tcp
   echo nvme-tcp | sudo tee /etc/modules-load.d/nvme-tcp.conf
   ```

3. Open abe's API and its NVMe/TCP range on the underlay only. Leave
   `xdebbie` out of the comments, or bootstrap deletes the rules:

   ```bash
   sudo ufw allow in on wg-flock proto tcp from 10.22.0.0/16 to any port 23000:23999 comment 'abe nvme/tcp'
   sudo ufw allow in on wg-flock proto tcp from 10.22.0.0/16 to any port 443 comment 'abe api'
   ```

4. Tell xdebbied: add one line to this box's `[[hosts]]` entry in
   `/etc/xdebbie/xdebbied.toml`, then restart:

   ```toml
   [[hosts]]
   name = "box0"
   # ...the lines bootstrap wrote...
   abe = "https://10.22.0.1:443"
   ```

   ```bash
   sudo systemctl restart xdebbied
   ```

5. Create a VM on abe disks:

   ```bash
   xdctl vm create demo --image debian-13 --disk-gib 10 --storage abe
   ```

## What you should see

`xdctl host ls` fills in the `ABE GiB` column (free/total of what may still
be promised) and `TLS yes`. `xdctl vm describe demo` lists the VM's
`disks`; on the host, `sudo nvme list` shows them attached.

## How an abe VM's disks are made

1. The host's `abe` URL says where its abe API is (`http://` for an abe
   before 0.5.0; used as written). A host without one cannot run
   `storage = abe` VMs: the scheduler skips it (`no abe`).
2. At provisioning xdebbied asks abe for the disks and records the replies
   in the VM before anything else happens, so the next pass picks up where
   a crash left off:

   ```json
   "disks": [
     {"role": "root", "server": "https://10.22.0.1:443", "traddr": "10.22.0.1",
      "port": "23000", "id": "3f2c...", "bytes": 10737418240, "populated": true,
      "origin": "b7e1..."},
     {"role": "data", "server": "https://10.22.0.1:443", "traddr": "10.22.0.1",
      "port": "23001", "id": "9a01...", "bytes": 107374182400, "populated": true}
   ]
   ```

   With abe 0.4.0 or later the root is a thin clone of a
   [base volume](#base-volumes) (`origin`), and the data disk is
   `GET /configure?bytes=N&raw=true&name=vm-UUID-data`. The `name` makes a
   retry get the same volume back. Against an older abe (no `GET /version`)
   xdebbied logs that once and makes unnamed volumes, filling the root with
   a full copy.
3. The host attaches each volume:
   `nvme connect -t tcp -a TRADDR -s PORT -n ID --ctrl-loss-tmo -1 --nr-io-queues 4`.
   It retries for ever if the exporter is away, so the guest stalls rather
   than getting I/O errors. abe sets the namespace's `device_uuid` to the
   volume id, so udev makes `/dev/disk/by-id/nvme-uuid.ID`, a path that
   survives reboots and reconnects.
4. Without thin clones, the root is filled once from the host's copy of the
   base image (`/dev/VG/base-IMAGE` with LVM, else the pool's
   `IMAGE.qcow2`) with `qemu-img convert -n -O raw`, and `populated` is set.
   Data volumes stay blank.
5. The domain gets `<disk type='block'>` on those by-id paths. The seed ISO
   stays in the local pool.
6. On every start, including after a host reboot, xdebbied
   [refreshes the endpoints](#endpoint-refresh) and re-attaches anything
   missing first.
7. Deleting the VM disconnects every volume and deletes it on its abe; the
   first error makes the reconciler retry. The base a root was cloned from
   stays.

One abe call may take 120 seconds (a create waits for abe's fio run and
trim). abe's feature list (`/version`) is cached for 60 seconds.

## Base volumes

With abe 0.4.0 or later (`GET /version` lists `base` and `clone`), a root
disk is a thin clone of a base volume: one per image per abe, filled once.

1. `qemu-img info` gives the image's virtual size, rounded up to a MiB. A
   VM whose `--disk-gib` is smaller fails provisioning, saying so.
2. `GET /configure?base=IMAGE&bytes=SIZE` returns that image's base,
   creating it if needed. abe tags it `abe_base=IMAGE`, so a second call
   gets the same one.
3. If the base is not sealed yet, xdebbied connects it, fills it with
   `qemu-img convert`, flushes, disconnects and calls `GET /seal/ID`.
   Sealing un-exports the base for good. All of this runs under a lock per
   (abe, image), so two VMs from one image share one fill.
4. `GET /clone/BASE_ID?bytes=DISK_BYTES&name=vm-UUID-root` makes the root:
   a thin snapshot grown to the size asked. It costs nothing until the
   guest writes.

A sealed base is tied to the image name: re-importing a changed image under
the same name does not refresh it, and nothing deletes old bases. To pick
up a new image, delete the base on abe (abe refuses while clones exist); the
next VM rebuilds it:

```bash
sudo curl --cacert /etc/xdebbie/api-ca.pem --cert /etc/xdebbie/api-cert.pem \
     --key /etc/xdebbie/api-key.pem -X DELETE https://ABE_ADDRESS/id/BASE_ID
```

## Capacity

Every reconcile pass reads `GET /utilization` from each host's abe and
keeps it on the host as `abeCapacity` (`thin`, `bytes`, `usedBytes`,
`freeBytes`, `provisionedBytes`, `dataPercent`, `metadataPercent`, `tls`,
`seen`). A failed read keeps the last one; one older than
`host_stale_secs` (60) counts as unknown.

For a `storage = abe` VM the scheduler ignores the libvirt pool and asks the
host's abe:

| Reason | When |
|---|---|
| `no abe` | the host has no `abe` URL |
| `abe capacity unknown` | no reading yet, or it is stale |
| `abe pool full` | thin pool: data at or over `abe_max_data_percent` (85), or metadata at or over `abe_max_metadata_percent` (80) |
| `abe capacity` | thin: provisioned + pending + this request is over `bytes × abe_overcommit` (1.0); thick: over `freeBytes` |

*Pending* is what is promised but not made yet: disks of placed abe VMs not
yet provisioned, and volume creates in flight. Among hosts that fit, the
score uses the fullest of CPU, memory and the abe pool, so `spread` prefers
the emptier pool.

```toml
[[hosts]]
abe = "https://10.22.1.1:443"
abe_overcommit = 1.0
abe_max_data_percent = 85.0
abe_max_metadata_percent = 80.0
```

## Endpoint refresh

abe hands out ports lowest-free and re-exports everything when it
restarts, so a volume may come back on another port. Before starting a
stopped abe VM, before provisioning one whose disks exist, and before
attaching a volume, xdebbied asks `GET /id/ID` for each disk that is not
connected. When the address or port changed, it drops any controller still
retrying the old one (`nvme disconnect`), updates the record, and logs
`abe moved ID from OLD to NEW; record updated`. A connected disk is left
alone: its controller follows the exporter.

## TLS and the port fence

xdebbied talks to abe with the client it uses for other boxes: the cluster
CA (`ca_file` plus `peer_ca_dir`) as trust, and the box's certificate as
its own. An abe 0.5.0 set up as above therefore admits exactly the boxes
enrolled in the cluster: no token, nothing else to configure. A refused
handshake shows in the log and in the host's inventory error as
`tls: REASON`. TLS applies to `https://` URLs only.

abe opens an NVMe/TCP port only to the box that asked for it: the client
address of the call that made it goes into an nftables set on the abe host,
and everyone else gets a reset. `nvme connect` always runs on the box that
made the abe call, and the endpoint refresh re-asks `GET /id` before each
connect, so the fence follows when a VM moves or abe restarts.

## Addresses

`ABE_ADVERTISE` ends up in `traddr`, so every host that may attach the
volume must reach it. Use the box's underlay address (`10.22.BOX.1`):
NVMe/TCP then rides the encrypted WireGuard link directly, not the Geneve
overlay. Bind both the API (`ABE_LISTEN`) and NVMe/TCP (`ABE_TRADDR`) to it,
so neither is reachable from the LAN or from guests. abe 0.5.0 uses
23000-23999, one port per volume.

Guests on the bridge arrive on `virbr-xdebbie`, not `wg-flock`, so they
cannot reach abe even though `10.22.0.1` is routable from them.

## The storage bridge

Optional: a second, isolated bridge, `virbr-abe`, on which VMs reach their
host's abe directly. Two settings turn it on:

```toml
[network]
cidr = "10.23.0.0/16"
storage_cidr = "10.24.0.0/16"     # same prefix length as cidr

[[hosts]]
name = "box0"
storage_bridge = "virbr-abe"      # libvirt network "abe"
```

Then run `sudo xdebbied bootstrap`. It creates the libvirt network `abe` if
it is missing: bridge `virbr-abe`, the box at `10.24.BOX.1`, isolated (no
`<forward>`), no DHCP. A bridge not named `virbr-NAME` is yours to create.

- A VM defined on that host gets a second NIC there, MAC `52:54:24:` plus
  the same id bytes as its first, and a static `10.24.X.Y/16` with the host
  bits of its overlay address. No gateway and no DNS on it. VMs defined
  before keep one NIC until they are re-created.
- Its network-config carries one route: the host's abe address (`/32`, from
  the host's `abe` URL) via `10.24.BOX.1`. The host accepts packets for
  any of its addresses on any interface, so the guest reaches the same abe,
  with the same fence and TLS. Only that host's abe is routed this way.
- The firewall accepts abe's API port and 23000-23999 on `virbr-abe` from
  the storage network, nothing else, and `xdebbie_host` forwards nothing in
  or out of it.

## Tests

`tests/abe/run.sh` runs the real daemon unprivileged against a fake abe
(python; volumes are files) with stub `nvme`, `qemu-img` and `virsh`. It
covers thin clones, a base filled and sealed once, re-attach after a host
reboot, endpoint refresh, concurrent creates of one volume, a failed
delete, the pre-0.4 path, a crash after seal, capacity (`Pending`, 507) and
hosts without abe.

## If it goes wrong

- If the VM stays `Pending` with `no abe`, then the host's `[[hosts]]`
  entry has no `abe` line, or it is in the wrong table.
- If it says `abe capacity unknown`, then xdebbied has not read abe's
  `/utilization`: `xdctl host describe HOST` shows the error, often
  `tls: ...`.
- If you see `tls: invalid peer certificate: UnknownIssuer`, then abe's
  `ABE_TLS_CA` is not the cluster CA.
- If you see *live migration of abe-backed VMs is not supported yet*, then
  that is true. Delete and re-create, or let Cluster API replace the node.

## Reference

[Host keys `abe`, `abe_*`, `storage_bridge`](../reference/configuration.md#hosts),
[`storage_cidr`](../reference/configuration.md#network),
[Limits: storage](../reference/limits.md#storage).
