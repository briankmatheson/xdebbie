# Install the first box

Install xdebbied on one Debian 13 machine and end with a working cluster of
one.

## What you need

- Debian 13 (trixie), amd64, with hardware virtualization on (`ls /dev/kvm`
  shows the device).
- Root access, and a console in case the firewall change surprises you.
  Bootstrap turns on ufw with default deny inbound; it keeps SSH (22/tcp)
  open from anywhere.
- A hostname that resolves, ideally a fully qualified one. Other boxes and
  `xdctl` reach this box by it.
- For more boxes later: the boxes can reach each other on 23443/tcp and
  51820/udp.

## 1. Add the apt repository

```bash
sudo curl -fsSLo /usr/share/keyrings/xdebbie-archive-keyring.gpg \
     https://apt.xdebbie.org/xdebbie-archive-keyring.gpg
echo "deb [signed-by=/usr/share/keyrings/xdebbie-archive-keyring.gpg] https://apt.xdebbie.org stable main" \
  | sudo tee /etc/apt/sources.list.d/xdebbie.list
sudo apt update
```

## 2. Install the package

```bash
sudo apt install xdebbied
```

The package pulls in libvirt, QEMU, LVM, WireGuard, nftables and ufw. On
first install it runs `xdebbied bootstrap`, which:

1. listens 20 seconds for a running cluster on the LAN, hears none, and
   starts a new one;
2. creates the libvirt network `xdebbie` (bridge `virbr-xdebbie` at
   `10.23.0.1`) and the storage pool `xdebbie`;
3. makes the cluster CA, this box's certificate and the API token;
4. writes `/etc/xdebbie/xdebbied.toml` and `/etc/xdebbie/xdebbie_env`;
5. sets up the firewall.

Then the service starts. [Bootstrap](../bootstrap.md) describes every step.

Note the line with the CA fingerprint (`sha256:…`). You check it when the
next box joins. `xdctl ca` prints it again.

## 3. Check the service

```bash
systemctl status xdebbied
journalctl -u xdebbied -f
```

## 4. Set up xdctl

The env file holds the API URL, the token and the CA. Root and members of
the `xdebbie` group can read it. Add yourself to the group, then log in
again:

```bash
sudo usermod -aG xdebbie "$USER"
```

After logging in again:

```bash
source /etc/xdebbie/xdebbie_env
xdctl version
xdctl host ls
xdctl net ls
xdctl mesh ls
```

`xdctl host ls` shows this box. It turns `Ready` once xdebbied has read its
capacity, within a few seconds.

## Next

- [Join a second box](join.md)
- [Create your first VM](first-vm.md)
- Optional: [use an LVM thin pool](../how-to/lvm-thin-pool.md) for faster
  disks.
