# Glossary

Every noun and verb in xdebbie, one or two lines each, with a link to
where it is explained.

The list was built from the code: `xdctl` subcommands, API routes, the
model types and their phases, config keys, the mesh's actions and
conditions, and the agent operations.

## A

- **abe**: the block exporter that serves LVM volumes over NVMe/TCP; a
  host's `abe` URL lets it run abe-backed disks and volumes.
  [abe-backed disks](storage/abe.md)
- **abe capacity** (`abeCapacity`): the last `/utilization` reading of a
  host's abe, used to decide whether a disk fits.
  [Capacity](storage/abe.md#capacity)
- **abe-csi**: the Kubernetes CSI driver that uses xdebbie volumes from
  inside VMs. [Volumes](storage/volumes.md)
- **add** (`xdctl host add`, `mesh peer add`, `mesh client add`): register
  a host, a mesh peer or an xdj client. [xdctl](reference/xdctl.md)
- **address** (VM): the overlay IPv4 address bound to a VM for its life,
  listed first in `addresses`. [Addresses and DHCP](network/addresses-and-dhcp.md#how-a-vm-gets-its-address)
- **adopt**: give an address to a VM that existed before `[network]`,
  keeping one it already reported. [Moving an existing host](network/addresses-and-dhcp.md#moving-an-existing-host-to-xdebbies-dhcp)
- **advertise URL** (`advertise_url`): the URL other boxes and `xdctl` use
  to reach a box. [Discovery](network/discovery.md#advertised-url)
- **agent**: a box's daemon doing host-local work for the leader through
  `POST /v1/agent/OP` (inventory, provision, start, shutdown, reboot,
  destroy, migrate, attach-volume, ...). [Scheduling](compute/scheduling.md#the-reconcile-loop)
- **allocatable**: what a host can take after reserve and overcommit.
  [Capacity](compute/scheduling.md#capacity)
- **allow_plain_underlay**: bring your own underlay instead of the managed
  WireGuard. [Bring your own underlay](network/mesh.md#bring-your-own-underlay)
- **announce** (`ANNOUNCE`): the discovery message each box sends every 5
  seconds. [Discovery](network/discovery.md#the-protocol)
- **anti-affinity group**: VMs spread across hosts (`--group`); strict
  means never two on one host. [Anti-affinity](compute/scheduling.md#anti-affinity)
- **apply** (`xdctl mesh apply`): replace a box's mesh spec from a file.
  [Mesh spec](reference/mesh-spec.md)
- **approve** (`xdctl approve`): sign a box's certificate request and give
  it a box id. [Enrolment](compute/boxes.md#join-a-cluster-enrolment-and-approval)
- **attach** (`xdctl volume attach`): plug a volume into a VM;
  `attachTo` is the wish, `attached` what happened. [Volumes](storage/volumes.md#how-it-behaves)

## B

- **base image**: the disk image VMs are made from, `IMAGE.qcow2` in the
  pool or the LV `base-IMAGE`, on every host. [Base images](storage/local.md#base-images)
- **base volume**: an abe volume filled once from an image and sealed;
  root disks are thin clones of it. [Base volumes](storage/abe.md#base-volumes)
- **blackhole route**: a route at metric 4294967295 that drops mesh
  traffic when its link goes away (`blackhole_add`, `blackhole_del`).
  [Blackhole routes](network/routes-and-firewall.md#blackhole-routes-for-the-mesh-networks)
- **blocking condition**: a mesh condition that stops all kernel changes
  until it clears. [Conditions](network/mesh.md#conditions)
- **bootstrap** (`xdebbied bootstrap`): set a box up, or check and repair
  it. [Boxes](compute/boxes.md#what-bootstrap-does)
- **box**: a machine running xdebbied, a member of the cluster.
  [Network](network/README.md)
- **box id**: a box's number, 0-253, assigned at approval; it fixes its
  addresses `10.22.N.1` and `10.23.N.1`. [Enrolment](compute/boxes.md#join-a-cluster-enrolment-and-approval)
- **bridge**: the overlay bridge `virbr-xdebbie` (libvirt network
  `xdebbie`) that every VM NIC and tunnel joins. [The bridge](network/mesh.md#the-bridge)
- **bridge MAC**: `52:54:00:23:NN:fe`, which the other boxes' FDB points
  at. [The bridge](network/mesh.md#the-bridge)

## C

- **CA** (cluster CA): the one certificate authority of a cluster, kept on
  the first box. [Security](compute/security.md#one-ca-per-cluster)
- **CA box**: the box holding the CA key; enrolment calls go there
  (`ca_url`). [Enrolment](compute/boxes.md#join-a-cluster-enrolment-and-approval)
- **capacity**: a host's CPUs, memory and free disk, read by inventory.
  [Capacity](compute/scheduling.md#capacity)
- **capx**: the Cluster API infrastructure provider for xdebbie.
  [capx and Cluster API](compute/kubernetes.md)
- **check** (`xdebbied check`): try every configured host over libvirt.
  [xdebbied](reference/xdebbied.md)
- **client**: a laptop or server on the mesh through one WireGuard link to
  a hub; no Geneve. [Clients and the hub](network/clients.md)
- **client config** (`[clients]`, `/v1/client-config`): DNS zones, search
  domains and a banner a hub tells its clients. [What clients are told](network/clients.md#what-clients-are-told-dns-search-welcome)
- **clone**: a thin abe root disk made from a base volume.
  [Base volumes](storage/abe.md#base-volumes)
- **Cluster API**: the Kubernetes project capx plugs xdebbie into.
  [capx and Cluster API](compute/kubernetes.md)
- **completion** (`xdctl completion`): print a shell completion script.
  [xdctl](reference/xdctl.md#other)
- **condition**: a named true-or-false fact in the mesh status, such as
  `Ready` or `BridgeMissing`. [Conditions](network/mesh.md#conditions)
- **control-plane eligible**: a host that may run control-plane VMs.
  [Filters](compute/scheduling.md#filters)
- **cordon**, **uncordon**: stop, or allow again, new VMs on a host.
  [Cordon](compute/scheduling.md#cordon-and-uncordon)
- **create** (`xdctl vm create`, `volume create`): make a VM or volume.
  [VMs](compute/vms.md#create-a-vm)

## D

- **data disk**: an optional second, blank disk that lives and dies with
  its VM. [Data disks](storage/local.md#data-disks)
- **Degraded**: the mesh condition when some peer is not `Up`.
  [Conditions](network/mesh.md#conditions)
- **delete**, **rm**: remove a VM, volume, host, peer or client; a VM is
  powered off hard. [Delete a VM](compute/vms.md#delete-a-vm)
- **deny** (`xdctl enroll deny`): refuse a pending enrolment.
  [Enrolment](compute/boxes.md#join-a-cluster-enrolment-and-approval)
- **derived** (FDB mode): static forwarding entries from the MAC scheme;
  the default. [FDB modes](network/mesh.md#fdb-modes-derived-and-learn)
- **describe** (`xdctl ... describe`): print a whole record as JSON.
  [xdctl](reference/xdctl.md)
- **detach**: unplug a volume from its VM. [Volumes](storage/volumes.md)
- **DHCP responder**: xdebbied's own DHCP server, answering only its VMs.
  [Addresses and DHCP](network/addresses-and-dhcp.md)
- **discovery**: signed multicast by which boxes on a LAN find each other.
  [Discovery](network/discovery.md)
- **disk** (`disks[]`): an abe volume backing a VM's root or data disk.
  [How an abe VM's disks are made](storage/abe.md#how-an-abe-vms-disks-are-made)
- **domain**: the libvirt VM behind an xdebbie VM, named after it.
  [What a VM is made of](compute/vms.md#what-a-vm-is-made-of)
- **drain** (`xdctl host drain`): cordon a host and live-migrate its VMs
  away. [Drain a host](compute/migration.md#drain-a-host)
- **dry run** (`--dry-run`, `POST /v1/schedule`): rank hosts for a VM
  without creating it. [Scheduling](compute/scheduling.md#ask-where-a-vm-would-go)

## E

- **election**: how boxes with discovery on pick a leader.
  [Election](compute/leader.md#election)
- **endpoint**: where a WireGuard peer is reached, `HOST:PORT`.
  [The underlay](network/mesh.md#the-underlay-wireguard)
- **endpoint refresh**: re-asking abe where a volume is exported before
  connecting. [Endpoint refresh](storage/abe.md#endpoint-refresh)
- **enrol**, **enrolment** (`xdctl enroll`): a box (or xdj client) asking
  the CA for a certificate; pending, approved or denied.
  [Enrolment](compute/boxes.md#join-a-cluster-enrolment-and-approval)

## F

- **failure domain** (`xdctl fd`): a rack, PDU or room; VMs can be kept in
  one, and control planes are spread across them. [Failure domains](compute/scheduling.md#failure-domains)
- **FDB**: the bridge's forwarding database; `fdb_add` and `fdb_del` are
  mesh actions. [FDB modes](network/mesh.md#fdb-modes-derived-and-learn)
- **fence** (port fence): abe opens an export only to the box that asked.
  [TLS and the port fence](storage/abe.md#tls-and-the-port-fence)
- **filter**: a scheduler test a host must pass. [Filters](compute/scheduling.md#filters)
- **fingerprint**: `sha256:` of the CA, checked when joining; tokens are
  shown by a 12-digit fingerprint. [Security](compute/security.md)
- **follower**: a box that is not the leader; it forwards and mirrors.
  [The leader](compute/leader.md)
- **forward**: a follower passing a cluster request to the leader.
  [What the leader does](compute/leader.md#what-the-leader-does)

## G

- **generation**, **observed generation**: the mesh spec's version, and
  the version the kernel matches. [Mesh spec](reference/mesh-spec.md#status)
- **Geneve**: the tunnel protocol carrying the overlay between boxes
  (`gnv-bN`, UDP 6081). [The overlay](network/mesh.md#the-overlay-geneve)
- **grow** (`xdctl volume grow`): make a volume bigger in place.
  [Grow a volume](storage/volumes.md#grow-a-volume)

## H

- **heartbeat** (`HEARTBEAT`): the discovery message sent every second;
  5 seconds without the leader's starts an election. [Discovery](network/discovery.md#the-protocol)
- **host**: a hypervisor record (`[[hosts]]`, `/v1/hosts`); each box
  registers its own. [Scheduling](compute/scheduling.md)
- **host label**: a key=value on a host that VMs can require.
  [Host labels](compute/scheduling.md#host-labels)
- **hub**: the box an xdj client connects to. [Clients and the hub](network/clients.md)

## I

- **image import** (`xdctl image import`): copy an image into the thin
  pool as `base-NAME`. [Base images](storage/local.md#base-images)
- **immutable**: a VM's spec never changes; re-create to resize.
  [What a VM is made of](compute/vms.md#what-a-vm-is-made-of)
- **inventory**: the leader reading each host's capacity every pass.
  [The reconcile loop](compute/scheduling.md#the-reconcile-loop)

## J

- **join** (`bootstrap --join`): enrol a box with an existing cluster.
  [Join a second box](getting-started/second-box.md)

## K

- **keepalive**: WireGuard's persistent keepalive, 25 seconds.
  [The underlay](network/mesh.md#the-underlay-wireguard)

## L

- **leader**: the box that schedules, reconciles and answers cluster
  requests (`xdctl leader`). [The leader](compute/leader.md)
- **learn** (FDB mode): the bridge learns VM MACs across tunnels.
  [FDB modes](network/mesh.md#fdb-modes-derived-and-learn)
- **lease**: how long a DHCP answer lasts; renewals change nothing.
  [Addresses and DHCP](network/addresses-and-dhcp.md)
- **list**, **ls**: show records as a table. [xdctl](reference/xdctl.md)
- **local** (storage): disks on the VM's own host. [Local disks](storage/local.md)

## M

- **mesh**: the Geneve overlay over the WireGuard underlay, per box.
  [The mesh](network/mesh.md)
- **mesh-only** (`serve --mesh-only`): run only the API and the mesh.
  [Mesh-only mode](network/mesh.md#mesh-only-mode-and-tests)
- **mesh_routed**: let routed peers (xdj clients) open connections to a
  box's VMs. [mesh_routed](network/clients.md#letting-clients-reach-vms-mesh_routed)
- **migrate** (`xdctl vm migrate`): live-move a running VM to another host.
  [Migration](compute/migration.md)
- **mirror**: a follower copying the leader's records every 5 seconds.
  [Followers keep up](compute/leader.md#followers-keep-up)
- **MTU**: the overlay's packet size, underlay minus 50 by default.
  [MTU](network/mesh.md#mtu)

## O

- **overcommit**: how far a host's CPUs or memory may be promised beyond
  what it has. [Capacity](compute/scheduling.md#capacity)
- **overlay**: the VM network, `10.23.0.0/16`, one L2 segment across
  boxes. [Network](network/README.md)

## P

- **pack**, **spread** (`policy`): fill the fullest, or the emptiest, host
  first. [Scores](compute/scheduling.md#scores)
- **peer**: another box in a box's mesh; `Pending`, `Up`, `Down` or
  `Error`. [The mesh](network/mesh.md)
- **pending** (abe): bytes promised to disks not yet made.
  [Capacity](storage/abe.md#capacity)
- **phase**: where a VM is in its life: `Pending`, `Provisioning`,
  `Running`, `Stopped`, `Migrating`, `Deleting`, `Failed`. [Phases](compute/vms.md#phases)
- **pin** (`--host`, `--ip`): fix a VM's host or address.
  [VMs](compute/vms.md#create-a-vm)
- **place**, **schedule**: choose a host for a pending VM.
  [Scheduling](compute/scheduling.md)
- **placement**: a VM's rules for where it may go. [Filters](compute/scheduling.md#filters)
- **pool** (address): the range a box hands VM addresses from.
  [The layout](network/addresses-and-dhcp.md#the-layout)
- **pool** (storage): the libvirt pool `xdebbie`, or an LVM thin pool, or
  an abe pool. [Local disks](storage/local.md)
- **power intent**: whether a VM should be on or off; the reconciler acts
  on it. [Start, stop, reboot](compute/vms.md#start-stop-reboot)
- **probe**: the ping that decides whether a peer is `Up`.
  [Probes](network/mesh.md#probes)
- **protect** (`xdebbie.io/protected=true`): a label that makes delete need
  `--force`. [Protected VMs](compute/vms.md#protected-vms)
- **providerID**: `xdebbie://VM_UUID`, a VM's identity in Cluster API.
  [capx](compute/kubernetes.md)
- **provision**: make a placed VM's disks, seed and domain, then start it.
  [Phases](compute/vms.md#phases)

## Q

- **QoS** (`/v1/qos`): abe's IOPS arbiter reports, merged. [QoS](storage/volumes.md#qos)

## R

- **reboot** (`xdctl vm reboot`): restart a running VM.
  [Start, stop, reboot](compute/vms.md#start-stop-reboot)
- **reconcile**: compare what should be with what is, and take one step
  towards it; the leader does it for VMs, every box for its mesh
  (`xdctl mesh reconcile`). [The reconcile loop](compute/scheduling.md#the-reconcile-loop)
- **register**, **registrar**: a box putting its own host record on the
  leader. [Followers keep up](compute/leader.md#followers-keep-up)
- **renew**: an xdj client getting its next hourly certificate.
  [Security](compute/security.md#one-ca-per-cluster)
- **reserve** (`reserved`, `reserve_from_dns`): keep addresses from VMs.
  [Reserved addresses](network/addresses-and-dhcp.md#reserved-addresses)
- **rotate** (`xdctl token rotate`): replace the cluster token everywhere.
  [Rotate the token](compute/security.md#rotate-the-token)

## S

- **score**: how hosts are ranked for a VM, or boxes for leadership.
  [Scores](compute/scheduling.md#scores), [Election](compute/leader.md#election)
- **seal**: abe un-exporting a filled base volume for good.
  [Base volumes](storage/abe.md#base-volumes)
- **seed**: a VM's cloud-init NoCloud ISO (`VM-seed.iso`).
  [What a VM is made of](compute/vms.md#what-a-vm-is-made-of)
- **seen** (`xdctl net seen`): MACs heard on the bridge and what they
  were told. [Check the responder](network/addresses-and-dhcp.md#check-the-responder)
- **serve** (`xdebbied serve`): run the daemon. [xdebbied](reference/xdebbied.md)
- **settings** (`/v1/settings`): a box's runtime switches, `discovery` and
  `mesh_routed`. [API](reference/api.md#settings)
- **spec**, **status**: what you want, and what is; used for VMs and the
  mesh. [Mesh spec](reference/mesh-spec.md)
- **standalone**: a box leading alone, discovery off.
  [The leader](compute/leader.md)
- **start**, **stop**: power a VM on, or shut it down by ACPI.
  [Start, stop, reboot](compute/vms.md#start-stop-reboot)
- **state file** (`state_file`): the JSON file with hosts, VMs and
  volumes. [Backups](storage/backups.md)
- **storage** (`local`, `abe`): where a VM's disks live. [Storage](storage/README.md)
- **storage bridge** (`virbr-abe`): an optional isolated bridge for abe
  traffic. [The storage bridge](storage/abe.md#the-storage-bridge)

## T

- **take over**: `--join` moving aside the empty cluster the package made.
  [A box that was a cluster of its own](compute/boxes.md#a-box-that-was-a-cluster-of-its-own)
- **thin pool**: an LVM pool where disks cost space only as written.
  [Use an LVM thin pool](storage/local.md#use-an-lvm-thin-pool)
- **token**: the cluster's bearer token, shared by every box; narrow
  tokens open one route each. [Security](compute/security.md#what-each-credential-opens)
- **tunnel**: a Geneve link `gnv-bN` to peer N (`create_tunnel`,
  `delete_tunnel`). [The overlay](network/mesh.md#the-overlay-geneve)

## U

- **underlay**: the WireGuard network between boxes, `10.22.0.0/16`, link
  `wg-flock`. [The underlay](network/mesh.md#the-underlay-wireguard)
- **upgrade**: install a new package, box by box, leader last.
  [Upgrade](compute/boxes.md#upgrade)
- **upstream DNS** (`upstream_dns`): where VMs' DNS queries are sent.
  [NAT and DNS](network/addresses-and-dhcp.md#nat-and-dns-for-vms)

## V

- **version** (`xdctl version`, `/v1/version`): what runs where.
  [Upgrade](compute/boxes.md#upgrade)
- **VM**: a virtual machine xdebbie places and runs. [VMs](compute/vms.md)
- **VNI**: the Geneve network id, 23. [The overlay](network/mesh.md#the-overlay-geneve)
- **volume**: a standalone abe disk you plug into VMs.
  [Volumes](storage/volumes.md)

## W

- **wg** (`xdctl mesh wg`): a box's WireGuard identity and handshakes.
  [Add a peer](network/mesh.md#add-a-peer)
- **WireGuard**: the encrypted underlay (`wg_create`, `wg_set_iface`,
  `wg_peer_set`, `wg_peer_del`). [The underlay](network/mesh.md#the-underlay-wireguard)

## X

- **xdctl**: the command-line client. [xdctl](reference/xdctl.md)
- **XDebbie8sCluster**, **XDebbie8sMachine**, and their templates: capx's
  Kubernetes resources. [capx](compute/kubernetes.md#the-resources)
- **xdebbied**: the daemon on every box. [xdebbied](reference/xdebbied.md)
- **xdj**: the client that puts a laptop on the mesh.
  [A laptop on the mesh](getting-started/laptop.md)
