# One box to a running VM

Install xdebbie on one Debian 13 box, create a VM, and log in to it.

## What you need

- Debian 13 (trixie), amd64, with `/dev/kvm` present.
- Root, and a console. Bootstrap turns on ufw with default deny inbound.
  It keeps SSH (22/tcp) open from anywhere, so you should not lose your
  session, but a console is cheap insurance.
- A hostname that resolves, ideally fully qualified. Other boxes and
  `xdctl` reach this box by it.
- For more boxes later: the boxes reach each other on 23443/tcp (the API)
  and 51820/udp (WireGuard).

## Steps

1. Add the apt repository:

   ```bash
   sudo curl -fsSLo /usr/share/keyrings/xdebbie-archive-keyring.gpg \
        https://apt.xdebbie.org/xdebbie-archive-keyring.gpg
   echo "deb [signed-by=/usr/share/keyrings/xdebbie-archive-keyring.gpg] https://apt.xdebbie.org stable main" \
     | sudo tee /etc/apt/sources.list.d/xdebbie.list
   sudo apt update
   ```

2. Install the package:

   ```bash
   sudo apt install xdebbied
   ```

   It pulls in libvirt, QEMU, LVM, WireGuard, nftables and ufw, then runs
   `xdebbied bootstrap`. Bootstrap listens 20 seconds for a cluster on the
   LAN, hears none, and makes this box box 0 of a new cluster: the libvirt
   network `xdebbie` (bridge `virbr-xdebbie` at `10.23.0.1`), the storage
   pool `xdebbie`, the cluster CA, this box's certificate, the API token,
   `/etc/xdebbie/xdebbied.toml` and the firewall. Then the service starts.
   [Boxes](../compute/boxes.md) has every step.

3. Note the line `CA fingerprint: sha256:...` in the output. You check it
   when the next box joins. `xdctl ca` prints it again.

4. Check the service:

   ```bash
   systemctl status xdebbied
   ```

5. Let your account use `xdctl`. The env file holds the API URL, token and
   CA, and only root and the `xdebbie` group can read it:

   ```bash
   sudo usermod -aG xdebbie "$USER"
   ```

   Log out and in again, then:

   ```bash
   source /etc/xdebbie/xdebbie_env
   xdctl host ls
   ```

6. Put a base image where xdebbied looks. Without LVM, the image `NAME` is
   the volume `NAME.qcow2` in the `xdebbie` pool. Debian's cloud image
   works:

   ```bash
   curl -fLO https://cloud.debian.org/images/cloud/trixie/latest/debian-13-generic-amd64.qcow2
   sudo cp debian-13-generic-amd64.qcow2 /var/lib/libvirt/xdebbie/debian-13.qcow2
   sudo virsh pool-refresh xdebbie
   ```

   With an LVM thin pool, use `xdctl image import` instead
   ([Local disks](../storage/local.md)).

7. Write cloud-init user-data with your SSH key:

   ```bash
   cat > user-data.yaml <<'EOF'
   #cloud-config
   ssh_authorized_keys:
     - ssh-ed25519 AAAA_YOUR_PUBLIC_KEY you@example.org
   EOF
   ```

8. Create the VM:

   ```bash
   xdctl vm create web1 --image debian-13 --cpus 2 --memory-mib 2048 --disk-gib 20 \
         --user-data user-data.yaml
   ```

9. Watch it start:

   ```bash
   xdctl vm ls
   ```

10. Log in from the box, with the address `xdctl vm ls` shows. `debian` is
    the Debian cloud image's user:

    ```bash
    ssh debian@10.23.0.2
    ```

11. Clean up when you are done:

    ```bash
    xdctl vm stop web1
    xdctl vm rm web1
    ```

## What you should see

`xdctl host ls` lists this box as `Ready` within a few seconds, once
xdebbied has reached its libvirt:

```text
NAME              FD         CP     VCPU        MEM GiB       DISK GiB       ABE GiB TLS  STATUS
box0.example.org  default    true     0/36       0/60             400             -  -    Ready
```

`VCPU` and `MEM GiB` are used/allocatable after the reserve and overcommit
([Scheduling](../compute/scheduling.md#capacity)).

`xdctl vm ls` shows the VM go `Pending`, `Provisioning`, `Running`. The `IP`
column is filled from the start: xdebbied picks the address when you create
the VM, and its own DHCP responder hands it out.

The VM reaches the internet through the box, which masquerades its
traffic. Hosts on your LAN cannot open connections to it.

## If it goes wrong

- If `xdctl` says *Permission denied* reading `xdebbie_env`, then you have
  not logged in again since joining the `xdebbie` group.
- If `xdctl host ls` stays `Unknown`, then xdebbied cannot reach libvirt:
  `journalctl -u xdebbied -n 50`.
- If the VM stays `Pending`, then `xdctl vm describe web1` says why in
  `message` ([A VM stays Pending](../reference/troubleshooting.md#a-vm-stays-pending)).
- If the VM is `Running` but never gets an address, then the image may be
  waiting on a screen nobody sees
  ([A VM never finishes booting](../reference/troubleshooting.md#a-vm-never-finishes-booting)).
- If the service is not running, then `systemctl status xdebbied` names
  the problem ([xdebbied will not start](../reference/troubleshooting.md#xdebbied-will-not-start)).

## Reference

[`xdctl vm create`](../reference/xdctl.md#vms),
[`xdebbied bootstrap`](../reference/xdebbied.md#xdebbied-bootstrap),
[the configuration file](../reference/configuration.md),
[files and ports](../reference/files-and-ports.md).

Next: [Join a second box](second-box.md).
