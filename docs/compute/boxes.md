# Boxes: bootstrap, join, enrol, approve

How a stock Debian 13 machine becomes a box, how a box joins a cluster,
and how to upgrade, re-join or start over.

`xdebbied bootstrap` sets a box up. The package runs it on install and on
every upgrade, and you can run it again by hand: it creates only what is
missing. The service does not run it before starting. `xdebbied serve`
instead checks, read-only, what bootstrap should have left, and for each
gap logs what is missing and the command that fixes it:

- a missing API certificate, key or token file stops it with exit status
  78, which systemd does not retry;
- an LVM volume group that is not there yet (a disk still being scanned at
  boot) exits 1, and systemd restarts it in 5 seconds;
- libvirt not answering, no libvirt TLS certificate, or no `xdebbie_env`
  turns only that part off.

## What you need

- Debian 13, amd64, root.
- For joining: the URL of a box in the cluster and its CA fingerprint.

## What bootstrap does

1. **Dependencies.** Checks for `virsh`, `qemu-system-x86_64`, `qemu-img`
   and `cloud-localds`; as root it installs what is missing:
   libvirt-daemon-system, libvirt-clients, qemu-kvm, qemu-utils,
   cloud-image-utils, lvm2, thin-provisioning-tools, ufw.
2. **libvirtd.** Starts and enables it.
3. **Existing resources.** Lists libvirt networks and pools, and uses the
   ones it is told to (`--network`, `--pool`).
4. **Identity.** Without `--join` or `--standalone` it first listens 20
   seconds (`--discover-secs`) on the discovery group for a running
   cluster. At a terminal, it offers what it heard (`Join it via URL?
   [Y/n/other URL]`); hearing nothing, it asks for a URL, and Enter means a
   new cluster. Without a terminal, as in the package's first install, a
   cluster heard is an error telling you to re-run with `--join`; silence
   means a new cluster. A box that holds the CA key or has `ca_url` never
   asks.
   - **A new cluster**: makes the token, the cluster CA and this box's
     certificate; this is box 0.
   - **Joining**: enrols (below), then copies the overlay settings from the
     cluster.
5. **Resources.** Creates the libvirt network `xdebbie` if missing (bridge
   `virbr-xdebbie`, MAC `52:54:00:23:BOX:fe`, this box's bridge address,
   route forwarding, no libvirt DHCP) and the storage pool `xdebbie` (a
   directory at `/var/lib/libvirt/xdebbie`, mode 0711). With a storage
   bridge set, also the isolated network `abe`. For an existing network it
   warns when the MAC is wrong.
6. **Configuration.** Writes `/etc/xdebbie/xdebbied.toml`, with a comment
   on every setting, if there is none. An existing one is kept; bootstrap
   only sets `[mesh] box` and `ca_url` and adds a `[network]` copied from
   the cluster if there is none.
7. **libvirt TLS.** Copies the cluster CA and this box's certificate and
   key into libvirt's places, makes libvirtd listen for TLS on the bridge
   address with client certificates checked against the cluster CA, and
   restarts libvirtd when anything moved. It warns when the certificate has
   no name for that address.
8. **Firewall.** ufw and the `ip xdebbie_host` table, from the config
   ([Routes and firewall](../network/routes-and-firewall.md)).
9. **Register** (joining only): `PUT /v1/hosts/FQDN` at the CA box, with
   `uri = qemu+tls://BRIDGE_ADDRESS/system`.

Run it again at any time:

```bash
sudo xdebbied bootstrap
```

## Files it writes

| File | Owner, mode | What |
|---|---|---|
| `/etc/xdebbie/xdebbied.toml` | root:xdebbie 0440 | [the configuration](../reference/configuration.md) |
| `/etc/xdebbie/token` | root:root 0400 | the cluster token: 64 hex characters (32 random bytes) |
| `/etc/xdebbie/xdebbie_env` | root:xdebbie 0440 | `XDEBBIE_URL`, `XDEBBIE_TOKEN`, `XDEBBIE_CA` for `xdctl` |
| `/etc/xdebbie/api-ca.pem` | root:xdebbie 0440 | the cluster CA certificate |
| `/etc/xdebbie/api-ca-key.pem` | root:root 0400 | the CA key, on the first box only |
| `/etc/xdebbie/api-cert.pem` | root:xdebbie 0440 | this box's certificate |
| `/etc/xdebbie/api-key.pem` | root:root 0400 | this box's key; it never leaves the box |

