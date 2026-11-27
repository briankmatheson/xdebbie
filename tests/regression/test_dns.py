"""What each box's DHCP tells VMs to use for DNS works.

2026-11-26: box 5 handed out 10.23.0.11, eyrie's load-balanced resolver,
which an eyrie node cannot use for itself (Cilium); the joiner's DNS is now
chosen by xdebbied's bootstrap (config_gen.rs joiner_dns).
"""
import ipaddress
import subprocess

import pytest


def answers(server, name):
    r = subprocess.run(["dig", "+time=2", "+tries=1", f"@{server}", name],
                       capture_output=True, text=True, timeout=10)
    return "status: NOERROR" in r.stdout or "status: NXDOMAIN" in r.stdout


def test_no_box_hands_out_a_reserved_address(hosts):
    bad = [f"{h.short}: {d}" for h in hosts for d in h.network.get("dns", [])
           if d in h.network.get("reserved", [])]
    assert not bad, "DNS is a reserved (load-balancer) address: " + ", ".join(bad)


def test_every_box_dns_answers(hosts):
    silent = []
    for h in hosts:
        name = h.network.get("domain") or "localhost"
        for d in h.network.get("dns", []):
            # a box's own bridge forwards (DNAT) only for its VMs, not for us
            if ipaddress.ip_address(d) == h.gateway and h.network.get("upstream_dns"):
                d = h.network["upstream_dns"][0]
            if not answers(d, name):
                silent.append(f"{h.short}: {d}")
    assert not silent, "no DNS answer from: " + ", ".join(silent)
