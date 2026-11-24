# Security model

What each part of XDebbie trusts, what an attacker on the LAN or in a VM
can reach, and where the secrets live.

## Trust: one CA per cluster

The first box creates the cluster CA when it bootstraps, and keeps its
private key (`/etc/xdebbie/api-ca-key.pem`). Every other box gets a
certificate signed by it only after an operator approves the request with
`xdctl approve`. The joining box makes its own key, which never leaves it.

That one CA is the root of trust for:

- **The API.** HTTPS from the first start. Every box's certificate carries
  `serverAuth` and `clientAuth`.
- **Box to box.** Daemons authenticate to each other with their
  certificates (leader forwarding, token distribution, enrolment, agent
  calls). A box gets the cluster token itself this way, over mutual TLS.
- **libvirt.** Each box's libvirtd listens for TLS on its overlay bridge
  address and accepts client certificates signed by the cluster CA.
- **ABE.** ABE 0.5.0 and later reuses the box's certificate and admits only
  cluster certificates.

Check the CA fingerprint when a box joins (`--ca-fingerprint`, or the
prompt). It is the only moment a box trusts something it has not verified.

## Authentication on the API

A request is accepted if it carries one of:

| Credential | Opens |
|---|---|
| The cluster token (`Authorization: Bearer`) | everything |
| A box certificate signed by the cluster CA | everything |
| An xdj client certificate | `GET /v1/mesh`, `GET /v1/ca`, `POST /v1/enroll/renew` |
| The `settings_token_file` token | `/v1/settings` only |
| The `enroll_token_file` token | listing and deciding xdj client enrolments only |
| The `schedule_token_file` token | `POST /v1/schedule` only |
| nothing | `GET /healthz`, `GET /v1/version`, `GET /v1/ca`, `GET /v1/cert`, `POST /v1/enroll`, `GET /v1/enroll/ID`, `GET /v1/client-config` |

Without `token_file` the API has no authentication at all. That is for a
lab only; bootstrap always writes one.

The token is 32 random bytes. It is shared by every box in a cluster, so
treat every box as holding it. Rotate it with `xdctl token rotate`
([how-to](how-to/rotate-token.md)).

## Secrets on disk

| File | Mode | Who can read it |
|---|---|---|
| `token`, `token.previous`, `api-key.pem`, `api-ca-key.pem` | root:root 0400 | root |
| `xdebbie_env` (holds the token), `xdebbied.toml` | root:xdebbie 0440 | root and the `xdebbie` group |
| `/var/lib/xdebbie/wg.key` | 0600 | root |

Members of the `xdebbie` group can read the token: membership is full
control of the cluster. Secrets are written to a temporary file with the
final mode and renamed into place, so they are never world-readable.

Cloud-init user-data, which for Kubernetes nodes holds join tokens, is
stored in `state.json` and in a seed image in the host's pool. Restrict
both.

## Network exposure

Bootstrap runs ufw with default deny inbound. From the LAN, a box accepts
only SSH, WireGuard, the API and (with discovery on) the discovery port.
libvirt TLS, Geneve and DHCP are accepted only on the mesh's own
interfaces. See [Files, ports and interfaces](reference/files-and-ports.md).

- **Between boxes**, VM traffic rides Geneve inside WireGuard, so it is
  encrypted. An nftables table drops Geneve arriving on any interface but
  the WireGuard link.
- **VMs** can reach the internet through their host (masqueraded) and each
  other across the mesh. Hosts on the LAN cannot open connections to VMs,
  even with a route to the overlay. `mesh_routed` opens them to the
  WireGuard underlay only.
- **Discovery** messages are signed with a key derived from the cluster
  token. Anyone on the LAN can send to the group, but unsigned or stale
  messages are dropped. Boxes need clocks within 30 seconds of each other.

## What runs as root

xdebbied runs as root. It drives libvirt, LVM, nftables, WireGuard and ufw.
systemd keeps `/usr`, `/boot` and most of `/etc` read-only for it
(`ProtectSystem=full`), and the unit lists every path it may write.

A box's daemon can drive libvirt on every other box (through their
daemons, or libvirt TLS). Root on any box is root on the cluster.

## capx

capx, the Cluster API provider, holds the cluster token and reads bootstrap
Secrets. Grant it `get` on Secrets, and limit it to one namespace with
`WATCH_NAMESPACE` if you can.

## Reporting a vulnerability

See [SECURITY.md](https://github.com/briankmatheson/xdebbie/blob/main/SECURITY.md).
