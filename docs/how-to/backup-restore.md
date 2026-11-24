# Back up and restore a box's state

Save what xdebbied knows, and put it back after a disk failure or a
reinstall.

## What to back up

| Path | Why |
|---|---|
| `/var/lib/xdebbie/state.json` | Hosts, VMs and volumes. Without it, xdebbied forgets the VMs (they keep running). |
| `/var/lib/xdebbie/*.json` | Mesh spec, settings, client config, enrolments. |
| `/var/lib/xdebbie/wg.key` | The box's WireGuard identity. Without it, every peer must be given a new key. |
| `/etc/xdebbie/` | Config, token, certificates. On the first box, also the **cluster CA key**, `api-ca-key.pem`. |

The CA key is the most important file in the cluster: without it no box can
join, and no certificate can be renewed. Keep its copy apart from the other
backups, encrypted, on a different device.

VM disks are not in these paths. Back them up from inside the VMs, or as
LVM snapshots on the host.

## Make a backup

`state.json` is rewritten atomically (temporary file, `fsync`, rename), so a
plain copy is consistent:

```bash
sudo tar -czf xdebbie-BOX-$(date +%F).tar.gz /var/lib/xdebbie /etc/xdebbie
```

Store the archive somewhere else: it holds the token and private keys.

Every follower also mirrors the leader's `state.json` every 5 seconds, so
another box usually has a recent copy of the cluster's records.

## Restore a box

On a reinstalled box with the same hostname, install the package, then
replace what its first bootstrap created with the saved files. Remove the
fresh files first, so no stray CA key or libvirt network from the new
install stays behind:

```bash
sudo apt install xdebbied
sudo systemctl stop xdebbied
sudo virsh net-destroy xdebbie
sudo virsh net-undefine xdebbie
sudo rm -rf /etc/xdebbie/* /var/lib/xdebbie/*
sudo tar -xzf xdebbie-BOX-DATE.tar.gz -C /
sudo systemctl start xdebbied
```

Bootstrap at service start recreates the libvirt network for the restored
box id, and fixes the files' owners and modes.

## Restore only the VM records

To roll back `state.json` alone:

```bash
sudo systemctl stop xdebbied
sudo cp STATE_BACKUP.json /var/lib/xdebbie/state.json
sudo systemctl start xdebbied
```

The reconciler then compares the records with libvirt. A VM in the records
whose domain is missing is re-created from its spec.
