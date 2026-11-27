#!/usr/bin/env python3
"""Proto 230, in pictures: five small CALM architectures for people, cut
the way the FINOS examples on the CALM hub are (7-15 nodes, one box, one
sentence per label, a flow per story). The user, 2026-11-27: "a series of
CALM compliant arch diags built for human consumption and describing proto
230."

    python3 docs/network/proto230/make.py          # writes the *.calm.json beside it
    npx @finos/calm-cli@1.67.0 validate -a docs/network/proto230/<view>.calm.json

The facts come from crates/xdebbied/src/mesh/routes.rs and kernel.rs,
docs/network/clients.md and routes-and-firewall.md, and xdj's src/os.rs;
checked live against timesink and simplyaviary on 2026-11-27.
"""
import json
import pathlib

HERE = pathlib.Path(__file__).parent
SCHEMA = "https://calm.finos.org/release/1.1/meta/calm.json"


def view(uid, name, description, box, nodes, rels, flows):
    """One architecture. `box` is (id, [members]); nodes {id: (type, name, sentence)};
    rels [(id, source, destination, sentence, protocol or None)];
    flows [(id, name, sentence, [(rel id, sentence)])]."""
    out_nodes = [{"unique-id": i, "node-type": t, "name": n, "description": d} for i, (t, n, d) in nodes.items()]
    out_rels = [{"unique-id": f"{box[0]}-has-{m}", "description": f"{nodes[box[0]][1]} has {nodes[m][1]}.",
                 "relationship-type": {"composed-of": {"container": box[0], "nodes": [m]}}} for m in box[1]]
    for rid, src, dst, sentence, proto in rels:
        r = {"unique-id": rid, "description": sentence,
             "relationship-type": {"connects": {"source": {"node": src}, "destination": {"node": dst}}}}
        if src == "you":
            r["relationship-type"] = {"interacts": {"actor": src, "nodes": [dst]}}
        if proto:
            r["protocol"] = proto
        out_rels.append(r)
    known = {r["unique-id"] for r in out_rels}
    out_flows = []
    for fid, fname, fdesc, steps in flows:
        for rid, _ in steps:
            assert rid in known, (uid, fid, rid)
        out_flows.append({"unique-id": fid, "name": fname, "description": fdesc,
                          "transitions": [{"relationship-unique-id": rid, "sequence-number": n, "description": s}
                                          for n, (rid, s) in enumerate(steps, 1)]})
    return {"$schema": SCHEMA, "unique-id": uid, "name": name, "description": description,
            "metadata": [{"source": "xdebbie docs/network/proto230/make.py"}, {"checked-live": "2026-11-27"}],
            "nodes": out_nodes, "relationships": out_rels, "flows": out_flows}


# 1. The mesh at a glance ----------------------------------------------------
GLANCE = view(
    "proto230-mesh-at-a-glance", "Proto 230: The mesh at a glance",
    "Two boxes, three mesh networks, and the one laptop: who carries what.",
    ("mesh", ["timesink", "simplyaviary", "underlay", "overlay", "storage"]),
    {
        "mesh": ("system", "The mesh", "Every box xdebbied joins, tied together by WireGuard."),
        "timesink": ("system", "timesink (box 0, the hub)", "The founding box: lowest number, so it is the hub, and clients connect here."),
        "simplyaviary": ("system", "simplyaviary (box 5)", "A box with no clients of its own, so it keeps proto 230 routes back to the hub."),
        "underlay": ("network", "Underlay 10.22.0.0/16", "WireGuard link wg-flock; box N is 10.22.N.1."),
        "overlay": ("network", "Overlay 10.23.0.0/16", "One bridge, virbr-xdebbie, stretched across boxes by Geneve; box N is 10.23.N.1."),
        "storage": ("network", "Storage 10.24.0.0/16", "The bridge virbr-abe, where VMs reach abe's disks."),
        "vms": ("system", "VMs", "w10's nodes and eyrie, on the overlay bridge."),
        "laptop": ("system", "gram (xdj client 1)", "A laptop on the mesh: one WireGuard link to the hub, 10.22.1.1."),
        "default": ("network", "Default route", "The LAN and the internet: where mesh traffic must never go."),
    },
    [
        ("ts-underlay", "timesink", "underlay", "timesink is 10.22.0.1 on wg-flock.", None),
        ("sa-underlay", "simplyaviary", "underlay", "simplyaviary is 10.22.5.1 on wg-flock.", None),
        ("overlay-in-underlay", "overlay", "underlay", "Geneve (udp 6081) carries the bridge between boxes, inside WireGuard.", None),
        ("ts-overlay", "timesink", "overlay", "timesink's bridge address is 10.23.0.1.", None),
        ("sa-overlay", "simplyaviary", "overlay", "simplyaviary's bridge address is 10.23.5.1.", None),
        ("vms-overlay", "vms", "overlay", "VMs get 10.23 addresses by DHCP from their box.", None),
        ("vms-storage", "vms", "storage", "VMs reach their disks on 10.24.", "TCP"),
        ("laptop-hub", "laptop", "timesink", "The laptop talks WireGuard to the hub only.", None),
        ("sa-routes-back", "simplyaviary", "timesink", "proto 230: 10.22.0.0/17 and 10.22.128.0/17 via 10.23.0.1, so replies to clients go back through the hub.", None),
        ("boxes-default", "mesh", "default", "Only non-mesh traffic leaves this way; blackholes catch the rest.", None),
    ],
    [("glance", "Who routes what", "A tour of the mesh, network by network.", [
        ("ts-underlay", "Every box has an underlay address on wg-flock."),
        ("overlay-in-underlay", "The overlay rides inside the underlay."),
        ("vms-overlay", "VMs live on the overlay."),
        ("laptop-hub", "A laptop joins through the hub."),
        ("sa-routes-back", "Other boxes route client replies back through the hub: proto 230."),
        ("boxes-default", "Nothing for 10.22, 10.23 or 10.24 ever takes the default route."),
    ])],
)

