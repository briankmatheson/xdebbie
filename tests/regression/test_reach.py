"""Every running VM can be reached from here, whichever box it runs on.

2026-11-26: VMs on box 5 (simplyaviary) were unreachable from gram, an xdj
client of timesink: box 5 sent the replies into WireGuard, where no peer
carries gram's address. Fixed by xdebbied's routes back to other boxes'
clients (crates/xdebbied/src/mesh/routes.rs).
"""
from mesh import tcp_reaches

PORTS = (22, 10250)     # ssh, then kubelet: any answer proves the path


def test_every_vm_answers(vms):
    silent = [f"{v.name} {v.ip} on {v.host}" for v in vms
              if not any(tcp_reaches(v.ip, p) for p in PORTS)]
    assert not silent, "no answer on 22 or 10250 from: " + ", ".join(silent)

