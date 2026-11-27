# Security

Who trusts whom, what each credential opens, where the secrets live, and
how to rotate the token.

## What you need

Nothing extra: bootstrap sets up the CA, the certificates and the token.
This page explains them and the one routine job, token rotation.

## One CA per cluster

The first box makes the cluster CA when it bootstraps and keeps its key,
`/etc/xdebbie/api-ca-key.pem`. Every other box gets a certificate signed by
it only after someone approves it with `xdctl approve`. The joining box
makes its own key, which never leaves it.

That one CA is the root of trust for:

- **The API.** HTTPS from the first start. Each box's certificate names its
  hostname, FQDN and every local IPv4 address, and carries `serverAuth`
  and `clientAuth`.
- **Box to box.** Daemons present their certificates to each other (leader
  forwarding, token distribution, enrolment, agent calls). A new box gets
  the cluster token this way, over mutual TLS.
- **libvirt.** Each box's libvirtd listens for TLS on its bridge address
  and accepts client certificates from the cluster CA.
- **abe.** abe 0.5.0 and later reuse the box's certificate and admit only
  cluster certificates.

Check the CA fingerprint when a box joins (`--ca-fingerprint`, or the
prompt). It is the one moment a box trusts something it has not checked.

| Certificate | Lifetime |
|---|---|
| The cluster CA | 10 years |
| A box's certificate | 2 years |
| An xdj client's certificate | 60 minutes; xdj renews it with `POST /v1/enroll/renew` |

There is no revocation: an approved box's certificate stays valid until it
expires. Without the CA key (or a backup of it), no box can join and no
certificate can be renewed ([Backups](../storage/backups.md)).

## What each credential opens

| Credential | Opens |
|---|---|
| the cluster token (`Authorization: Bearer`) | everything |
| a box certificate signed by the cluster CA | everything |
| an xdj client certificate | `GET /v1/mesh`, `GET /v1/ca`, `POST /v1/enroll/renew`, and the public routes |
| the `settings_token_file` token | `/v1/settings` only |
| the `enroll_token_file` token | listing, reading, approving and denying enrolments only (it sees client requests only) |
| the `schedule_token_file` token | `POST /v1/schedule` only |
| nothing | `GET /healthz`, `GET /v1/version`, `GET /v1/ca`, `GET /v1/cert`, `POST /v1/enroll`, `GET /v1/enroll/ID`, `GET /v1/client-config`, `GET /v1/client-config/xdj` |

A narrow token used anywhere else gets 403; no credential gets 401.
Without `token_file` the API has no authentication at all: lab use only.
Bootstrap always writes one.

The token is 32 random bytes, shared by every box, so treat every box as
holding it. `xdctl --insecure` (or `XDEBBIE_INSECURE=1`) skips certificate
checks, for a lab.

## Secrets on disk

| File | Mode | Who can read it |
|---|---|---|
| `token`, `token.previous`, `api-key.pem`, `api-ca-key.pem` | root:root 0400 | root |
| `xdebbie_env` (holds the token), `xdebbied.toml` | root:xdebbie 0440 | root and the `xdebbie` group |
| `/var/lib/xdebbie/wg.key` | 0600 | root |

Membership of the `xdebbie` group is full control of the cluster: it can
read the token.

cloud-init user-data, which for Kubernetes nodes holds join tokens, is
kept in `state.json` and in each VM's seed image. Restrict both.

## Network exposure

- From the LAN a box accepts only SSH, WireGuard, the API and (with
  discovery on) the discovery port. libvirt TLS, Geneve and DHCP are
  accepted on the mesh's own interfaces only
  ([Routes and firewall](../network/routes-and-firewall.md)).
- Between boxes, VM traffic rides Geneve inside WireGuard, so it is
  encrypted, and Geneve arriving anywhere but the WireGuard link is
  dropped.
- VMs reach the internet through their host and each other across the
  mesh. LAN hosts cannot open connections to VMs, even with a route;
  `mesh_routed` opens them to the WireGuard underlay only.
- Discovery messages are signed with a key derived from the token; unsigned
  or stale ones are dropped.

## What runs as root

xdebbied runs as root: it drives libvirt, LVM, nftables, WireGuard and ufw.
systemd keeps `/usr`, `/boot` and the rest of `/etc` read-only for it
(`ProtectSystem=full`), and the unit lists every path it may write. A box's
daemon can drive libvirt on every other box. Root on any box is root on
the cluster.

capx holds the cluster token and reads bootstrap Secrets. Limit it to one
namespace with `WATCH_NAMESPACE` if you can.

## Rotate the token

All boxes share one token. Rotation makes a new one on the leader and hands
it to every box it knows; the old one keeps working until every box has
the new one, then it is retired everywhere, followers first, leader last.

The leader knows the boxes through discovery. With discovery off it knows
none, so a rotation changes only the leader's token. Turn discovery on
first, or copy the token by hand as below.

1. Rotate:

   ```bash
   xdctl token rotate
   ```

   It prints the new token. This is the only time anything shows it.

2. Watch it spread:

   ```bash
   xdctl token status
   ```

3. Update what holds the token: re-run `source /etc/xdebbie/xdebbie_env` in
   open shells (each box rewrites `export XDEBBIE_TOKEN=` there as it
   switches), update capx's Secret, and any other client.

`xdctl token status` shows, for each box:

| State | Meaning |
|---|---|
| `current` | has the new token; the old one is retired there |
| `overlap` | accepts both |
| `installing` | the leader is handing it the new token |
| `stale` | rejects every token the leader holds; fix it by hand |
| `unreachable: ...` | the leader cannot reach it |

While one rotation is spreading, another is refused (409). Tokens are shown
by fingerprint: the first 12 hex digits of their SHA-256. If the leader
restarts mid-rotation, it resumes from `/etc/xdebbie/token.previous`.

**A box that missed it** ends up `stale`. Copy `/etc/xdebbie/token` from
the leader to the same path on that box (root:root, mode 0400), then
restart xdebbied there.

## Reporting a vulnerability

See [SECURITY.md](https://github.com/briankmatheson/xdebbie/blob/main/SECURITY.md).

## If it goes wrong

- If `xdctl` says *certificate verify failed* or `UnknownIssuer`, then
  `XDEBBIE_CA` is not the cluster CA: `source /etc/xdebbie/xdebbie_env`.
- If it answers 401, then the token is wrong or was rotated: source the env
  file again.
- If it answers 403 with *the settings token opens /v1/settings only* (or
  the like), then you are using a narrow token for a wider job.

## Reference

[`token_file` and the narrow tokens](../reference/configuration.md#top-level-keys),
[`/v1/token`, `/v1/enroll`](../reference/api.md#token),
[`xdctl token`, `xdctl ca`](../reference/xdctl.md#token).
