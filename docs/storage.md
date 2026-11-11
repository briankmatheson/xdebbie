# VM storage: local or ABE

A VM's disks live in one of two places, chosen per VM with `spec.storage`
(`xdctl vm create --storage local|abe`, CAPX `XDebbie8sMachineTemplate
.spec.template.spec.storage`):

| `storage` | root disk | data disk (`dataDiskGib`) | reachable from |
|---|---|---|---|
| `local` (default) | thin LVM snapshot of `base-<image>` or qcow2 overlay in the host's pool | thin LV / raw pool volume | the host that created it |
| `abe` | NVMe/TCP volume exported by the host's `abe` daemon, filled from the base image | NVMe/TCP volume, blank | **any host** that can reach the exporter |

`local` is what xdebbie always did. `abe` is for the case where **every
hypervisor runs an [ABE](https://github.com/brianmatheson/abe) exporter**:
the VM's volumes are created on the ABE of the host it is scheduled on (so
I/O stays local), but they are plain NVMe-oF subsystems, so another host can
attach them later. That is the storage half of live migration without
copying disks; the compute half (attaching on a remote host) is not in this
version yet.

## How it works

1. `[[hosts]].abe = "http://<addr>:<port>"` tells xdebbied where each host's
   ABE API is. Hosts without it cannot run `storage = abe` VMs (the VM stays
   in `Provisioning` with `host X has no ABE`).
2. When an `abe` VM is provisioned the driver asks ABE for a blank volume per
   disk (`GET /configure?bytes=N&raw=true`) and records the reply in the VM:

   ```json
   "disks": [
     {"role": "root", "server": "http://10.22.0.1:80", "traddr": "10.22.0.1",
      "port": "44001", "id": "3f2c…", "bytes": 10737418240, "populated": true},
     {"role": "data", "server": "http://10.22.0.1:80", "traddr": "10.22.0.1",
      "port": "44002", "id": "9a01…", "bytes": 107374182400, "populated": true}
   ]
   ```

   The record is persisted **before** anything else happens, so a failure
   later (or a crash) never allocates a second volume: the next reconcile
   tick picks up where it left off.
3. The host attaches each volume: `nvme connect -t tcp -a <traddr> -s <port>
   -n <id> --ctrl-loss-tmo -1` (retry forever if the exporter is away, so the
   guest stalls instead of taking I/O errors). ABE ≥ 0.2.0 sets the
   namespace's `device_uuid` to the volume id, which makes udev create
   `/dev/disk/by-id/nvme-uuid.<id>` — a path that survives reboots and
   reconnects, unlike `/dev/nvmeXnY`.
4. The root volume is filled once from the host's copy of the base image
   (`/dev/<vg>/base-<image>` with LVM, else the pool's `<image>.qcow2`) using
   `qemu-img convert -n -O raw`; `populated` is then set. Data volumes stay
   blank.
5. The domain is defined with `<disk type='block'>` pointing at the by-id
   paths. The cloud-init seed ISO stays in the local pool.
6. On every `start` (including after a host reboot, when the domain exists
   but the devices do not) the driver re-attaches anything missing first.
7. Deleting the VM disconnects every volume and `DELETE`s it on its ABE; all
   are attempted and the first error makes the reconciler retry.

What is *not* done: capacity accounting (the scheduler still looks at the
libvirt pool), thin clones (every root is a full copy of the base image over
the network — a few GB), migration (refused with a clear message), picking a
different ABE than the scheduled host's.

## Setting up a host

```sh
# the exporter (abe >= 0.2.0), see abe/README.md
sudo apt install ./abe_0.2.0-1_amd64.deb nvme-cli
sudo tee /etc/default/abe <<'EOF'
ABE_VG=timesink-vg            # any VG; a thin pool is recommended
ABE_THIN_POOL=xdebbie-pool
ABE_ADVERTISE=10.22.0.1       # what initiators connect to; the WireGuard
                              # underlay address when the mesh is on
ABE_LISTEN=0.0.0.0:80
EOF
sudo systemctl enable --now abe
sudo modprobe nvme-tcp && echo nvme-tcp | sudo tee /etc/modules-load.d/nvme-tcp.conf

# tell xdebbied
sudo tee -a /etc/xdebbie/xdebbied.toml <<'EOF'   # inside the host's [[hosts]] block
abe = "http://127.0.0.1:80"
EOF
sudo systemctl restart xdebbied
```

Then `xdctl vm create demo --image debian-13 --disk-gib 10 --storage abe`.
`xdctl vm get demo --json | jq .disks` shows the volumes; `nvme list` on the
host shows them attached; `curl -s http://127.0.0.1/volumes` shows them on
the exporter.

### Addresses

`ABE_ADVERTISE` is what ends up in `traddr`, so it must be reachable from
every host that may attach the volume. With the [mesh](mesh.md) on, use the
box's WireGuard underlay address (`10.22.<box>.1`): NVMe/TCP then rides the
encrypted underlay directly instead of the Geneve overlay, and the local
host reaches its own underlay address too. The ABE HTTP API only needs to be
reachable by the xdebbied that schedules VMs onto that host.

The nvmet listener binds `0.0.0.0` (ABE_TRADDR), so the listener does not
depend on `wg-flock` existing at boot. ABE opens one TCP port per volume
(`44001`, `44002`, …) and adds a ufw rule for each when ufw is present.

### Firewall

Hosts with a default-deny input policy need the ABE ports open on the
underlay in addition to the [mesh rules](mesh.md#host-firewalls):

```sh
sudo ufw allow in on wg-flock proto tcp from 10.22.0.0/16 to any port 44000:44999
```

## Configuration reference

```toml
[[hosts]]
name = "timesink"
uri = "qemu:///system"
abe = "http://127.0.0.1:80"     # this host's ABE API

# binaries, overridable for tests
nvme = "nvme"
qemu_img = "qemu-img"
nvme_sys = "/sys/class/nvme"
```

`VmSpec.storage`: `local` | `abe` (default `local`). `Vm.disks[]`: `role`
(`root`|`data`), `server`, `traddr`, `port`, `id` (== NQN), `bytes`,
`populated`.

## Tests

`tests/abe/run.sh` runs the real daemon unprivileged against a fake ABE
(python; volumes are files) and stub `nvme`/`qemu-img`/`virsh`, with a tmpfs
bound over `/dev/disk/by-id` in a user namespace: allocation with a failed
first call, attach arguments, by-id paths in the domain XML, root filled
exactly once, re-attach after a simulated host reboot without refilling,
local VMs untouched, delete releases everything. `abe/tests/e2e.sh` (root)
round-trips a real volume through the kernel target on a host.
