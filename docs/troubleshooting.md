# Troubleshooting

Symptoms, their usual causes, and what to run.

Start with the daemon's own view:

```bash
systemctl status xdebbied
journalctl -u xdebbied -n 100
xdctl version
```

## xdebbied will not start

**`systemctl status` shows exit status 78.** A configuration, state or token
file does not load. The message names the file and the problem, such as an
unknown key or a bad value (`policy = "binpack"` is one: the value is
`"pack"`). systemd does not retry on 78. Fix the file, then:

```bash
sudo systemctl restart xdebbied
```

**Bootstrap fails before serve starts.** The service runs `xdebbied
bootstrap` first. Run it by hand to see its output:

```bash
sudo xdebbied bootstrap
```

- *libvirt is not reachable*: `systemctl status libvirtd` and
  `journalctl -u libvirtd`.
- *a cluster is already running*: the box heard another cluster's
  discovery. Join it (`--join URL`) or start a separate one
  (`--standalone`).

**Permission denied on files in `/etc/xdebbie`.** Run
`sudo xdebbied bootstrap` (without `--force`): it repairs owners and modes
without touching contents.

## A box will not join

| What you see | Cause and fix |
|---|---|
| `--join` says *this box is the CA of a cluster of its own, and that cluster is in use* | The box was used on its own before joining. Move or remove its VMs and volumes, or pass `--force` (see [Join a second box](getting-started/join.md#2-if-the-new-box-is-already-in-use)). |
| `--join` says *this box is enrolled with … already, not …* | It joined another cluster before. `--force` enrols it with the new one. |
| `--join` says *Already enrolled (certificate and token present); reusing* on 0.21 or older | The package's bootstrap made the box a cluster of its own. Upgrade to 0.22, or clear `/etc/xdebbie/xdebbied.toml`, `xdebbie_env`, `token`, `api-*.pem` and the `xdebbie` libvirt network by hand, then join again. |
| The CA fingerprint does not match | You are talking to a different box than you think, or someone is in the middle. Stop and check `xdctl ca` on the first box. |
| `GET /v1/ca` fails, or *is that the box bootstrapped first?* | The URL is wrong, or 23443/tcp is blocked. `curl -k https://box0.example.org:23443/healthz` from the new box should print `ok`. |
| `--join refuses plain http://` | Use the `https://` URL. |
| Waiting forever for approval | Nobody approved it yet. On a member: `xdctl enroll ls`, then `xdctl approve NEW_BOX`. |
| *enrolment was denied* | Re-run with `--force` to ask again with a new key. |
| *enrolment disappeared* | The CA box lost its request list. Re-run with `--force`. |
| Too many pending requests | The CA box keeps at most 64. Deny the stale ones (`xdctl enroll deny ID`). |
| The new host shows in `xdctl host ls` but VMs never start there | The leader reaches the box's libvirt over the overlay. Connect the mesh both ways (`xdctl mesh peer add` on each box). |

## The mesh is degraded

```bash
xdctl mesh ls
xdctl mesh wg
```

| What you see | Cause and fix |
|---|---|
| A peer is `Down` and `xdctl mesh wg` shows no handshake | WireGuard cannot connect. Check each box has the other's public key, the `endpoint` is right, and 51820/udp is reachable. |
| Handshakes work, the peer stays `Down`, `gnv-bN` TX climbs but the far side's RX stays 0 | The far box drops Geneve. `sudo ufw status` there must show `6081/udp on wg-flock ... ALLOW IN`. Re-run `sudo xdebbied bootstrap` on it. |
| Peer reason `UnderlayRouteMismatch` | The route to the peer's underlay address leaves by another interface. Look for an overlapping route to `10.22.0.0/16`. |
| Condition `BridgeMacMismatch` | The bridge MAC is not `52:54:00:23:BOX:fe`, so peers cannot reach this box's gateway address. Fix with the `virsh net-edit` the warning prints. |
| Condition `GeneveModuleMissing` or `WireguardModuleMissing` | `sudo modprobe geneve wireguard`. |
| Condition `NameConflict` | Something else owns a `gnv-bN` link or the `wg-flock` name. xdebbied never touches it; remove it yourself. |
| A peer is `Down` forever and it is a laptop | It was added as a peer, not a client. See [Connect a laptop with xdj](how-to/mesh-clients.md#a-client-that-was-added-as-a-peer). |
| Boxes are `Up` but VMs on different boxes cannot reach each other | In `fdb = "derived"` mode the tunnels carry only known MACs, and VM MACs are not among them. Set `fdb = "learn"` in `[mesh]` on every box. See [the mesh page](mesh.md#host-requirements). |

## A VM gets no address

```bash
xdctl net ls
xdctl net seen
xdctl vm describe VM
```

| What `xdctl net ls` says | Cause and fix |
|---|---|
| `serving` | The responder works. Check `net seen` for the VM's MAC; if it is absent, the VM never asked (look at its console). |
| `NOT LISTENING: bind udp/67` | Another DHCP server holds the port: libvirt's dnsmasq (the network has a `<dhcp>` range; remove it with `virsh net-edit`) or `isc-dhcp-server`. The responder retries every 5 seconds. |
| `NOT LISTENING: bridge … does not exist` | The libvirt network is down: `virsh net-start xdebbie`. |
| `standby (not the leader)` | `leader_only = true` and this box is not the leader. Expected. |
| 404, no `[network]` | The box has no `[network]` table. Add one (see [configuration](reference/configuration.md#network)). |

A MAC in `net seen` that xdebbied did not create is ignored on purpose: the
responder answers only for VMs in its store.

**The VM has an address but no DNS.** `[network] upstream_dns` is empty.
Set it to a resolver and restart xdebbied.

## A VM never finishes booting

`xdctl vm ls` shows `Running`, but the VM never gets an address and
`virsh console VM_DOMAIN` stops after `Booting 'Debian GNU/Linux'`.

The kernel and GRUB are writing to a screen, not to the serial console.
VMs have a serial console and, since 0.8.1, a virtio video device with VNC
on the host's `127.0.0.1`. Two fixes:

- **See the screen.** On the host, `sudo virsh vncdisplay VM_DOMAIN`, then
  connect a VNC viewer through an SSH tunnel.
- **Make the image use the serial console.** Set
  `GRUB_TERMINAL="console serial"` and add `console=ttyS0` to
  `GRUB_CMDLINE_LINUX` in the image's `/etc/default/grub`, then run
  `update-grub` in it (for example with `virt-customize`). Releases before
  0.8.1 had no video device at all, and images using
  `GRUB_TERMINAL_OUTPUT="gfxterm serial"` hung there.

To test an image without a screen:

```bash
qemu-system-x86_64 -enable-kvm -m 1024 -vga none -display none -serial stdio \
    -drive file=IMAGE.qcow2,if=virtio,snapshot=on
```

Plain `-nographic` still adds a VGA device and hides the problem.

## A VM stays Pending

`xdctl vm describe VM` shows why in `message`: each host and the filter it
failed. Common ones:

| Reason | Fix |
|---|---|
| cpu, memory or disk | Free resources, add a host, or raise overcommit. |
| cordoned | `xdctl host uncordon HOST`. |
| unhealthy | The host has not been reached for `host_stale_secs`. Check its libvirt and the mesh. |
| `no abe`, `abe capacity`, `abe pool full`, `abe capacity unknown` | See [Use ABE volumes](how-to/abe-volumes.md#if-it-does-not-work). |
| `pinned elsewhere` | The VM is pinned to a host the leader does not know yet. It registers within seconds of its daemon starting. |

## xdctl cannot connect

| Error | Fix |
|---|---|
| certificate verify failed, UnknownIssuer | `XDEBBIE_CA` is not the cluster CA. `source /etc/xdebbie/xdebbie_env`. |
| 401 | The token is wrong or was rotated. Re-source the env file. |
| 503 and *waiting for a leader election* | An election is running; `xdctl` waits up to 20 seconds. |
| 502 naming the leader | The box you asked cannot reach the leader. |
| Permission denied reading `xdebbie_env` | Join the `xdebbie` group and log in again. |
