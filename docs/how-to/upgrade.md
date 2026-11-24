# Upgrade xdebbied

Move every box to a new release, one at a time.

Running VMs are not touched by an upgrade: they are QEMU processes under
libvirt, not under xdebbied. While a box's daemon restarts, new work waits.

## 1. Read the changelog

Check [CHANGELOG.md](https://github.com/briankmatheson/xdebbie/blob/main/CHANGELOG.md)
for anything marked as a change in behaviour between your version and the
new one.

## 2. Upgrade one box

```bash
sudo apt update
sudo apt install xdebbied
```

The package restarts the service. Before it starts, bootstrap runs and adds
anything the new release needs; it keeps your configuration.

Check it came back:

```bash
systemctl status xdebbied
xdctl version
```

`xdctl version` shows xdctl's version, this box's daemon, and the
leader's if it is another box.

## 3. Repeat on the other boxes

Do the followers first and the leader last (`xdctl leader` names it), so
the cluster changes leader at most once.

## Mixed versions

Boxes on different releases work together while you upgrade. One case
needs a setting: daemons from before signed discovery messages send
unsigned ones. While such boxes remain, set
`discovery_accept_unsigned = true` on the upgraded ones, and remove it when
the last box is done.

## If the service does not start

A configuration that the new release cannot read stops the daemon with exit
status 78, and systemd does not retry. `systemctl status xdebbied` names
the problem. Fix the file, then:

```bash
sudo systemctl restart xdebbied
```

`sudo xdebbied bootstrap --force` regenerates the configuration and keeps
your settings; see the [configuration reference](../reference/configuration.md).
