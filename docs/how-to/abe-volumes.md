# Use ABE volumes

Give a host an ABE block exporter so its VMs can use network disks, and
create standalone volumes for Kubernetes.

[ABE](https://github.com/briankmatheson/abe) exports LVM volumes over
NVMe/TCP. With ABE on every host, a VM created with `--storage abe` gets its
disks from the ABE of the host it lands on, and `xdctl volume` makes disks
you can plug into any VM. [VM storage](../storage.md) explains how it works.

You need:

- ABE 0.5.0 or later (mutual TLS), and `nvme-cli`.
- A volume group on the host, ideally with a thin pool.
- The mesh's WireGuard underlay up on the host (`ip addr show wg-flock`).

The examples use box 0, whose underlay address is `10.22.0.1`. On box N use
`10.22.N.1`.

## 1. Install and configure ABE

Install the package, then tell ABE which volume group to use and to listen
on the underlay address only:

```bash
sudo apt install ./abe-block-exporter_VERSION_amd64.deb nvme-cli
sudo tee /etc/default/abe <<'EOF'
ABE_VG=VG
ABE_THIN_POOL=THIN_POOL
ABE_TRADDR=10.22.0.1
ABE_ADVERTISE=10.22.0.1
ABE_LISTEN=10.22.0.1:443
ABE_TLS_CERT=/etc/xdebbie/api-cert.pem
ABE_TLS_KEY=/etc/xdebbie/api-key.pem
ABE_TLS_CA=/etc/xdebbie/api-ca.pem
EOF
```

ABE reuses the box's cluster certificate, so it admits only boxes enrolled
in the cluster.

## 2. Start ABE after the underlay exists

xdebbied creates `wg-flock`, so make ABE wait for its address at boot:

```bash
sudo mkdir -p /etc/systemd/system/abe.service.d
sudo tee /etc/systemd/system/abe.service.d/underlay.conf <<'EOF'
[Unit]
After=xdebbied.service
[Service]
ExecStartPre=/usr/bin/timeout 90 /bin/sh -c 'until ip -4 addr show to 10.22.0.1/32 | grep -q inet; do sleep 1; done'
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now abe
sudo modprobe nvme-tcp
echo nvme-tcp | sudo tee /etc/modules-load.d/nvme-tcp.conf
```

## 3. Open ABE's ports on the underlay

Bootstrap's firewall denies everything it does not know about. Open ABE's
API and its NVMe/TCP range on the underlay only:

```bash
sudo ufw allow in on wg-flock proto tcp from 10.22.0.0/16 to any port 23000:23999 comment 'abe nvme/tcp'
sudo ufw allow in on wg-flock proto tcp from 10.22.0.0/16 to any port 443 comment 'abe api'
```

Leave `xdebbie` out of these comments: bootstrap deletes `xdebbie`-tagged
rules it did not create.

## 4. Tell xdebbied

Edit `/etc/xdebbie/xdebbied.toml` and add one line to this box's `[[hosts]]`
entry:

```toml
[[hosts]]
name = "box0"
# ...the lines bootstrap wrote...
abe = "https://10.22.0.1:443"
```

Then restart:

```bash
sudo systemctl restart xdebbied
xdctl host ls        # the ABE GiB and TLS columns fill in
```

## 5. Create a VM on ABE disks

```bash
xdctl vm create demo --image debian-13 --disk-gib 10 --storage abe
xdctl vm describe demo | jq .disks
```

On the host, `sudo nvme list` shows the volumes attached.

## 6. Create and attach a standalone volume

```bash
xdctl volume create pvc-demo --gib 5
xdctl volume ls                         # note the ID
xdctl volume attach VOLUME_ID demo
```

In the guest, the disk appears at
`/dev/disk/by-id/scsi-0QEMU_QEMU_HARDDISK_VOLUME_ID`. To remove it:

```bash
xdctl volume detach VOLUME_ID
xdctl volume rm VOLUME_ID
```

## If it does not work

- **VM stays `Pending` with `no abe`**: the host has no `abe` line, or it is
  in the wrong table.
- **`abe capacity unknown`**: xdebbied has not read ABE's `/utilization`
  yet. Check `xdctl host describe HOST` for the error, often `tls: …`.
- **`tls: invalid peer certificate: UnknownIssuer`**: ABE's `ABE_TLS_CA` is
  not the cluster CA.
- **507 from `volume create`**: no ABE has room. The message lists each
  host's reason.
