# Join a second box

Add a Debian 13 box to the cluster, approve it, and connect the two over
the mesh.

## What you need

- A first box, running ([One box to a running VM](first-box.md)). It is
  `box0.example.org` below.
- Its CA fingerprint: `xdctl ca` on it prints the `sha256:` line.
- The new box reaches the first on 23443/tcp, and each reaches the other
  on 51820/udp.

## Steps

1. On the new box, add the apt repository as in
   [step 1 of the first box](first-box.md#steps), then install:

   ```bash
   sudo apt install xdebbied
   ```

   The package runs bootstrap, which listens 20 seconds for a cluster. With
   discovery off (the default) it hears nothing and makes the box a cluster
   of its own. That is fine: step 2 takes it over while it is empty. If it
   does hear yours, it stops and tells you to re-run with `--join`.

2. Ask to join, on the new box:

   ```bash
   sudo xdebbied bootstrap --join https://box0.example.org:23443 \
        --ca-fingerprint sha256:FINGERPRINT
   ```

   `--join box0.example.org` means the same: a missing `https://` and port
   `23443` are filled in. Bootstrap fetches the cluster CA, checks the
   fingerprint, makes a key and a certificate request, sends it, prints the
   key fingerprint and waits. Leave it running.

3. On any box already in the cluster, check the request and approve it:

   ```bash
   xdctl enroll ls
   xdctl approve NEW_BOX
   ```

   `NEW_BOX` is the hostname in the list. Check that the `FINGERPRINT`
   column matches what the new box printed. Approval signs the certificate
   and gives the box the lowest free box id: 1 for the second box.

4. Within seconds bootstrap on the new box finishes: it installs the
   certificate, fetches the cluster token, takes its own `/24` (bridge
   `10.23.1.1`, VMs `10.23.1.2` to `10.23.1.254`), creates its bridge,
   writes its config and registers itself as a host. Then start the
   service:

   ```bash
   sudo systemctl start xdebbied
   ```

5. Print each box's WireGuard public key. Run on each box:

   ```bash
   xdctl mesh wg
   ```

6. Add box 1 as a peer, on box 0:

   ```bash
   xdctl mesh peer add 1 --pubkey BOX1_PUBKEY --endpoint box1.example.org:51820
   ```

7. Add box 0 as a peer, on box 1:

   ```bash
   xdctl mesh peer add 0 --pubkey BOX0_PUBKEY --endpoint box0.example.org:51820
   ```

   Peers are per box, so each command runs on the box it changes. The
   endpoint is any address the other box reaches on UDP 51820; on one LAN,
   its LAN address works as well as its name.

8. Check both ends:

   ```bash
   xdctl mesh ls
   xdctl host ls
   ```

## What you should see

On each box, `xdctl mesh ls` shows:

- the peer's row `Up`, with an RTT;
- under `WIREGUARD`, the peer's handshake a few seconds old;
- under `CONDITIONS`, `Ready true AllPeersUp` and no blocking condition.

`xdctl host ls` lists both hosts. The new one turns `Ready` once the mesh
is up: the leader reaches its libvirt over the overlay.

With discovery off each box leads alone, so point `XDEBBIE_URL` at the
first box from everywhere: that is the box holding the cluster's VMs. To
let the boxes elect a leader and fail over, turn discovery on
([The leader](../compute/leader.md)).

## If the new box was already in use

`--join` on a box that is still the empty cluster the package made (no VMs,
no volumes, no other boxes, no mesh peers) moves that cluster's identity
aside and enrols. The files are renamed, not deleted:
`/etc/xdebbie/api-*.pem.own-cluster-STAMP` and `token.own-cluster-STAMP`.

If you used the box on its own first, `--join` refuses and says what is in
it. Move or remove those, or pass `--force`. `--force` replaces the box's
CA and certificates, and its VMs then belong to no cluster you can reach.
To wipe a box and join it from scratch, see
[Start over on a box](../compute/boxes.md#start-over-on-a-box).

## If it goes wrong

- If the handshake under `WIREGUARD` says `never`, then the other box does
  not have this one as a peer yet, or UDP 51820 does not get through.
- If a condition says `BridgeAddressMismatch`, then the bridge has another
  box's overlay address: it was set up under another box id. Start over on
  that box.
- If bootstrap waits for ever, then nobody has approved it yet:
  `xdctl enroll ls` on a member.
- If the CA fingerprint does not match, then stop: you are talking to a
  different box than you think.
- Everything else: [A box will not join](../reference/troubleshooting.md#a-box-will-not-join)
  and [The mesh is degraded](../reference/troubleshooting.md#the-mesh-is-degraded).

## Reference

[Boxes: bootstrap, join, enrol, approve](../compute/boxes.md),
[The mesh](../network/mesh.md),
[`xdctl enroll` and `xdctl mesh`](../reference/xdctl.md#boxes-and-the-ca),
[`xdebbied bootstrap`](../reference/xdebbied.md#xdebbied-bootstrap).

Next: [A Kubernetes cluster with capx](kubernetes.md).
