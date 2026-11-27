"""VMs reach the internet, wherever their address came from.

2026-11-26: a box masqueraded only its own /24, so a VM holding another
box's address (eyrie-4: 10.23.0.5 on box 5; the w10 nodes: 10.23.5.x on
timesink) left unNATed. Now the whole overlay (bootstrap/firewall.rs).
Checked from inside each VM we can ssh into; the rest are skipped.
"""
import pytest

from mesh import ssh

# -4: the overlay is IPv4-only (a v6 answer would fail for that alone).
# `|| true`: ssh then fails only when it cannot get in (that VM is skipped)
PROBE = "curl -4 -s -o /dev/null -m 8 -w %{http_code} https://deb.debian.org/ || true"


def test_vms_reach_the_internet(vms):
    tried, failed = 0, []
    for v in vms:
        try:
            code = ssh(v.ip, PROBE, timeout=20).strip()
        except Exception:  # noqa: BLE001 - no ssh into this VM from here
            continue
        tried += 1
        if not code.startswith(("2", "3")):
            failed.append(f"{v.name} {v.ip}: HTTP {code or 'none'}")
    if not tried:
        pytest.skip("no VM accepts ssh from here")
    assert not failed, "; ".join(failed)

