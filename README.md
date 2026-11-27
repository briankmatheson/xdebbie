# xdebbie

**xdebbie runs VMs on a few Debian boxes as one cluster.** It places them,
moves them, and puts them back when a box goes away. Kubernetes gets its
machines from it through Cluster API.

Install the package on the first box and you have a cluster of one. Every
other box joins with one command and your approval. The boxes talk over an
encrypted WireGuard mesh with one shared VM network on top, so a VM on one
box can reach a VM on any other as if they shared a switch.

## What you get

- **One package.** `apt install xdebbied` on stock Debian 13 sets up
  libvirt, storage, a certificate authority and an API token.
- **Join by approval.** A new box asks; you run `xdctl approve`; it gets
  its certificate and the cluster token. Nothing to copy by hand.
- **One VM network.** Every box's VMs share `10.23.0.0/16` over a Geneve
  overlay on WireGuard. Laptops join with xdj.
- **Fast disks.** LVM thin snapshots of a base image (under a second), or
  [abe](https://github.com/briankmatheson/abe) volumes over NVMe/TCP that
  any box can attach.
- **Kubernetes.** `capx` is a Cluster API v1beta2 infrastructure provider.

## Install

On every box, add the apt repository:

```bash
sudo curl -fsSLo /usr/share/keyrings/xdebbie-archive-keyring.gpg \
     https://apt.xdebbie.org/xdebbie-archive-keyring.gpg
echo "deb [signed-by=/usr/share/keyrings/xdebbie-archive-keyring.gpg] https://apt.xdebbie.org stable main" \
  | sudo tee /etc/apt/sources.list.d/xdebbie.list
sudo apt update && sudo apt install xdebbied
```

The first box starts a cluster and prints its CA fingerprint. Each other
box joins with that fingerprint and waits for you to approve it:

```bash
sudo xdebbied bootstrap --join https://FIRST_BOX:23443 --ca-fingerprint sha256:FINGERPRINT
xdctl approve NEW_BOX                # on a box that is already in
```

A new box starts a cluster of its own when the package installs; clear that
first. Getting started in the docs walks through it.

## Try it

```bash
source /etc/xdebbie/xdebbie_env      # API URL, token and CA for xdctl
xdctl host ls
xdctl vm create web1 --image debian-13 --cpus 2 --memory-mib 4096
xdctl vm ls
xdctl mesh ls
```

## Learn more

The docs are in [`docs/`](docs/README.md): getting started, network,
storage, compute, reference, and a glossary of every noun and verb in
xdebbie.

## Status

Runs a two-box home lab every day, with two Kubernetes clusters on it.
IPv4 only for now; IPv6 is planned for 3.0. abe-backed VMs cannot live
migrate yet.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
