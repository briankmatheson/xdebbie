# Troubleshooting

Symptoms, their usual causes, and what to run, by what you see.

Start with the daemon's own view:

```bash
systemctl status xdebbied
journalctl -u xdebbied -n 100
xdctl version
```

Log detail follows `RUST_LOG` (default `info`); `RUST_LOG=debug` in a
drop-in for the unit gives more.

## xdebbied will not start

- If `systemctl status` shows exit status 78, then a config, state, token
  or certificate file does not load. The message names the file and the
  problem, such as an unknown key or a bad value (`policy = "binpack"` is
  one: the value is `"pack"`). systemd does not retry 78. Fix the file,
  then `sudo systemctl restart xdebbied`.
- If it says *does not exist: this box was never bootstrapped*, then run
  `sudo xdebbied bootstrap`.
- If it exits 1 and restarts every 5 seconds, then something is not there
  *yet*, usually the LVM volume group at boot. The log says which.
- If it runs but logs that a part is off (libvirt not answering, no libvirt
  TLS certificate, no `xdebbie_env`), then the log line names the command
  that fixes it, usually `sudo xdebbied bootstrap`.
- If bootstrap itself fails, then run it by hand to see its output. The
  service never runs it; the package does, on install and upgrade.

  ```bash
  sudo xdebbied bootstrap
  ```

  *libvirt is not reachable*: `systemctl status libvirtd`. *A cluster is
  already running*: the box heard another cluster; join it (`--join URL`)
  or start a separate one (`--standalone`).
- If files in `/etc/xdebbie` give *Permission denied*, then
  `sudo xdebbied bootstrap` (without `--force`) repairs owners and modes
  without touching contents.

## A box will not join

