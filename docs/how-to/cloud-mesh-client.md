# Run a cloud server as a mesh client

Put a cloud server (DigitalOcean here) on the mesh as a client, built with
Terraform: the server can then reach the hub and the overlay
(`10.23.0.0/16`) over WireGuard, while nothing on it is open to the
internet except WireGuard and ssh from your hub's address.

This is for a hub that **cannot be reached from the internet** on UDP (a
home line that passes only a few TCP ports, say). The hub dials the
server instead: the server listens, the hub has its address as the
client's endpoint. A laptop that moves around is the other case; for that
see [Connect a laptop with xdj](mesh-clients.md).

The Terraform is in [`examples/do-mesh-client`](../../examples/do-mesh-client).

## How it works

| Part | What it does |
|---|---|
| `wireguard_asymmetric_key` | Makes the client's WireGuard key, so its public key is known before the server exists. |
| `digitalocean_droplet` | The server. cloud-init installs `wireguard-tools` and `ufw`, writes `/etc/wireguard/wg-flock.conf`, and enables `wg-quick@wg-flock`. |
| `wg-flock.conf` | The client is `10.22.BOX.1/16`. Its only peer is the hub, with `AllowedIPs = 10.22.0.1/32, 10.23.0.0/16` and **no `Endpoint`**: it waits for the hub. |
| `digitalocean_firewall` + ufw | Inbound: UDP 51820 and TCP 22 from `hub_public_ip` only. Everything outbound. |
| output `hub_command` | The `xdctl mesh client add ... --endpoint IP:51820` line that makes the hub dial the server. |

Because the config is an ordinary `wg-quick` unit, the link comes back by
itself after a reboot. No `xdj` runs on the server.

## 1. Fill in the variables

```bash
cd examples/do-mesh-client
cp terraform.tfvars.example terraform.tfvars
```

| Variable | Where it comes from |
|---|---|
| `name` | Any name; it is also the client's name on the hub. |
| `box` | A free box number, 1-254. `xdctl mesh ls` on the hub lists the taken ones (clients show as `Nc`). |
| `hub_pubkey` | `xdctl mesh wg` on the hub, the `pubkey=` value. |
| `hub_public_ip` | The address the hub's traffic leaves your network from: `curl -4 ifconfig.me` on the hub. |
| `ssh_key_ids` | Your DigitalOcean ssh keys (`doctl compute ssh-key list`). |

`DIGITALOCEAN_TOKEN` must be set in the environment.

## 2. Create the server

```bash
terraform init
terraform apply
```

## 3. Register it on the hub

Run the line Terraform printed, on the hub:

```bash
terraform output -raw hub_command
# xdctl mesh client add --box 3 --pubkey ... --name backup2 --endpoint 203.0.113.50:51820
```

Within half a minute the hub dials the server.

## 4. Check it

On the hub, the client has a handshake a few seconds old:

```bash
xdctl mesh ls        # the client's row, e.g. 3c, under WIREGUARD and CLIENTS
```

On the server:

```bash
wg show wg-flock latest-handshakes
ping -c1 10.22.0.1   # the hub over the mesh
```

`never` on either side: see [What can go wrong](#what-can-go-wrong).

## What is open

| From | To | Allowed |
|---|---|---|
| `hub_public_ip` | UDP 51820 | WireGuard |
| `hub_public_ip` | TCP 22 | ssh |
| anywhere else | anything | dropped by DigitalOcean's firewall and by ufw |
| the mesh (`wg-flock`) | the server | what you add (`ufw allow in on wg-flock ...` for a service) |

The server's own services should listen on its mesh address
(`10.22.BOX.1`), not on the public one.

## Keys and Terraform state

The private key is made by Terraform, so it is in the Terraform state.
Keep the state encrypted and out of git (`.gitignore` in the example
already leaves out `*.tfstate` and `terraform.tfvars`): a remote backend
with encryption, or OpenTofu's state encryption. Whoever holds the state
holds the client's mesh identity.

To change the key, `terraform apply -replace=wireguard_asymmetric_key.client`,
then on the hub `xdctl mesh client rm NAME` and the new `hub_command`.

## What can go wrong

- **Do not run `xdj join` or `xdj up` on the server.** They wait for their
  own handshake with the hub, cannot get one (the hub is not reachable from
  the internet), and then take the link down. The hub dials; the server
  only listens.
- **The hub's API port is not WireGuard.** `HOST:23443` is the HTTPS API
  (TCP). WireGuard is UDP 51820.
- **`never` on the hub**: the server is not up yet (cloud-init takes a
  minute or two), `hub_public_ip` is not where the hub's traffic really
  comes from, or the endpoint in `hub_command` is wrong.
- **Added as a peer instead of a client**: a client gets no Geneve tunnel;
  a peer would show `Down` for ever and make the mesh `Degraded`. See
  [A client that was added as a peer](mesh-clients.md#a-client-that-was-added-as-a-peer).

## Remove it

```bash
xdctl mesh client rm NAME     # on the hub
terraform destroy
```
