# VMs

Create, start, stop, reboot and delete VMs, and what each one is made of.

## What you need

- A running cluster and `xdctl` set up
  ([One box to a running VM](../getting-started/first-box.md)).
- A base image on every host a VM may land on ([Base images](../storage/local.md#base-images)).

## Create a VM

```bash
xdctl vm create web1 --image debian-13 --cpus 2 --memory-mib 2048 --disk-gib 20 \
      --user-data user-data.yaml
```

| Option | Default | Meaning |
|---|---|---|
| `--image IMAGE` | required | base image name, as it exists on every host |
| `--cpus N` | 2 | vCPUs |
| `--memory-mib N` | 4096 | memory, MiB, at least 256 |
| `--disk-gib N` | 40 | root disk, GiB |
| `--data-disk-gib N` | none | a second, blank disk |
| `--storage local\|abe` | `local` | where the disks live ([Storage](../storage/README.md)) |
| `--ip ADDR` | allocated | pin an overlay address |
| `--user-data FILE` | none | cloud-init user-data |
| `--failure-domain FD`, `--group G`, `--strict`, `--host H` | any | placement ([Scheduling](scheduling.md)) |
| `--label KEY=VALUE` | none | labels; repeatable |
| `--dry-run` | off | show the ranked hosts; create nothing |

The name is a DNS label: 1-63 of `a-z`, `0-9` and `-`, not starting or
ending with `-`. It is unique among VMs that are not being deleted.

## Look at VMs

```bash
xdctl vm ls                      # ID NAME PHASE HOST IP AGE
xdctl vm ls -l xdebbie.dev/cluster=default.demo
xdctl vm describe web1           # the whole record, JSON; name or id
```

`message` in the record says what the VM waits for or what failed last.

## Start, stop, reboot

```bash
xdctl vm stop web1     # ACPI shutdown
xdctl vm start web1
xdctl vm reboot web1   # running VMs only
```

These set what you want; the reconciler makes it so on its next pass.
`stop` sends an ACPI shutdown and waits for the guest; there is no timeout
and no forced power-off. A guest that ignores ACPI stays up.

A VM you shut down from inside the guest is started again on the next pass,
because its wanted power state is still on. Use `xdctl vm stop`.

## Delete a VM

```bash
xdctl vm rm web1
```

Delete is a hard power-off (`virsh destroy`), then undefine and remove its
disks and seed. The API answers 202 and the VM shows `Deleting` until it is
gone. abe volumes attached to it are unplugged and kept.

### Protected VMs

A VM labelled `xdebbie.io/protected=true` is refused delete (409) unless
you say so:

```bash
xdctl vm create db1 --image debian-13 --label xdebbie.io/protected=true
xdctl vm rm db1 --force
```

## Phases

```text
Pending --place--> Provisioning --disk, seed, define, start--> Running <--> Stopped
                        ^                                         |  ^
                        +------------- domain missing ------------+  |
                                                       Migrating ----+
any --delete--> Deleting --destroy, undefine, remove disks--> (gone)
```

| Phase | Meaning |
|---|---|
| `Pending` | accepted, not placed yet; `message` says why |
| `Provisioning` | placed; disks, seed and domain being made |
| `Running` | the domain runs |
| `Stopped` | the domain is defined and shut off |
| `Migrating` | a live migration is in progress |
| `Deleting` | being torn down |
| `Failed` | stopped for good, for example when its host was removed; see `message` |

A `Running` VM whose domain disappears goes back to `Provisioning` and is
re-created from its spec.

## What a VM is made of

- **Identity.** The VM's id is a UUID chosen by the client
  (`PUT /v1/vms/{id}`; `xdctl` makes one). It is also the libvirt domain
  UUID, the cloud-init `instance-id`, and the Cluster API providerID
  (`xdebbie://ID`). The libvirt domain is named after the VM. The MAC is
  `52:54:00:` plus the id's first three bytes.
- **The domain.** x86_64, q35, `host-passthrough` CPU, virtio disks, NIC
  and RNG, a QEMU guest agent channel, a serial console, and a virtio video
  device with VNC on the host's `127.0.0.1`. Marked autostart. Its NIC MTU
  is the mesh MTU.
- **The seed.** A NoCloud ISO built by `seed_command` (`cloud-localds`):
  your `user-data`; `meta-data` with `instance-id`, `local-hostname` and
  `provider_id`; a `network-config` for DHCP on the VM's MAC; and
  `vendor-data`, a script that enables `fstrim` on every boot so freed
  blocks go back to the thin pool.
- **Addresses.** The allocated overlay address comes first; others come
  from the guest agent, ARP or the DHCP binding.

VMs are immutable: `PUT` with the same spec answers 200, a different spec
409. To resize, create a new VM and delete the old one.

## See the console

```bash
sudo virsh console web1        # on the VM's host; Ctrl-] to leave
sudo virsh vncdisplay web1     # then a VNC viewer through an SSH tunnel
```

## If it goes wrong

- If a VM stays `Pending`, then `message` lists each host and the filter it
  failed ([Scheduling](scheduling.md#filters)).
- If it is `Running` with no address, then look at its console
  ([A VM never finishes booting](../reference/troubleshooting.md#a-vm-never-finishes-booting)).
- If `rm` answers 409 *is protected*, then add `--force`.
- If `create` answers 409 *a vm named ... already exists*, then pick
  another name, or wait until the old one is gone.

## Reference

[`xdctl vm`](../reference/xdctl.md#vms), [`/v1/vms`](../reference/api.md#vms),
[`seed_command`](../reference/configuration.md#top-level-keys),
[Limits: VMs and hosts](../reference/limits.md#vms-and-hosts).
