# Create your first VM

Put a base image on every host, create a VM from it, and log in to it.

## 1. Get a base image

VMs are made from a base image that must exist on every host they may run
on. Debian's cloud images work. Download one on each host:

```bash
curl -fLO https://cloud.debian.org/images/cloud/trixie/latest/debian-13-generic-amd64.qcow2
```

The image must boot without a screen. XDebbie's VMs have no video device,
only a serial console. See
[A VM never finishes booting](../troubleshooting.md#a-vm-never-finishes-booting)
if yours hangs.

## 2. Put it where xdebbied looks

Without LVM, the base image is the volume `IMAGE.qcow2` in the host's
storage pool. Copy it there under the name you will use, and tell libvirt:

```bash
sudo cp debian-13-generic-amd64.qcow2 /var/lib/libvirt/xdebbie/debian-13.qcow2
sudo virsh pool-refresh xdebbie
```

With an [LVM thin pool](../how-to/lvm-thin-pool.md), use
`xdctl image import` instead.

## 3. Write cloud-init user-data

Give the VM your SSH key:

```bash
cat > user-data.yaml <<'EOF'
#cloud-config
ssh_authorized_keys:
  - ssh-ed25519 AAAA_YOUR_PUBLIC_KEY you@example.org
EOF
```

## 4. Create the VM

```bash
xdctl vm create web1 --image debian-13 --cpus 2 --memory-mib 2048 --disk-gib 20 \
      --user-data user-data.yaml
```

Add `--dry-run` first to see which host it would go to.

## 5. Watch it start

```bash
xdctl vm ls
```

The VM moves from `Pending` to `Provisioning` to `Running`. Its address
(`10.23.BOX.N`) is in the `IP` column from the start: xdebbied assigns it at
creation, and its DHCP responder hands it out.

```bash
xdctl vm describe web1
```

shows the whole record, including `message` if something waits or fails.

## 6. Log in

From the host the VM runs on (or any box on the mesh):

```bash
ssh debian@10.23.0.2
```

Use the address `xdctl vm ls` showed. `debian` is the Debian cloud image's
user.

The VM reaches the internet through its host, which masquerades its
traffic. Hosts on your LAN cannot open connections to the VM.

## 7. Clean up

```bash
xdctl vm stop web1
xdctl vm rm web1
```

## Next

- [Run Kubernetes on it with Cluster API](cluster-api.md)
- [Move VMs off a host](../how-to/drain-and-migrate.md)