# 2. How a route gets owned ---------------------------------------------------
OWNED = view(
    "proto230-owned-routes", "Proto 230: How a route gets owned",
    "xdebbied marks its routes with proto 230 and its blackholes with the worst metric, and touches nothing else.",
    ("table", ["p230-peer", "p230-halves", "blackholes", "connected", "others"]),
    {
        "spec": ("data-asset", "Mesh spec", "Peers, clients and blackholes, from the config or the API."),
        "xdebbied": ("service", "xdebbied", "The daemon on each box; its mesh pass runs every few seconds."),
        "reconcile": ("service", "Mesh pass", "Works out the wanted routes, compares, and fixes the difference."),
        "table": ("network", "Kernel routing table (simplyaviary)", "What `ip route` shows on box 5."),
        "p230-peer": ("network", "10.22.0.1/32 dev wg-flock", "proto 230: each peer's underlay stays in WireGuard, for Geneve and probes."),
        "p230-halves": ("network", "10.22.0.0/17, 10.22.128.0/17 via 10.23.0.1", "proto 230: everything else on the underlay (clients) goes via the hub's bridge address."),
        "blackholes": ("network", "Blackholes, metric 4294967295", "10.22, 10.23, 10.24 /16: the worst metric, which is also how xdebbied knows they are its own."),
        "connected": ("network", "Connected routes (proto kernel)", "10.22.0.0/16 on wg-flock, 10.23.0.0/16 on the bridge: the kernel's, not ours."),
        "others": ("network", "Everyone else's routes", "The default route, DHCP's, yours: xdebbied never lists or changes them."),
    },
    [
        ("read-spec", "xdebbied", "spec", "xdebbied reads the mesh spec each pass.", None),
        ("run-pass", "xdebbied", "reconcile", "Each pass computes the wanted routes from the spec.", None),
        ("list-ours", "reconcile", "table", "It lists only `ip -j route show proto 230`.", None),
        ("del-unwanted", "reconcile", "p230-halves", "A proto 230 route no longer wanted is deleted.", None),
        ("replace-wanted", "reconcile", "p230-peer", "A wanted route that is missing or different is set with `ip route replace ... proto 230`.", None),
        ("blackhole-add", "reconcile", "blackholes", "Missing blackholes are added (plan action BlackholeAdd); unlisted ones of ours removed (BlackholeDel).", None),
        ("connected-wins", "connected", "blackholes", "Any real route beats a blackhole: metric 0 against 4294967295.", None),
    ],
    [("owned", "One mesh pass", "How box 5 ends up with exactly its own routes, and nobody else's are touched.", [
        ("read-spec", "Read the spec: one peer (box 0), no clients."),
        ("run-pass", "Wanted: the peer's /32 and the two /17 halves."),
        ("list-ours", "Have: whatever carries proto 230, and nothing more."),
        ("del-unwanted", "Remove ours that are no longer wanted."),
        ("replace-wanted", "Set the missing ones; a failure is logged and retried next pass."),
        ("blackhole-add", "Make sure the blackholes are there, at the worst metric."),
        ("connected-wins", "While the links are up, the real routes win."),
    ])],
)

