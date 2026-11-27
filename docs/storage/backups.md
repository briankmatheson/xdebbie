# Backups

Save what a box knows, and put it back after a disk failure or a
reinstall.

## What you need

- Root on the box.
- Somewhere else to keep the archive. It holds the token and private keys.

## What to back up

| Path | Why |
|---|---|
| `/var/lib/xdebbie/state.json` | hosts, VMs and volumes. Without it xdebbied forgets the VMs (they keep running). |
| `/var/lib/xdebbie/*.json` | mesh spec, settings, client config, enrolments, DNS reservations |
| `/var/lib/xdebbie/wg.key` | the box's WireGuard identity. Without it every peer needs a new key. |
| `/etc/xdebbie/` | config, token, certificates; on the first box also the **cluster CA key**, `api-ca-key.pem` |

The CA key matters most. Without it no box can join and no certificate can
be signed. Keep its copy apart from the rest, encrypted, on another device.

VM disks are not in these paths. Back them up from inside the VMs, or as
LVM snapshots on the host. abe volumes live on abe.

Followers also mirror the leader's records every 5 seconds (with
discovery on), so another box usually has a recent copy of the VMs.

## Make a backup

`state.json` is rewritten atomically (temporary file, `fsync`, rename), so
a plain copy is consistent:

```bash
sudo tar -czf xdebbie-BOX-$(date +%F).tar.gz /var/lib/xdebbie /etc/xdebbie
```

## Restore a box

On a reinstalled box with the same hostname:

1. Install the package, then stop the service:

   ```bash
   sudo apt install xdebbied
   sudo systemctl stop xdebbied
   ```

2. Remove what the fresh install made, so no stray CA key or libvirt
   network stays behind:

   ```bash
   sudo virsh net-destroy xdebbie
   sudo virsh net-undefine xdebbie
   sudo rm -rf /etc/xdebbie/* /var/lib/xdebbie/*
   ```

3. Put the saved files back:

   ```bash
   sudo tar -xzf xdebbie-BOX-DATE.tar.gz -C /
   ```

4. Run bootstrap, which re-creates the libvirt network for the restored box
   id and fixes owners and modes, then start:

   ```bash
   sudo xdebbied bootstrap
   sudo systemctl start xdebbied
   ```

## Roll back the VM records only

```bash
sudo systemctl stop xdebbied
sudo cp STATE_BACKUP.json /var/lib/xdebbie/state.json
sudo systemctl start xdebbied
```

The reconciler then compares the records with libvirt. A VM in the records
whose domain is missing is re-created from its spec.

## What you should see

`xdctl host ls`, `xdctl vm ls` and `xdctl mesh ls` as before the failure,
and `xdctl ca` with the same fingerprint.

## If it goes wrong

- If the service will not start after a restore, then a file came back
  with the wrong owner: `sudo xdebbied bootstrap` (without `--force`)
  repairs owners and modes without touching contents.
- If peers show `never` for the handshake, then `wg.key` was not restored
  and the box has a new WireGuard identity: give the peers its new key.

## Reference

[Files and paths](../reference/files-and-ports.md#files).
