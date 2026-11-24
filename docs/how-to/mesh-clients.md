# Connect a laptop with xdj

Join a laptop or a server outside the cluster to the mesh, so it can reach
the VMs.

[xdj](https://github.com/briankmatheson/xdj) makes one WireGuard link to a
box, the hub, and routes the overlay (`10.23.0.0/16`) through it. The
client is not a box: it gets no Geneve tunnel and never makes the mesh
`Degraded` when it is away. [The mesh page](../mesh.md#clients-xdj)
explains the details.

The hub is `box0.example.org` in the examples.

## 1. Make a key on the client

```bash
xdj key
```

It prints the client's public key, `KEY` below.

## 2. Register the client on the hub

On the hub:

```bash
xdctl mesh client add --pubkey KEY --name laptop --hub-endpoint box0.example.org
```

It picks the lowest free number for the client (here `3`) and prints the
line to run on the client:

```text
added xdj client box 3 (10.22.3.1)

run on the client:
  xdj join 3 HUB_PUBKEY box0.example.org:51820
```

`--box N` asks for a specific number; it is refused if taken.

## 3. Let routed clients reach VMs

By default the hub drops new connections into the VM bridge from any other
interface. Allow the mesh's routed peers through, on the hub:

```bash
curl --cacert "$XDEBBIE_CA" -H "Authorization: Bearer $XDEBBIE_TOKEN" \
     -H 'Content-Type: application/json' \
     -X PUT -d '{"mesh_routed": true}' "$XDEBBIE_URL/v1/settings"
```

`$XDEBBIE_URL` must be the hub's own URL: settings are per box. To make it
permanent in the config instead, set `mesh_routed = true` in `[network]`.

## 4. Join from the client

Run the line the hub printed:

```bash
xdj join 3 HUB_PUBKEY box0.example.org:51820
```

On the hub, `xdctl mesh ls` lists the client with its last handshake.

## Tell clients about your DNS

A hub can hand its clients DNS zones, search domains and a welcome banner.
Anyone who can reach the API can read them, so put nothing secret there.

```bash
xdctl mesh client-config set \
    --zone mesh.example.org=10.23.0.11 \
    --search mesh.example.org,example.org \
    --welcome 'Welcome to the example mesh.'
xdctl mesh client-config
```

Or set `[clients]` in the config; see the
[configuration reference](../reference/configuration.md#clients).

## Remove a client

```bash
xdctl mesh client rm laptop
```

It takes the number, the name or the key.

## A client that was added as a peer

A client added with `xdctl mesh peer add` shows `Down` forever, because it
never answers Geneve probes. Move it, keeping its number:

```bash
xdctl mesh peer rm BOX
xdctl mesh client add --box BOX --pubkey KEY
```