Secrets are written to a temporary file with their final mode and renamed
into place, so none is ever world-readable. `XDEBBIE_URL` in the env file
is `advertise_url` (the hostname, never `0.0.0.0`), so the same file works
from another machine. Bootstrap creates the `xdebbie` group; its members
can source the env file and run `xdctl` without sudo.

The libvirt network, as created for box 1:

```xml
<network>
  <name>xdebbie</name>
  <forward mode='route'/>
  <bridge name='virbr-xdebbie' stp='off' delay='0'/>
  <mac address='52:54:00:23:01:fe'/>
  <dns enable='no'/>
  <ip address='10.23.1.1' netmask='255.255.0.0'>
  </ip>
</network>
```

## Join a cluster: enrolment and approval

The walk-through is [Join a second box](../getting-started/second-box.md).
What happens:

1. On the new box, `sudo xdebbied bootstrap --join URL
   [--ca-fingerprint sha256:HEX]`. A missing `https://` and port 23443 are
   filled in; plain `http://` is refused. Bootstrap fetches the CA
   (`GET /v1/ca`), checks it against the fingerprint or asks
   `Trust this CA ...? [y/N]`, makes a key and a certificate request,
   sends it (`POST /v1/enroll`), prints the request's key fingerprint, and
   polls every 3 seconds, with a reminder every minute.
2. On any member, `xdctl enroll ls` shows the request, and
   `xdctl approve HOST` (the same as `xdctl enroll approve`, by hostname or
   id) signs it. Members forward these calls to the CA box (`ca_url`).
3. Approval assigns the box id: the lowest free one from 0 to 253, counting
   the CA box's own, its peers and clients, and every pending or approved
   enrolment. `--box N` asks for a particular one; it is a wish, granted if
   free. An approved box keeps its id for good; a box that re-enrols gets
   the one it had.
4. The new box picks up the certificate, installs `api-cert.pem` and
   `api-ca.pem`, writes its box id to `[mesh]`, creates its bridge with the
   matching MAC, fetches the cluster token over mutual TLS
   (`GET /v1/enroll/token`, client certificate only), and copies
   `[network]` from the CA box (`GET /v1/network`) with its own `/24`. Its
   config gets `ca_url`.
5. Bootstrap finishes and registers the host. Start the service and peer
   the mesh both ways.

`xdctl enroll deny HOST` refuses a pending request; the box can ask again
with `--join --force`. An approved box cannot be denied afterwards: its
certificate stays valid (there is no revocation). Re-running
`bootstrap --join` resumes a pending enrolment (kept in
`/var/lib/xdebbie/join.json`); `--force` starts over with a new key. The CA
box keeps requests in `/var/lib/xdebbie/enroll.json`, at most 64 pending.

### A box that was a cluster of its own

The package makes every new box a cluster of its own first. `--join` on a
box whose cluster is still empty (no VMs, volumes, other boxes or mesh
peers) moves that identity aside, as `FILE.own-cluster-STAMP`, and enrols.
A box whose cluster is in use is refused, naming what is in it; a box
enrolled with another CA is refused too. `--force` goes ahead anyway.

## Start over on a box

To join a box again from scratch, for instance one set up under another
box id. Only on a box whose VMs and volumes you do not need:
`sudo virsh list --all` must be empty.

```bash
sudo systemctl stop xdebbied
sudo ip link del wg-flock 2>/dev/null
for l in $(ip -o link | grep -o 'gnv-b[0-9]*'); do sudo ip link del "$l"; done
sudo nft delete table inet xdebbie_mesh 2>/dev/null
sudo virsh net-destroy xdebbie 2>/dev/null; sudo virsh net-undefine xdebbie
sudo virsh pool-destroy xdebbie 2>/dev/null; sudo virsh pool-undefine xdebbie
sudo rm -rf /etc/xdebbie /var/lib/xdebbie
```