| What you see | Cause and fix |
|---|---|
| *this box is the CA of a cluster of its own, and that cluster is in use* | the box was used on its own first. Move or remove its VMs and volumes, or pass `--force` ([A box that was a cluster of its own](../compute/boxes.md#a-box-that-was-a-cluster-of-its-own)) |
| *this box is enrolled with ... already, not ...* | it joined another cluster before; `--force` enrols it with this one |
| *Already enrolled (certificate and token present); reusing*, on 0.21 or older | the package made the box a cluster of its own; upgrade to 0.22 or later, or [start over](../compute/boxes.md#start-over-on-a-box) |
| the CA fingerprint does not match | you are talking to another box than you think, or someone is in the middle. Stop and check `xdctl ca` on the first box |
| `GET /v1/ca` fails, or *is that the box bootstrapped first?* | wrong URL, or 23443/tcp blocked. `curl -k https://box0.example.org:23443/healthz` from the new box should print `ok` |
| *--join must be an https:// URL* | use `https://` |
| waiting for ever | nobody approved it: `xdctl enroll ls`, then `xdctl approve NEW_BOX` on a member |
| *enrolment was denied* | re-run with `--force` to ask again with a new key |
| *enrolment ... disappeared* | the CA box lost its request list; re-run with `--force` |
| too many pending requests | the CA box keeps 64; deny stale ones (`xdctl enroll deny ID`) |
| the host is listed but VMs never start there | the leader reaches its libvirt over the overlay: peer the mesh both ways |

## The mesh is degraded

```bash
xdctl mesh ls
xdctl mesh wg
```

| What you see | Cause and fix |
|---|---|
| a peer `Down`, no handshake in `mesh wg` | WireGuard cannot connect: each box needs the other's key, the right `endpoint`, and 51820/udp open |
| handshakes work, the peer stays `Down`, `gnv-bN` TX climbs and the far RX stays 0 | the far box drops Geneve: `sudo ufw status` there must show `6081/udp on wg-flock ALLOW IN`; run `sudo xdebbied bootstrap` there |
| peer reason `UnderlayRouteMismatch` | the route to the peer's underlay address leaves by another interface; look for an overlapping route to `10.22.0.0/16` |
| `BridgeAddressMismatch` | the bridge has another box's overlay address; [start over on that box](../compute/boxes.md#start-over-on-a-box) |
| `BridgeMacMismatch` | the bridge MAC is not `52:54:00:23:BOX:fe`: `virsh net-edit xdebbie` to fix the `<mac>`, and `ip link set virbr-xdebbie address MAC` to apply it now |
| `GeneveModuleMissing`, `WireguardModuleMissing` | `sudo modprobe geneve wireguard` |
| `NameConflict` | something else owns a `gnv-bN` or the `wg-flock` name; xdebbied never touches it, remove it yourself |
| a peer `Down` for ever, and it is a laptop | it was added as a peer, not a client ([A laptop on the mesh](../getting-started/laptop.md#if-it-goes-wrong)) |
| peers `Up`, but VMs on different boxes cannot reach each other | `fdb = "derived"` does not carry VM MACs: set `fdb = "learn"` in `[mesh]` on every box |

## A VM gets no address

```bash
xdctl net ls
xdctl net seen
xdctl vm describe VM
```

| `xdctl net ls` says | Cause and fix |
|---|---|
| `serving` | the responder works. Look for the VM's MAC in `net seen`; if absent, the VM never asked (look at its console) |
| `NOT LISTENING: bind udp/67` | another DHCP server holds the port: libvirt's dnsmasq (remove the `<dhcp>` range with `virsh net-edit`) or `isc-dhcp-server`. Retried every 5 seconds |
| `NOT LISTENING: bridge ... does not exist` | the libvirt network is down: `sudo virsh net-start xdebbie` |
| `standby (not the leader)` | `leader_only = true` and this box is not the leader. Expected |
| 404, no `[network]` | the box has no `[network]` table ([`[network]`](configuration.md#network)) |

A MAC in `net seen` that xdebbied did not create is ignored on purpose.

If the VM has an address but no DNS, then `[network] upstream_dns` is
empty: set it and run `sudo xdebbied bootstrap`.

## A VM never finishes booting

`xdctl vm ls` says `Running`, the VM never gets an address, and
`sudo virsh console VM` stops after `Booting 'Debian GNU/Linux'`.

The kernel and GRUB are writing to a screen, not the serial console. VMs
have a serial console and, since 0.8.1, a virtio video device with VNC on
the host's `127.0.0.1`. Two fixes:

- **Look at the screen.** On the host, `sudo virsh vncdisplay VM`, then a
  VNC viewer through an SSH tunnel.
- **Make the image use the serial console.** In the image's
  `/etc/default/grub` set `GRUB_TERMINAL="console serial"` and add
  `console=ttyS0` to `GRUB_CMDLINE_LINUX`, then run `update-grub` in it
  (with `virt-customize`, for example).

To test an image without a screen:

```bash
qemu-system-x86_64 -enable-kvm -m 1024 -vga none -display none -serial stdio \
    -drive file=IMAGE.qcow2,if=virtio,snapshot=on
```

Plain `-nographic` still adds a VGA device and hides the problem.

## A VM stays Pending

`xdctl vm describe VM` shows why in `message`: each filter and how many
hosts failed it.

| Reason | Fix |
|---|---|
| `cpu`, `memory`, `disk` | free room, add a host, or raise the overcommit |
| `cordoned` | `xdctl host uncordon HOST` |
| `unhealthy` | the leader has not reached the host for `host_stale_secs`; check its libvirt and the mesh |
| `capacity unknown` | the host has not been read yet; wait a pass, then check `xdctl host describe HOST` |
| `no abe`, `abe capacity`, `abe pool full`, `abe capacity unknown` | [abe-backed disks](../storage/abe.md#if-it-goes-wrong) |
| `pinned elsewhere` | pinned to a host the leader does not know yet; a box registers within seconds of its daemon starting |
| `failure domain`, `labels`, `not control-plane eligible`, `strict anti-affinity` | the VM's placement rules exclude every host ([Scheduling](../compute/scheduling.md#filters)) |

## A host says Ready but nothing runs there

`STATUS Ready` in `xdctl host ls` means the host has been reached at least
once, not that it answers now. `xdctl host describe HOST` shows `lastSeen`;
older than `host_stale_secs` (60 seconds) means unhealthy, and the
scheduler skips it.

## xdctl cannot connect

| Error | Fix |
|---|---|
| certificate verify failed, `UnknownIssuer` | `XDEBBIE_CA` is not the cluster CA: `source /etc/xdebbie/xdebbie_env` |
| 401 | the token is wrong or was rotated: source the env file again |
| 403 *the ... token opens ... only* | a narrow token used for a wider job |
| *waiting for a leader election* | an election is running; `xdctl` waits up to 20 seconds |
| 502 naming the leader | the box you asked cannot reach the leader |
| *Permission denied* reading `xdebbie_env` | join the `xdebbie` group and log in again |

## Reference

[Configuration](configuration.md), [Files and ports](files-and-ports.md),
[Limits](limits.md).