# 3. A packet from a VM on box 5 to a pod on box 0 ----------------------------
PACKET = view(
    "proto230-vm-to-pod", "Proto 230: A packet from box 5 to box 0",
    "A VM on simplyaviary talks to a pod on timesink: one bridge across two boxes, and proto 230 only keeps the tunnel in WireGuard.",
    ("box5", ["vm5", "br5", "gnv5", "wg5"]),
    {
        "box5": ("system", "simplyaviary (box 5)", "Where the packet starts."),
        "vm5": ("system", "VM on box 5", "A w10 worker on simplyaviary, 10.23.5.x."),
        "br5": ("network", "virbr-xdebbie", "The overlay bridge on simplyaviary, 10.23.5.1."),
        "gnv5": ("network", "gnv-b0", "The Geneve tunnel from box 5 to box 0."),
        "wg5": ("network", "wg-flock", "WireGuard on simplyaviary, 10.22.5.1."),
        "wg0": ("network", "timesink wg-flock + gnv-b5", "WireGuard and Geneve on timesink, 10.22.0.1."),
        "br0": ("network", "timesink virbr-xdebbie", "The same overlay bridge, on timesink, 10.23.0.1."),
        "worker0": ("system", "w10 worker on box 0", "A VM on timesink, 10.23.0.x."),
        "pod": ("service", "Pod", "Runs inside the worker; the cluster's own network takes it from here."),
    },
    [
        ("vm-to-br", "vm5", "br5", "The VM sends to a 10.23 address: same network, so straight onto the bridge.", None),
        ("br-to-gnv", "br5", "gnv5", "The bridge's forwarding table says that address lives behind box 0's tunnel.", None),
        ("gnv-to-wg", "gnv5", "wg5", "Geneve wraps the frame in udp 6081 to 10.22.0.1, and the proto 230 /32 keeps that in WireGuard.", None),
        ("wg-to-wg", "wg5", "wg0", "WireGuard encrypts it to timesink, which unwraps WireGuard, then Geneve.", None),
        ("wg-to-br", "wg0", "br0", "The frame lands on timesink's half of the bridge.", None),
        ("br-to-worker", "br0", "worker0", "The bridge delivers it to the worker VM.", None),
        ("worker-to-pod", "worker0", "pod", "The worker hands it to the pod.", None),
    ],
    [("vm-to-pod", "VM on box 5 to a pod on box 0", "Follow one packet out of a VM and into a pod on another box.", [
        ("vm-to-br", "Onto the local bridge."),
        ("br-to-gnv", "Into the Geneve tunnel for box 0."),
        ("gnv-to-wg", "Into WireGuard: the one place proto 230 shows up, the peer's /32."),
        ("wg-to-wg", "Across the wire, encrypted, and unwrapped on timesink."),
        ("wg-to-br", "Onto timesink's bridge."),
        ("br-to-worker", "Into the worker."),
        ("worker-to-pod", "Into the pod."),
    ])],
)

# 4. A tunnel drops -----------------------------------------------------------
DROP = view(
    "proto230-tunnel-drops", "Proto 230: A tunnel drops",
    "When wg-flock or the bridge goes away, the blackholes hold and nothing leaks to the default route.",
    ("table", ["connected", "p230", "blackhole", "default"]),
    {
        "sender": ("service", "Something on the box", "A VM, abe, or anything sending to a mesh address."),
        "link": ("network", "wg-flock / virbr-xdebbie", "The link that just went away: torn down, deleted, mesh disabled."),
        "table": ("network", "Kernel routing table", "What is left once the link is gone."),
        "connected": ("network", "10.22.0.0/16 dev wg-flock", "Gone: the kernel removes a connected route with its link."),
        "p230": ("network", "proto 230 /17s via 10.23.0.1", "Gone too: they go through the bridge, and the bridge is gone."),
        "blackhole": ("network", "blackhole 10.22.0.0/16 metric 4294967295", "Still there: teardown keeps it on purpose."),
        "default": ("network", "default via the LAN router", "0.0.0.0/0, the shortest prefix there is."),
        "internet": ("network", "LAN and internet", "Where mesh traffic would leak without the blackhole."),
        "dropped": ("service", "Dropped", "A local sender gets EINVAL; a forwarded packet just disappears, no ICMP."),
    },
    [
        ("link-carried", "link", "connected", "The link held the connected route.", None),
        ("bridge-carried", "link", "p230", "The bridge held the proto 230 /17s.", None),
        ("sends", "sender", "table", "Something sends to 10.22.1.1 after the link is gone.", None),
        ("lookup", "table", "blackhole", "Longest prefix wins: the /16 blackhole beats the default /0.", None),
        ("drop", "blackhole", "dropped", "The blackhole drops it.", None),
        ("never", "default", "internet", "The default route only ever sees traffic that is not for the mesh.", None),
    ],
    [("drops", "A tunnel drops", "What happens to mesh traffic the moment its link goes.", [
        ("link-carried", "The link goes, and its connected route with it."),
        ("bridge-carried", "The proto 230 routes through the bridge go too."),
        ("sends", "Traffic for the mesh keeps coming."),
        ("lookup", "The kernel looks up the route: the blackhole is the best match left."),
        ("drop", "Dropped, right here."),
        ("never", "Nothing for the mesh reaches the LAN or the internet."),
    ])],
)