That removes the config, token, certificates, mesh state and WireGuard key
(the box gets a new one). On the boxes that had it as a peer, remove it:
`xdctl mesh peer rm BOX`. Then join again. Bootstrap moves libvirtd's
`listen_addr` to the new bridge address by itself.

## Regenerate the configuration

```bash
sudo xdebbied bootstrap --force
```

The old file is kept as `xdebbied.toml.bak-TIMESTAMP`. These are carried
into the new one: `volume_group`, `thin_pool`, `tls_cert`, `tls_key`,
`advertise_url`, `ca_url`, `policy`, `reconcile_interval_secs`,
`host_stale_secs`, and the `[network]`, `[mesh]` and `[[hosts]]` tables.
Anything else you added must be added again. On the CA box, `--force`
reissues the box's certificate but keeps the CA. A config that exists but
does not parse is never replaced without `--force`. The certificate lists
the box's IPv4 addresses when it is made; after an address change,
`--force` reissues it.

## Upgrade

Running VMs are not touched: they are QEMU processes under libvirt, not
under xdebbied. While a daemon restarts, new work waits.

1. Read [CHANGELOG.md](https://github.com/briankmatheson/xdebbie/blob/main/CHANGELOG.md)
   for changes in behaviour between your version and the new one.
2. Upgrade the followers first and the leader last (`xdctl leader`), so
   leadership moves at most once. On each box:

   ```bash
   sudo apt update
   sudo apt install xdebbied
   ```

   The package runs bootstrap, which adds what the new release needs and
   keeps your configuration, then restarts the service.

3. Check:

   ```bash
   systemctl status xdebbied
   xdctl version
   ```

`xdctl version` shows xdctl's version, the daemon's on the box you asked,
and the leader's when that is another box. Boxes on different releases
work together during an upgrade. Daemons from before signed discovery send
unsigned messages: while any remain, set `discovery_accept_unsigned = true`
on the upgraded ones, and remove it when the last is done.

## The package

One package, `xdebbied`, carries the daemon and `xdctl`. Its scripts:

- **postinst** creates the `xdebbie` group, `/etc/xdebbie` (root:xdebbie
  0750) and `/var/lib/xdebbie`, sets file modes, loads `geneve` and
  `wireguard`, adds an AppArmor rule for libvirt's `virt-aa-helper`, and
  runs `xdebbied bootstrap`: on first install, then prints what to do next;
  on upgrade, reporting a failure without failing the install. The service
  is then enabled and started (restarted on upgrade).
- **prerm** does nothing of its own; the daemon keeps running while files
  unpack.
- **postrm purge** removes `/etc/xdebbie` and `/var/lib/xdebbie` (this
  box's config, token, certificates, keys and state) and the AppArmor rule.
  VM disks and the `xdebbie` group stay.

The unit runs `xdebbied serve` as root with `ProtectSystem=full`,
`Restart=on-failure` after 5 seconds, except exit 78. What it installs:
[Files](../reference/files-and-ports.md#the-package).

## If it goes wrong

- If `serve` exits 78, then a config, state, token or certificate file
  does not load; `systemctl status xdebbied` names it.
- If files in `/etc/xdebbie` give *Permission denied*, then run
  `sudo xdebbied bootstrap` (without `--force`): it repairs owners and
  modes without touching contents.
- If a libvirt network with the same bridge already exists, then use it
  (`--network NAME`), pick another name, or remove the conflicting bridge.
- More: [A box will not join](../reference/troubleshooting.md#a-box-will-not-join).

## Reference

[`xdebbied bootstrap`](../reference/xdebbied.md#xdebbied-bootstrap),
[`xdctl enroll`](../reference/xdctl.md#boxes-and-the-ca),
[`/v1/enroll`](../reference/api.md#enrolment),
[Files](../reference/files-and-ports.md).
