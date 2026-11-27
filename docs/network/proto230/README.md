# Proto 230, in pictures

Seven small [CALM](https://calm.finos.org) diagrams of how xdebbied owns its
mesh routes, for people rather than tools. Each has one flow you can step
through on the CALM hub (namespace `aviary.proto230`).

| File | The story |
|---|---|
| `proto230-mesh-at-a-glance.calm.json` | Two boxes, three mesh networks, one laptop: who carries what. |
| `proto230-owned-routes.calm.json` | One mesh pass: xdebbied finds its routes by `proto 230` and its blackholes by metric 4294967295, and touches nothing else. |
| `proto230-vm-to-pod.calm.json` | A packet from a VM on box 5 to a pod on box 0. |
| `proto230-tunnel-drops.calm.json` | A link goes away: the blackhole holds and nothing leaks to the default route. |
| `proto230-laptop-joins.calm.json` | A laptop joins with xdj, talks to box 5, and the reply comes back over a proto 230 route. |
| `proto230-auto-macs.calm.json` | Every MAC is worked out from a VM id or a box number: libvirt, the VM's network config, DHCP and the FDB all use it, and there is no table. |
| `proto230-renumbering.calm.json` | A box changes number during an upgrade, and learn mode keeps VM traffic flowing. Both numbers answering at once is marked planned: it is not built yet. |

The prose is [Routes and firewall](../routes-and-firewall.md) and
[Clients and the hub](../clients.md). Change `make.py`, not the JSON:

```bash
python3 docs/network/proto230/make.py
npx @finos/calm-cli@1.67.0 validate -a docs/network/proto230/proto230-mesh-at-a-glance.calm.json
```
