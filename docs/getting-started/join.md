# Join a second box

Add another Debian 13 machine to the cluster and connect the two over the
mesh.

The first box is `box0.example.org` in the examples. Replace it with your
first box's name.

## 1. Install the package

On the new box, add the apt repository as in
[Install the first box](install.md#1-add-the-apt-repository), then:

```bash
sudo apt install xdebbied
```

The package runs bootstrap, which listens 20 seconds for a running cluster:

- **It heard yours** (discovery is on in the cluster and the boxes share a
  LAN): bootstrap stops and tells you to re-run with `--join`. Nothing was
  created. Go to step 3.
- **It heard nothing** (discovery is off, the default): it set the new box
  up as a cluster of its own. Clear that first, in step 2.

To tell which happened:

```bash
sudo test -e /etc/xdebbie/api-ca-key.pem && echo "a cluster of its own: do step 2"
```

## 2. Clear the box's own cluster

Do this only on a new box with no VMs. It removes the identity, config and
libvirt network that the package's bootstrap just made:

```bash
sudo systemctl stop xdebbied
sudo rm -f /etc/xdebbie/xdebbied.toml /etc/xdebbie/xdebbie_env \
           /etc/xdebbie/token /etc/xdebbie/api-*.pem
sudo virsh net-destroy xdebbie
sudo virsh net-undefine xdebbie
```

Joining needs this because bootstrap keeps an existing certificate, token
and network: `--join` on a box that already has them reuses them and does
not enrol.

## 3. Ask to join

On the new box, with the fingerprint from the first box (`xdctl ca` there
prints it):

```bash
sudo xdebbied bootstrap --join https://box0.example.org:23443 \
     --ca-fingerprint sha256:FINGERPRINT
```

Bootstrap fetches the cluster CA, checks the fingerprint, makes a key and a
certificate request, sends it, and waits. It prints the request's key
fingerprint. Leave it running.

## 4. Approve it

On any box already in the cluster:

```bash
xdctl enroll ls
xdctl approve NEW_BOX
```

Check that the fingerprint in `enroll ls` matches what the new box printed.
`NEW_BOX` is the hostname in that list.

Approval signs the certificate and assigns the box a mesh id, the lowest
free one (`1` for the second box). Within seconds, bootstrap on the new box:

- installs the certificate and fetches the cluster token;
- copies the overlay settings and takes its own `/24`: bridge `10.23.1.1`,
  VMs `10.23.1.2` to `10.23.1.254`;
- creates its bridge and writes its config;
- registers the box as a host with the cluster.

Then start the service on the new box:

```bash
sudo systemctl start xdebbied
```

## 5. Connect the mesh

The boxes now trust each other, but their WireGuard underlay has no peers
yet. Peers are per box, so run each command on the box it changes.

On each box, print its public key:

```bash
xdctl mesh wg
```

On box 0, add box 1:

```bash
xdctl mesh peer add 1 --pubkey BOX1_PUBKEY --endpoint box1.example.org:51820
```

On box 1, add box 0:

```bash
xdctl mesh peer add 0 --pubkey BOX0_PUBKEY --endpoint box0.example.org:51820
```

Within a few seconds both boxes show the peer `Up`:

```bash
xdctl mesh ls
```

## 6. Check the cluster

```bash
xdctl host ls
xdctl leader
```

Both hosts are listed. With discovery off (the default) each box leads
alone, so point `XDEBBIE_URL` at the first box from everywhere: `xdctl`
then talks to the box that holds the cluster's VMs. To let the boxes elect
a leader and fail over, turn discovery on; see
[Discovery, election and the leader](../autodiscovery.md).

If something goes wrong, see
[Troubleshooting](../troubleshooting.md#a-box-will-not-join).

## Next

[Create your first VM](first-vm.md).
