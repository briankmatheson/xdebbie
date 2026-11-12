# Operations

## Install xdebbied

```sh
cargo build --release
sudo install -m755 target/release/{xdebbied,xdctl} /usr/local/bin/
sudo apt install libvirt-clients cloud-image-utils     # virsh, cloud-localds
sudo useradd --system --home-dir /var/lib/xdebbie --create-home xdebbie
sudo -u xdebbie ssh-keygen -t ed25519 -N '' -f /var/lib/xdebbie/.ssh/id_ed25519
sudo install -d -m750 -o xdebbie /etc/xdebbie /etc/xdebbie/tls
openssl rand -hex 32 | sudo tee /etc/xdebbie/token >/dev/null
sudo install -m644 deploy/systemd/xdebbied.service /etc/systemd/system/
sudo systemctl enable --now xdebbied
```

Each host's SSH key must be in the xdebbie user's `known_hosts` before the
first connection (`sudo -u xdebbie ssh xdebbie@srv2 true`).

## Day-2 tasks

| Task | Command |
|---|---|
| Take a host out for maintenance | `xdctl host drain srv2` → wait for `vm ls` to show no VMs on it |
| Bring it back | `xdctl host uncordon srv2` |
| Add a host | prep it, then `xdctl host add srv5 --uri qemu+ssh://xdebbie@srv5/system --failure-domain fd-5` |
| Remove a host | drain, then `xdctl host rm srv5` |
| Move one VM | `xdctl vm migrate demo-md-0-abcde --to srv3` |
| Ship a new base image | `build-image.sh v1.37 && push-image.sh … srv1 srv2 srv3 srv4`, then point the machine templates at it |
| Upgrade Kubernetes | New XDebbie8sMachineTemplate with the new image; bump `version` on KCP and the MachineDeployment |

When xdebbied will not start, `systemctl status xdebbied` shows it: a
config, state or token file that does not load exits with status 78
(`EX_CONFIG`) and systemd does not retry, so the error stays put. Fix the
file and `systemctl restart xdebbied`. `xdebbied bootstrap --force`
regenerates the config and token; it backs both up as `.bak-<timestamp>`
and carries `volume_group`, `thin_pool`, `tls_*`, `advertise_url`,
`policy`, `[network]`, `[mesh]` and `[[hosts]]` into the new file.

For Kubernetes VMs, prefer `kubectl drain` + letting CAPI replace the
Machine over live migration; it exercises the path you rely on in a failure.
Live migration exists for pets and for draining quickly.

## Backup

The whole control state is `/var/lib/xdebbie/state.json`. Copy it anywhere
(it is rewritten atomically, so a plain `cp` is consistent). Losing it does
not stop running VMs; it only makes xdebbied forget them.

## Security model

- xdebbied holds an SSH key that can drive libvirt on every host, which is
  root-equivalent there. Treat the xdebbied box accordingly.
- The API is protected by one bearer token, shared by every box in a
  cluster. On disk it lives in `/etc/xdebbie/token` and `xdebbie_env`, both
  root:root 0400 (written atomically with that mode); source the env file
  from a root shell or keep a private copy. Run it with TLS (`tls_cert`, `tls_key`) whenever it listens off
  loopback; xdebbied warns if you do not.
- Rotate it with `xdctl token rotate`. The leader mints a new token, keeps
  accepting the old one, and pushes the pair to every peer; once each peer
  has authenticated a request with the new token the old one is retired
  everywhere, peers first. `xdctl token status` shows where each peer
  stands (`current`, `overlap`, `installing`, `stale`, `unreachable`).
  Each box rewrites `export XDEBBIE_TOKEN=` in its `/etc/xdebbie/xdebbie_env`
  as it switches; re-source it, and update other holders (the abe-csi
  secret, CAPX). A box that is down for the whole rotation ends up `stale`
  and must be given the current token by hand (`PUT /v1/token`, or copy
  `/etc/xdebbie/token` from the leader and restart). A leader restart
  resumes an unfinished rotation from `/etc/xdebbie/token.previous`.
- Bootstrap data (which contains cluster join tokens) is stored in
  state.json and in a seed volume on the host. Restrict both.
- capq needs `get` on Secrets cluster-wide to read bootstrap data. Scope it
  with `WATCH_NAMESPACE` if you can.

## Limits

- **Single xdebbied.** No HA. If it is down, running VMs are unaffected but
  nothing new happens. Restart it, or restore state.json on another box.
- **Host-local storage.** A dead host's VMs cannot be restarted elsewhere;
  CAPI's MachineHealthCheck replaces them instead. That is the intended model.
- **Live migration needs** identical pool and bridge names, matching CPU
  models (host-passthrough), name resolution between hosts, and TCP
  49152–49215 open between hosts.
- **No graceful-stop timeout.** `stop` sends ACPI shutdown; a guest that
  ignores it stays up. `DELETE` always uses a hard `destroy`.
- **x86_64 only** in the domain template today.
