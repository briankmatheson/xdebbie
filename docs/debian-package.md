# The xdebbied Debian package

One package, `xdebbied`, carries the daemon and the CLI. It is built with
[cargo-deb](https://github.com/kornelski/cargo-deb) from
`[package.metadata.deb]` in `crates/xdebbied/Cargo.toml`, and CI publishes
each tagged release to the signed apt repository at
<https://apt.xdebbie.org> (`.drone.yaml`, `ci/apt-publish`).

## Build and check

```bash
scripts/build-deb.sh        # cargo build --release -p xdebbied -p xdctl; cargo deb -p xdebbied --no-build
scripts/verify-package.sh   # control fields, maintainer scripts, files
```

The version is the workspace's (`Cargo.toml`); `revision` in the deb
metadata is bumped only when the same version is rebuilt.

## What it installs

| Path | What |
|---|---|
| `/usr/bin/xdebbied`, `/usr/bin/xdctl` | the daemon and the CLI |
| `/usr/lib/systemd/system/xdebbied.service` | the unit (`crates/xdebbied/debian/xdebbied.service`) |
| `/etc/modules-load.d/xdebbie-mesh.conf` | loads `geneve` and `wireguard` at boot (conffile) |
| `/etc/apparmor.d/local/wg` | lets `wg` read `/var/lib/xdebbie/wg.key` (conffile) |
| `/usr/share/doc/xdebbied/` | README, BOOTSTRAP, mesh and network docs, copyright |

Depends: libvirt-daemon-system, libvirt-clients, qemu-kvm, qemu-utils,
cloud-image-utils, lvm2, thin-provisioning-tools, iproute2, nftables,
wireguard-tools, ufw, systemd.

## Maintainer scripts

`crates/xdebbied/debian/`. Each ends in `#DEBHELPER#`, where cargo-deb
puts its systemd section (from `systemd-units`): enable the unit, start it
on install, **restart it on upgrade**, stop it on remove, mask on remove
and clean up on purge, and `daemon-reload`. Our scripts do none of that
themselves.

- **postinst** (configure): creates the `xdebbie` group, `/etc/xdebbie`
  (root:xdebbie 0750) and `/var/lib/xdebbie`; sets file modes (secrets
  root 0400, config/env/certificates root:xdebbie 0440); loads `geneve`
  and `wireguard`; adds one AppArmor rule for libvirt's `virt-aa-helper`
  and reloads the `wg` profile. On **first install** (no config yet) it
  runs `xdebbied bootstrap` and prints what to do next, so bootstrap's
  output lands in the terminal. Bootstrap on a box that hears a running
  cluster stops and says to re-run with `--join`; it never starts a
  second cluster.
- **prerm**: nothing of its own. On upgrade the daemon keeps running while
  files unpack; postinst's systemd section restarts it.
- **postrm** (purge): removes `/etc/xdebbie` and `/var/lib/xdebbie` (the
  box's config, token, certificates, keys and state) and the AppArmor rule
  postinst added. VM disks and the `xdebbie` group stay.

## The service

`ExecStartPre=xdebbied bootstrap` (creates only what is missing), then
`xdebbied serve`. `Restart=on-failure`, except exit 78 (a bad config or
token file: restarting cannot help). Runs as root with
`ProtectSystem=full`; the unit lists the paths it may write and why.
