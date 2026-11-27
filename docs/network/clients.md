# Clients and the hub

Laptops and outside servers join the mesh as clients: one WireGuard link
to a box, the hub, and the overlay routed through it.

A client is not a box. It has a number and an underlay address like a box
(`10.22.N.1`), but no Geneve tunnel, no probe and no FDB entries, and its
absence never makes the mesh `Degraded`. The client software is
[xdj](https://github.com/briankmatheson/xdj); a plain `wg-quick` setup
works too.

| | peer (a box) | client |
|---|---|---|
| WireGuard peer, `allowed-ips 10.22.N.1/32`, keepalive | yes | yes |
| Geneve tunnel `gnv-bN`, FDB entries, probe | yes | no |
| Row in `status.peers`, counts for Ready and Degraded | yes | no |
| Row in `status.wireguard.peers` | yes | yes, with `"client": true` and its `name` |
| `endpoint` | usually | optional: clients call the hub |

## What you need

- A hub: any box with the managed WireGuard underlay (the default).
- The client's WireGuard public key.

## Add and remove a client

The walk-through is [A laptop on the mesh](../getting-started/laptop.md).
In short, on the hub:

```bash
xdctl mesh client add --pubkey KEY --name laptop --hub-endpoint box0.example.org
xdctl mesh client rm laptop
```

`client add` picks the lowest free number from 1 to 254 that is not this
box, a peer or another client; `--box N` asks for one and is refused if
taken. Nothing is ever replaced. Adding a key that is already a client just
reports its number. The printed `xdj join` line carries the hub's public
key and `HOST:PORT`; without `--hub-endpoint` it prints the placeholder
`HUB_HOST:PORT` for you to fill in.

The rules, checked on every change: clients need the managed underlay;
`box` is 1-254 and unique across this box, its peers and clients; `pubkey`
is a 44-character WireGuard key, unique across peers and clients; `name`
is optional, letters, digits, `.`, `_` and `-`, not all digits, and unique.
`xdctl mesh peer add` refuses a number that belongs to a client.

`client rm` takes the number, the name or the key. It removes the WireGuard
peer and nothing else.

## Letting clients reach VMs: mesh_routed

The hub forwards a client's traffic from `wg-flock` into the overlay. By
default it drops new flows into the bridge from any interface but the
bridge itself, so turn on `mesh_routed` on the hub:

```bash
curl --cacert "$XDEBBIE_CA" -H "Authorization: Bearer $XDEBBIE_TOKEN" \
     -H 'Content-Type: application/json' \
     -X PUT -d '{"mesh_routed": true}' "$XDEBBIE_URL/v1/settings"
```

That adds one nftables accept (from `10.22.0.0/16` on the underlay) in
`ip xdebbie_host` and one ufw route rule, and nothing else. It is kept in
`settings.json` and survives restarts. `mesh_routed = true` in `[network]`
does the same from the config. A box without `[network]` refuses it with
409.

## Clients behind another box

A client reaches every box's VMs through its hub and the overlay. The
replies are the problem: a VM on box 1 answers through box 1, whose route
for `10.22.0.0/16` points into WireGuard, where box 1 has no peer for the
client. So a box with no clients of its own and at least one peer sends
the underlay back through its hub's bridge address:

| Route | Why |
|---|---|
| `10.22.PEER.1/32 dev wg-flock` | each peer's underlay stays in WireGuard (Geneve, probes) |
| `10.22.0.0/17` and `10.22.128.0/17 via 10.23.HUB.1 dev virbr-xdebbie` | everything else (clients) goes back the way it came; two halves beat the kernel's `/16` |

The hub is the lowest-numbered peer, normally box 0. A box with clients of
its own is a hub and adds none of these. They carry `proto 230`
(`ip route show proto 230`), are converged on every mesh pass and removed
on teardown. When the bridge goes, the `/17`s go with it and the
[blackhole route](routes-and-firewall.md#blackhole-routes-for-the-mesh-networks) for
`10.22.0.0/16` catches what they carried.

## What clients are told: DNS, search, welcome

A hub tells its clients which resolver answers which DNS zone, the search
domains to try short names in (in order), and a welcome banner. xdj fetches
it after each handshake from `GET /v1/client-config/xdj` and keeps a copy.

Anyone who can reach the API can read it, so put nothing secret there.

1. Set it while running:

   ```bash
   xdctl mesh client-config set \
       --zone mesh.example.org=10.23.0.11 \
       --zone int.example.org=10.23.0.243 \
       --search mesh.example.org,int.example.org,example.org \
       --welcome-file welcome.txt
   ```

2. Check it:

   ```bash
   xdctl mesh client-config
   ```

Each option given replaces that part; `--zone none` and `--search none`
clear, `--clear-welcome` drops the banner. It is kept in
`/var/lib/xdebbie/client-config.json`, which overrides `[clients]` in the
config from then on. The same in the config:

```toml
[clients]
search = ["mesh.example.org", "int.example.org", "example.org"]
welcome = """
Welcome to the example mesh.
"""

[[clients.dns]]
zone = "mesh.example.org"
server = "10.23.0.11"
```

It is checked: zones and search domains are DNS names (63 bytes a label,
253 in all), each zone once, servers are IP addresses, and the banner is
printable text (tabs and newlines allowed) of at most 4096 bytes. Each hub
answers for itself; nothing is forwarded.

## A cloud server as a client

For a hub that cannot be reached from the internet on UDP (a home line, for
instance), turn it round: the server listens, and the hub dials it. The
Terraform in [`examples/do-mesh-client`](../../examples/do-mesh-client)
builds one on DigitalOcean.

| Part | What it does |
|---|---|
| `wireguard_asymmetric_key` | the client's key, so its public key is known before the server exists |
| `digitalocean_droplet` | the server; cloud-init installs `wireguard-tools` and `ufw`, writes `/etc/wireguard/wg-flock.conf` and enables `wg-quick@wg-flock` |
| `wg-flock.conf` | the client is `10.22.BOX.1/16`; its one peer is the hub, `AllowedIPs = 10.22.0.1/32, 10.23.0.0/16`, and no `Endpoint`: it waits for the hub |
| `digitalocean_firewall` and ufw | inbound UDP 51820 and TCP 22 from `hub_public_ip` only |
| output `hub_command` | the `xdctl mesh client add ... --endpoint IP:51820` line that makes the hub dial the server |

1. Fill in the variables:

   ```bash
   cd examples/do-mesh-client
   cp terraform.tfvars.example terraform.tfvars
   ```

   `name` (also the client's name on the hub), `box` (a free number,
   1-254; clients show as `Nc` in `xdctl mesh ls`), `hub_pubkey`
   (`xdctl mesh wg` on the hub), `hub_public_ip` (`curl -4 ifconfig.me` on
   the hub), `ssh_key_ids` (`doctl compute ssh-key list`). Set
   `DIGITALOCEAN_TOKEN` in the environment.

2. Create the server:

   ```bash
   terraform init
   terraform apply
   ```

3. Register it on the hub with the line Terraform printed:

   ```bash
   terraform output -raw hub_command
   ```

4. Check, within half a minute. On the hub, `xdctl mesh ls` shows the
   client with a fresh handshake. On the server:

   ```bash
   wg show wg-flock latest-handshakes
   ping -c1 10.22.0.1
   ```

The private key is in the Terraform state: keep the state encrypted and
out of git (the example's `.gitignore` leaves out `*.tfstate` and
`terraform.tfvars`). To change the key:
`terraform apply -replace=wireguard_asymmetric_key.client`, then
`xdctl mesh client rm NAME` on the hub and the new `hub_command`. To remove
it: `xdctl mesh client rm NAME`, then `terraform destroy`.

The server's own services should listen on `10.22.BOX.1`, and you open
them with `ufw allow in on wg-flock ...`.

## If it goes wrong

- If you run `xdj join` or `xdj up` on such a server, then it waits for its
  own handshake, gets none (the hub is not reachable), and takes the link
  down. The hub dials; the server only listens.
- If the hub shows `never`, then the server is not up yet (cloud-init takes
  a minute or two), `hub_public_ip` is not where the hub's traffic really
  comes from, or the endpoint in `hub_command` is wrong.
- If a client was added as a peer, then it is `Down` for ever and the mesh
  `Degraded`: `xdctl mesh peer rm N`, then `xdctl mesh client add --box N --pubkey KEY`.
- If you aim WireGuard at port 23443, then remember that is the HTTPS API
  (TCP). WireGuard is UDP 51820.

## Reference

[`[[mesh.clients]]` and `[clients]`](../reference/configuration.md#meshclients),
[`xdctl mesh client`](../reference/xdctl.md#mesh),
[`/v1/client-config`](../reference/api.md#client-configuration-xdj),
[`/v1/settings`](../reference/api.md#settings).
