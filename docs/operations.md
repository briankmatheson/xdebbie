# Operations

## Install qfodufk8sd

```sh
cargo build --release
sudo install -m755 target/release/{qfodufk8sd,qfodufk8sctl} /usr/local/bin/
sudo apt install libvirt-clients cloud-image-utils     # virsh, cloud-localds
sudo useradd --system --home-dir /var/lib/qfodufk8s --create-home qfodufk8s
sudo -u qfodufk8s ssh-keygen -t ed25519 -N '' -f /var/lib/qfodufk8s/.ssh/id_ed25519
sudo install -d -m750 -o qfodufk8s /etc/qfodufk8s /etc/qfodufk8s/tls
openssl rand -hex 32 | sudo tee /etc/qfodufk8s/token >/dev/null
sudo install -m644 deploy/systemd/qfodufk8sd.service /etc/systemd/system/
sudo systemctl enable --now qfodufk8sd
```

Each host's SSH key must be in the qfodufk8s user's `known_hosts` before the
first connection (`sudo -u qfodufk8s ssh qfodufk8s@srv2 true`).

## Day-2 tasks

| Task | Command |
|---|---|
| Take a host out for maintenance | `qfodufk8sctl host drain srv2` → wait for `vm ls` to show no VMs on it |
| Bring it back | `qfodufk8sctl host uncordon srv2` |
| Add a host | prep it, then `qfodufk8sctl host add srv5 --uri qemu+ssh://qfodufk8s@srv5/system --failure-domain fd-5` |
| Remove a host | drain, then `qfodufk8sctl host rm srv5` |
| Move one VM | `qfodufk8sctl vm migrate demo-md-0-abcde --to srv3` |
| Ship a new base image | `build-image.sh v1.37 && push-image.sh … srv1 srv2 srv3 srv4`, then point the machine templates at it |
| Upgrade Kubernetes | New Qfodufk8sMachineTemplate with the new image; bump `version` on KCP and the MachineDeployment |

For Kubernetes VMs, prefer `kubectl drain` + letting CAPI replace the
Machine over live migration; it exercises the path you rely on in a failure.
Live migration exists for pets and for draining quickly.

## Backup

The whole control state is `/var/lib/qfodufk8s/state.json`. Copy it anywhere
(it is rewritten atomically, so a plain `cp` is consistent). Losing it does
not stop running VMs; it only makes qfodufk8sd forget them.

## Security model

- qfodufk8sd holds an SSH key that can drive libvirt on every host, which is
  root-equivalent there. Treat the qfodufk8sd box accordingly.
- The API is protected by one bearer token. Run it with TLS (`tls_cert`,
  `tls_key`) whenever it listens off loopback; qfodufk8sd warns if you do not.
- Bootstrap data (which contains cluster join tokens) is stored in
  state.json and in a seed volume on the host. Restrict both.
- capq needs `get` on Secrets cluster-wide to read bootstrap data. Scope it
  with `WATCH_NAMESPACE` if you can.

## Limits

- **Single qfodufk8sd.** No HA. If it is down, running VMs are unaffected but
  nothing new happens. Restart it, or restore state.json on another box.
- **Host-local storage.** A dead host's VMs cannot be restarted elsewhere;
  CAPI's MachineHealthCheck replaces them instead. That is the intended model.
- **Live migration needs** identical pool and bridge names, matching CPU
  models (host-passthrough), name resolution between hosts, and TCP
  49152–49215 open between hosts.
- **No graceful-stop timeout.** `stop` sends ACPI shutdown; a guest that
  ignores it stays up. `DELETE` always uses a hard `destroy`.
- **x86_64 only** in the domain template today.
