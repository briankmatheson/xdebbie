# A laptop on the mesh with xdj

Put a laptop on the mesh so it can reach the VMs, with one WireGuard link
to a box.

[xdj](https://github.com/briankmatheson/xdj) makes one WireGuard link to a
box, the hub, and routes the overlay (`10.23.0.0/16`) through it. The
laptop is a client, not a box: it gets no Geneve tunnel, and the mesh is
not `Degraded` while it is away. The hub is `box0.example.org` below.

## What you need

- xdj installed on the laptop.
- A box to be the hub, which the laptop reaches on UDP 51820.

## Steps

1. Make a key on the laptop. It prints the public key, `KEY` below:

   ```bash
   xdj key
   ```

2. Register the laptop on the hub. It takes the lowest free number (here
   3) and prints the line to run on the laptop:

   ```bash
   xdctl mesh client add --pubkey KEY --name laptop --hub-endpoint box0.example.org
   ```

3. Let routed clients reach the hub's VMs. By default the hub drops new
   connections into the VM bridge from any other interface. On the hub:

   ```bash
   source /etc/xdebbie/xdebbie_env
   curl --cacert "$XDEBBIE_CA" -H "Authorization: Bearer $XDEBBIE_TOKEN" \
        -H 'Content-Type: application/json' \
        -X PUT -d '{"mesh_routed": true}' "$XDEBBIE_URL/v1/settings"
   ```

   `XDEBBIE_URL` must be the hub's own URL: settings are per box. To keep
   it in the config instead, set `mesh_routed = true` in `[network]`.

4. Join from the laptop, with the line the hub printed:

   ```bash
   xdj join 3 HUB_PUBKEY box0.example.org:51820
   ```

5. Check it on the hub:

   ```bash
   xdctl mesh ls
   ```

## What you should see

Step 2 prints:

```text
added xdj client box 3 (10.22.3.1)

run on the client:
  xdj join 3 HUB_PUBKEY box0.example.org:51820
```

with the hub's real public key in place of `HUB_PUBKEY`. After step 4,
`xdctl mesh ls` on the hub lists the laptop under `CLIENTS (xdj)` with a
handshake a few seconds old, and the laptop reaches `10.23.0.0/16`.

To remove it: `xdctl mesh client rm laptop` (the number, the name or the
key all work).

## If it goes wrong

- If you added the laptop with `xdctl mesh peer add`, then it shows `Down`
  for ever, because it never answers Geneve probes. Move it, keeping its
  number: `xdctl mesh peer rm 3`, then
  `xdctl mesh client add --box 3 --pubkey KEY`.
- If the handshake works but no VM answers, then `mesh_routed` is off on
  the hub: `curl ... "$XDEBBIE_URL/v1/settings"` without `-X PUT` shows it.
- If `--box N` is refused, then that number is taken by a box, a peer or
  another client.

## Reference

[Clients and the hub](../network/clients.md) (how routing works, DNS for
clients, a cloud server as a client),
[`xdctl mesh client`](../reference/xdctl.md#mesh),
[`/v1/settings`](../reference/api.md#settings).
