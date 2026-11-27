"""Boxes other than the hub send this client's traffic back through the hub.

The route a box picks for this machine's mesh address must leave by the
overlay bridge towards the hub's bridge address, not into WireGuard (where
the box has no peer for it). The hub itself reaches us over WireGuard.
"""
import json

from mesh import HUB, UNDERLAY, ssh


def route_to(host, ip):
    return json.loads(ssh(host.ssh, f"ip -j route get {ip}"))[0]


def test_other_boxes_route_clients_via_the_hub(hosts, here):
    hub = next((h for h in hosts if h.short == HUB), None)
    assert hub, f"hub {HUB} is not among the hosts"
    wrong = []
    for h in hosts:
        r = route_to(h, here)
        if h is hub:
            ok = r.get("dev") == UNDERLAY
        else:
            ok = r.get("gateway") == str(hub.gateway) and r.get("dev") == h.network.get("bridge")
        if not ok:
            wrong.append(f"{h.short}: {r.get('dev')} via {r.get('gateway')}")
    assert not wrong, f"route to {here}: " + "; ".join(wrong)

