# DHCP options served

What the built-in DHCP responder puts in its replies, and where each value
comes from.

The responder runs when `[network]` is set and `dhcp` is not `false`. It
answers only MACs of VMs in its store. See
[Overlay addressing and DHCP](../network.md) for how it behaves.

| Option | Name | Value | Sent |
|---|---|---|---|
| 1 | Subnet mask | from `[network] cidr` | always |
| 3 | Router | `gateway` (default: first host of `cidr`) | always |
| 6 | DNS servers | `dns` (default: the gateway) | always |
| 12 | Host name | the VM's name | always |
| 15 | Domain name | `domain` | when set |
| 26 | Interface MTU | the mesh MTU | when the bridge is the mesh's overlay bridge |
| 28 | Broadcast address | from `cidr` | always |
| 42 | NTP servers | `ntp` | when not empty |
| 51 | Lease time | `lease_secs` (default 86400) | OFFER and ACK |
| 54 | Server identifier | this box's address on the bridge, else the gateway | always |
| 56 | Message | the reason | NAK only |
| 58 | Renewal time (T1) | half of `lease_secs` | OFFER and ACK |
| 59 | Rebinding time (T2) | seven eighths of `lease_secs` | OFFER and ACK |

Message types:

| Client sends | Responder answers |
|---|---|
| DISCOVER | OFFER with the VM's stored address |
| REQUEST for the stored address | ACK |
| REQUEST for any other address | NAK, so the guest starts over |
| INFORM | ACK with options, no lease |
| anything from an unknown MAC | nothing |

Replies are broadcast while the client has no address, unicast to
`ciaddr` for renewals and INFORM, and sent to `giaddr` through a relay.
