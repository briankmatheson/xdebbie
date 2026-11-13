# VM storage: local or ABE

A VM's disks live in one of two places, chosen per VM with `spec.storage`
(`xdctl vm create --storage local|abe`, CAPX `XDebbie8sMachineTemplate
.spec.template.spec.storage`):

| `storage` | root disk | data disk (`dataDiskGib`) | reachable from |
|---|---|---|---|
| `local` (default) | thin LVM snapshot of `base-<image>` or qcow2 overlay in the host's pool | thin LV / raw pool volume | the host that created it |
| `abe` | NVMe/TCP volume exported by the host's `abe` daemon, filled from the base image | NVMe/TCP volume, blank | **any host** that can reach the exporter |

`local` is what xdebbie always did. `abe` is for the case where **every
hypervisor runs an [ABE](https://github.com/briankmatheson/abe) exporter**:
the VM's volumes are created on the ABE of the host it is scheduled on (so
I/O stays local), but they are plain NVMe-oF subsystems, so another host can
attach them later. That is the storage half of live migration without
copying disks. Attaching on a host other than the leader goes through that
host's daemon (`Host.agent`, [autodiscovery](autodiscovery.md)), which
makes the ABE call and the `nvme connect` from the same box, so the
[port fence](#port-fence) follows. Live migration of an abe-backed VM
between boxes is still not done.

## How it works

1. `[[hosts]].abe = "https://<addr>:<port>"` tells xdebbied where each host's
   ABE API is (`http://` for an ABE before 0.5.0; the scheme is used as
   written, nothing rewrites it). Hosts without it cannot run `storage = abe` VMs: the
   scheduler skips them (`no abe`) and the VM stays `Pending` until a host
   with an ABE and enough [capacity](#capacity-accounting) exists.
2. When an `abe` VM is provisioned the driver asks ABE for its disks and
   records the replies in the VM:

   ```json
   "disks": [
     {"role": "root", "server": "https://10.22.0.1:443", "traddr": "10.22.0.1",
      "port": "23000", "id": "3f2c…", "bytes": 10737418240, "populated": true,
      "origin": "b7e1…"},
     {"role": "data", "server": "https://10.22.0.1:443", "traddr": "10.22.0.1",
      "port": "44002", "id": "9a01…", "bytes": 107374182400, "populated": true}
   ]
   ```

   With ABE ≥ 0.4.0 the root is a thin clone of a shared [base
   volume](#base-volumes) (`origin` is the base's id) and the data disk is
   `GET /configure?bytes=N&raw=true&name=vm-<uuid>-data`. ABE's `?name=`
   makes the call idempotent: a retry after a crash gets the same volume
   back (`existing: true`) instead of a second one. The record is persisted
   **before** anything else happens, so the next reconcile tick picks up
   where it left off. Against an ABE older than 0.4.0 (no `GET /version`)
   xdebbied logs `abe at <url> predates 0.4.0: no thin root clones, no name
   idempotency` once and does what 0.14 did: unnamed volumes, root filled
   with a full copy.
3. The host attaches each volume: `nvme connect -t tcp -a <traddr> -s <port>
   -n <id> --ctrl-loss-tmo -1` (retry forever if the exporter is away, so the
   guest stalls instead of taking I/O errors). ABE ≥ 0.2.0 sets the
   namespace's `device_uuid` to the volume id, which makes udev create
   `/dev/disk/by-id/nvme-uuid.<id>` — a path that survives reboots and
   reconnects, unlike `/dev/nvmeXnY`.
4. Without thin clones, the root volume is filled once from the host's
   copy of the base image (`/dev/<vg>/base-<image>` with LVM, else the
   pool's `<image>.qcow2`) using `qemu-img convert -n -O raw`; `populated`
   is then set. Data volumes stay blank.
5. The domain is defined with `<disk type='block'>` pointing at the by-id
   paths. The cloud-init seed ISO stays in the local pool.
6. On every `start` (including after a host reboot, when the domain exists
   but the devices do not) the driver [refreshes the
   endpoints](#endpoint-refresh) and re-attaches anything missing first.
7. Deleting the VM disconnects every volume and `DELETE`s it on its ABE; all
   are attempted and the first error makes the reconciler retry. The base a
   root was cloned from is left alone.

What is *not* done: attaching on a host other than the one that holds the
ABE (remote-host NVMe; `traddr` is reachable from every host, the control
path is not there yet), migration (refused with a clear message), base
garbage collection (a sealed base stays until someone `DELETE`s it on ABE),
picking a different ABE than the scheduled host's.

### Base volumes

With ABE ≥ 0.4.0 (`GET /version` lists `base` and `clone`) a root disk is
not a copy but a thin clone of a **base volume**: one per image per ABE,
filled once.

1. `qemu-img info` gives the image's virtual size, rounded up to a MiB. A
   VM whose `disk_gib` is smaller than that fails provisioning with a clear
   message.
2. `GET /configure?base=<image>&bytes=<size>` returns the base for that
   image, creating it if needed. ABE tags it `abe_base=<image>`, so a second
   call (another VM, another xdebbied tick, a retry after a crash) gets the
   same volume back.
3. If the base is not `sealed` yet xdebbied connects it, fills it with
   `qemu-img convert`, flushes (`blockdev --flushbufs`), disconnects and
   calls `GET /seal/<id>`. Sealing un-exports the base for good (its port is
   freed; `GET /id/<base>` answers 409). All of that happens under a lock
   keyed by `(server, image)`, so two VMs from the same image provisioned at
   once share one fill.
4. `GET /clone/<base>?bytes=<disk_gib>&name=vm-<uuid>-root` makes the root:
   a thin snapshot that is grown to the requested size. The clone costs no
   space until the guest writes, and the provision step takes seconds
   instead of a full image copy. `Vm.disks[].origin` records the base.

A sealed base is tied to the image *name*: re-importing a changed image
under the same name does not refresh it. To pick up a new image, `curl -X
DELETE http://<abe>/id/<base id>` (ABE refuses while clones exist) and the
next VM rebuilds it.

### Capacity accounting

Every reconcile tick polls `GET /utilization` on each host's ABE and keeps
the reading on the host record (`abeCapacity`: `thin`, `bytes`,
`usedBytes`, `freeBytes`, `provisionedBytes`, `dataPercent`,
`metadataPercent`, `seen`). A failed poll keeps the last reading; one older
than `host_stale_secs` is treated as unknown.

For a `storage = abe` VM the scheduler ignores the libvirt pool and asks
`Host::abe_fit(pending, request)`:

| reason | when |
|---|---|
| `no abe` | the host has no `[[hosts]].abe` |
| `abe capacity unknown` | no `/utilization` reading yet, or it is stale |
| `abe pool full` | thin pool: `data_percent` or `metadata_percent` over the host's limits |
| `abe capacity` | `provisioned_bytes + pending + request` exceeds `bytes × abe_overcommit` (a thick VG: `free_bytes`) |

`pending` is what has been promised but does not exist yet: disks of ABE
VMs that are placed but not provisioned, plus standalone volumes whose
`POST` is in flight. Among hosts that fit, the score is the fullest of
cpu, memory and the ABE pool, so `spread` prefers the emptier pool. A VM
that fits nowhere stays `Pending` with the per-host reasons in `message`.

```toml
[[hosts]]
abe = "https://10.22.1.1:443"
abe_overcommit = 1.0            # promised bytes may reach bytes × this (thin pools only)
abe_max_data_percent = 85.0     # refuse new volumes when the pool's data is fuller
abe_max_metadata_percent = 80.0 # … or its metadata
```

`xdctl host ls` shows `ABE GiB` as `free/total` of what may still be
promised (thin: `limit − provisioned / limit`); `xdctl host describe` has
the raw reading.

### Endpoint refresh

ABE assigns ports lowest-free, and a restart re-exports every volume, so a
volume may come back on a different port than the one in xdebbied's record.
Before starting a stopped ABE VM, before provisioning one whose disks exist,
and before attaching a standalone volume, xdebbied asks `GET /id/<id>` for
each disk that is not connected; when `traddr` or `port` differ it drops any
controller still retrying the old port (`nvme disconnect`), updates the
record and logs `abe moved <id> from <old> to <new>; record updated`. A
connected disk is left alone: its controller follows the exporter on its
own.

## Standalone volumes (`/v1/volumes`)

Guests have no route to ABE (it listens on the WireGuard underlay only), so
block storage for things *inside* a VM — Kubernetes PersistentVolumes via
`abe-csi` — goes through xdebbied too:

```sh
xdctl volume create pvc-demo --gib 5        # ABE /configure?name=vol-pvc-demo on the ABE with the most headroom
xdctl volume attach <id> my-vm              # host: nvme connect + virsh attach-device --persistent
#   guest: /dev/disk/by-id/virtio-<first 20 chars of id>
xdctl volume detach <id>                    # host: virsh detach-disk + nvme disconnect
xdctl volume rm <id>                        # ABE DELETE /id/<id>; refused while attached
```

Create and delete are synchronous (the caller needs ABE's id). Create is
serialized per name and idempotent: concurrent `POST`s of one name make one
ABE call, the first answers 201 and the rest 200 with the same record. The
host is the one whose ABE has the most headroom unless `host` pins it; 507
means none fits (`no ABE host fits <bytes> bytes: <host>: <reason>, …`), 503
that no host has an ABE. Delete marks the record `deleting: true` first,
then calls ABE; if ABE fails the record stays with that flag and the call
answers 502, attach answers 409 and the reconciler leaves it alone until a
second `DELETE` finishes the job. Attach and detach are reconciled like
everything else: `attachTo` is the wish,
`attached {vm, host, target}` the observation, `message` says why not. The
reconciler re-runs the attach every tick while it is wanted, so a host
reboot (devices gone) or a recreated domain heals itself. If the VM is
deleted the volume is unplugged and kept.

Hot-plugging a virtio disk on the host beat an in-guest NVMe/TCP initiator
in every fio test we ran (3× on sequential reads, 6× lower p99 on random
writes), because the TCP session stays on the host and the guest only sees
virtio-blk. Volumes attach on the VM's own host only (the same local-nvme
limitation as above).

## Setting up a host

```sh
# the exporter (abe >= 0.5.0), see abe/README.md
sudo apt install ./abe-block-exporter_0.5.0-1_amd64.deb nvme-cli
sudo tee /etc/default/abe <<'EOF'
ABE_VG=timesink-vg            # any VG; a thin pool is recommended
ABE_THIN_POOL=xdebbie-pool
ABE_TRADDR=10.22.0.1          # bind nvmet ports and the API to the WireGuard
ABE_ADVERTISE=10.22.0.1       # underlay address only: reachable from other
ABE_LISTEN=10.22.0.1:443      # hypervisors, not from the LAN or from guests
# the API is mutual TLS; reuse xdebbie's cluster identity (every box's
# api-cert.pem carries serverAuth + clientAuth, and abe runs as root)
ABE_TLS_CERT=/etc/xdebbie/api-cert.pem
ABE_TLS_KEY=/etc/xdebbie/api-key.pem
ABE_TLS_CA=/etc/xdebbie/api-ca.pem
EOF
# wg-flock is created by xdebbied, so make abe wait for it at boot
sudo mkdir -p /etc/systemd/system/abe.service.d
sudo tee /etc/systemd/system/abe.service.d/underlay.conf <<'EOF'
[Unit]
After=xdebbied.service
[Service]
ExecStartPre=/usr/bin/timeout 90 /bin/sh -c 'until ip -4 addr show to 10.22.0.1/32 | grep -q inet; do sleep 1; done'
EOF
sudo systemctl daemon-reload && sudo systemctl enable --now abe
sudo modprobe nvme-tcp && echo nvme-tcp | sudo tee /etc/modules-load.d/nvme-tcp.conf

# tell xdebbied
sudo tee -a /etc/xdebbie/xdebbied.toml <<'EOF'   # inside the host's [[hosts]] block
abe = "https://10.22.0.1:443"
EOF
sudo systemctl restart xdebbied
```

Then `xdctl vm create demo --image debian-13 --disk-gib 10 --storage abe`.
`xdctl vm describe demo | jq .disks` shows the volumes; `nvme list` on the
host shows them attached; `curl --cacert /etc/xdebbie/api-ca.pem --cert
/etc/xdebbie/api-cert.pem --key /etc/xdebbie/api-key.pem
https://10.22.0.1/volumes` shows them on the exporter. `xdctl host ls` has a
`TLS` column (`yes` once `/version` says so, `no` for an older ABE).

### TLS

xdebbied talks to ABE with the same client it uses for other boxes
(`Config::http_client`): the cluster CA (`ca_file` plus every PEM in
`peer_ca_dir`) as trust roots and this box's `tls_cert`/`tls_key` as its
client certificate. An ABE 0.5.0 on the paths above therefore admits
exactly the xdebbieds enrolled under the cluster CA and nothing else: no
token, no extra configuration. A refused handshake shows up in the log and
in the host's inventory error as `tls: <reason>` (for example `tls: invalid
peer certificate: UnknownIssuer` when the CA paths disagree) rather than as
a timeout. TLS applies to `https://` URLs only, so an `http://` host record
keeps working for an ABE 0.4.0 that has not been upgraded.

### Port fence

ABE opens an nvmet port only to the box that asked for it: the client
address of the TLS connection that made the `/configure`, `GET /id` or
`/clone` call goes into an nftables set on the ABE host, and every other
source gets a TCP reset on that port. Nothing in xdebbie had to change for
this: `nvme connect` always runs on the box that made the ABE call (attach
is local), and the [endpoint refresh](#endpoint-refresh) already asks
`GET /id/<id>` for every disk that is not connected before connecting, so
after moving a VM or restoring ABE the next reconcile re-asks and the port
follows. An ABE restarted with volumes exported by 0.4.0 (no client
recorded) adopts the one established connection it finds on each port; a
port with none stays closed until the next `GET /id`, which the reconciler
makes.

### Addresses

`ABE_ADVERTISE` is what ends up in `traddr`, so it must be reachable from
every host that may attach the volume. With the [mesh](mesh.md) on, use the
box's WireGuard underlay address (`10.22.<box>.1`): NVMe/TCP then rides the
encrypted underlay directly instead of the Geneve overlay, and the local
host reaches its own underlay address too. The ABE HTTP API only needs to be
reachable by the xdebbied that schedules VMs onto that host.

Bind both the API (`ABE_LISTEN`) and nvmet (`ABE_TRADDR`) to that same
underlay address: the API admits only cluster certificates and the nvmet
ports are fenced to their caller, but neither should be reachable from the
LAN or from guests on the bridge in the first place. The cost is that abe then depends
on `wg-flock` existing, hence the `After=xdebbied.service` drop-in above.
ABE opens one TCP port per volume (`23000`, `23001`, …) in the 23000-23999
range (abe 0.5.0; earlier versions used 44001-44999, and a volume recorded on
an old port is moved into the range at abe's next re-export).

### Firewall

xdebbied's bootstrap sets ufw to default-deny; the ABE ports must be opened
on the underlay only, which `simple/bin/setup_abe_host.py` does:

```sh
sudo ufw allow in on wg-flock proto tcp from 10.22.0.0/16 to any port 23000:23999 comment 'abe nvme/tcp'
sudo ufw allow in on wg-flock proto tcp from 10.22.0.0/16 to any port 443 comment 'abe api'
```

Guests on the bridge arrive on `virbr-xdebbie`, not `wg-flock`, so they
cannot reach ABE even though 10.22.0.1 is routable from them.

## Configuration reference

```toml
[[hosts]]
name = "timesink"
uri = "qemu:///system"
abe = "https://10.22.0.1:443"   # this host's ABE API (underlay address)
abe_overcommit = 1.0            # thin pool: promised bytes may reach bytes × this
abe_max_data_percent = 85.0     # refuse new volumes above this data usage
abe_max_metadata_percent = 80.0 # … or this metadata usage

# binaries, overridable for tests
nvme = "nvme"
qemu_img = "qemu-img"
nvme_sys = "/sys/class/nvme"
```

`VmSpec.storage`: `local` | `abe` (default `local`). `Vm.disks[]`: `role`
(`root`|`data`), `server`, `traddr`, `port`, `id` (== NQN), `bytes`,
`populated`, `origin` (base id for a thin clone, absent otherwise).
`Host.abeCapacity` is the last `/utilization` reading (its `tls` says the
ABE's `/version` reports mutual TLS); `Volume.deleting`
is set while a delete is in flight or failed. All of these are optional in
the state file, so a 0.14 `state.json` loads unchanged.

## Tests

`tests/abe/run.sh` runs the real daemon unprivileged against a fake ABE
0.4.0 (python; volumes are files) and stub `nvme`/`qemu-img`/`virsh`, with
the work directory bound over `/dev/disk/by-id` in a user namespace. Its
phases: thin (allocation with a failed first call, base filled and sealed
exactly once, clone and data attach arguments, by-id paths in the domain
XML, re-attach after a simulated host reboot without refilling, two VMs
sharing one base, endpoint refresh after the fake moved an export, five
concurrent creates of one volume name, a delete whose ABE call fails once,
delete releases the clones and keeps the base), legacy (ABE < 0.4: full
copy, no names), crash (ABE dies right after `/seal`), capacity (15 GiB
pool: second 10 GiB VM `Pending`, 10 GiB volume 507) and noabe (`Pending`
with `no abe`, 503). `abe/tests/e2e.sh` (root) round-trips a real volume
through the kernel target on a host.
