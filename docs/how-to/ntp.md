# Give VMs NTP servers through DHCP

Tell every VM which NTP servers to use, with DHCP option 42.

Guests that use systemd-networkd pass the servers to systemd-timesyncd
(`UseNTP=` is on by default). Other DHCP clients may need configuring to
use option 42.

## 1. Set the servers

In `[network]` in `/etc/xdebbie/xdebbied.toml`, on every box:

```toml
ntp = ["10.23.0.247", "10.23.1.247"]
```

Use IPv4 addresses. Boxes that join later copy the setting from the first
box.

## 2. Restart xdebbied

```bash
sudo systemctl restart xdebbied
xdctl net ls          # shows the NTP servers
```

## 3. Let the VMs pick it up

A running guest gets the option at its next lease renewal, which is half
the lease time (12 hours by default). To apply it now, renew in the guest:

```bash
sudo networkctl renew INTERFACE
timedatectl timesync-status
```

New VMs get it at first boot.

To stop offering it, set `ntp = []` or remove the line.