# 5. A laptop joins with xdj ---------------------------------------------------
LAPTOP = view(
    "proto230-laptop-joins", "Proto 230: A laptop joins with xdj",
    "gram joins through the hub, reaches a VM on box 5, and the reply finds its way back over a proto 230 route.",
    ("hub", ["xdctl", "hub-wg", "routed", "hub-br"]),
    {
        "you": ("actor", "You", "The admin, at the hub and at the laptop."),
        "hub": ("system", "timesink (the hub)", "Box 0; clients connect here."),
        "xdctl": ("service", "xdctl mesh client add", "Gives the laptop a number (1) and prints its `xdj join` line."),
        "hub-wg": ("network", "wg-flock (box 0)", "The hub's WireGuard; the laptop is a peer with allowed-ips 10.22.1.1/32."),
        "routed": ("service", "mesh_routed", "One nftables accept on the hub: from 10.22.0.0/16 on wg-flock into the bridge."),
        "hub-br": ("network", "virbr-xdebbie (box 0)", "10.23.0.1: where the hub meets the overlay."),
        "xdj": ("service", "xdj (on gram)", "The mesh client: blackholes first, then the link, then routes via the hub."),
        "laptop-bh": ("network", "Blackholes on gram", "10.22 and 10.23 /16 at metric 4294967295, before the link comes up."),
        "vm5": ("system", "VM on box 5", "Any VM on simplyaviary, 10.23.5.x."),
        "sa-p230": ("network", "proto 230 /17s on box 5", "10.22.0.0/17 and 10.22.128.0/17 via 10.23.0.1."),
    },
    [
        ("add-client", "you", "xdctl", "You add the laptop's key on the hub.", None),
        ("join", "you", "xdj", "You run the printed `xdj join` line on the laptop.", None),
        ("bh-first", "xdj", "laptop-bh", "xdj puts the blackholes in before anything else, so nothing leaks while the link comes up.", None),
        ("xdj-to-hub", "xdj", "hub-wg", "WireGuard to the hub; routes 10.23.0.0/16 via 10.22.0.1.", None),
        ("hub-forward", "hub-wg", "routed", "The hub forwards the laptop's packet toward the bridge.", None),
        ("into-bridge", "routed", "hub-br", "mesh_routed lets it onto the overlay.", None),
        ("to-vm", "hub-br", "vm5", "Across Geneve to box 5's bridge and the VM.", None),
        ("reply", "vm5", "sa-p230", "The VM answers 10.22.1.1; box 5 has no WireGuard peer for it.", None),
        ("back-via-hub", "sa-p230", "hub-br", "proto 230 sends it back via 10.23.0.1, the way it came.", None),
        ("home", "hub-br", "hub-wg", "The hub hands it to WireGuard, and the laptop has its answer.", None),
    ],
    [("joins", "A laptop joins and talks to box 5", "From `client add` to a reply from a VM on another box.", [
        ("add-client", "Add the client on the hub."),
        ("join", "Join on the laptop."),
        ("bh-first", "Blackholes first."),
        ("xdj-to-hub", "Up to the hub."),
        ("hub-forward", "The hub forwards it."),
        ("into-bridge", "Onto the overlay."),
        ("to-vm", "Over to box 5."),
        ("reply", "The VM answers."),
        ("back-via-hub", "Box 5's proto 230 route sends the reply to the hub."),
        ("home", "Back to the laptop."),
    ])],
)

VIEWS = [GLANCE, OWNED, PACKET, DROP, LAPTOP]

if __name__ == "__main__":
    for v in VIEWS:
        (HERE / f"{v['unique-id']}.calm.json").write_text(json.dumps(v, indent=2) + "\n")
        print(f"{v['unique-id']:30} {len(v['nodes'])} nodes, {len(v['relationships'])} relationships, {len(v['flows'])} flow")
