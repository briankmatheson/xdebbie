// SPDX-License-Identifier: GPL-3.0-or-later
//! The pure part of the DHCP responder: one request in, one decision out.
//! No sockets, no clocks, no store — just `(request, binding, config)`.

use std::net::{Ipv4Addr, SocketAddrV4};

use uuid::Uuid;

use super::model::NetworkConfig;
use super::wire::{self, opt, MsgType, Reply, Request};

/// What the store knows about a MAC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub vm: Uuid,
    pub name: String,
    pub ip: Ipv4Addr,
}

/// Static per-server parameters, derived once from config + runtime facts.
#[derive(Debug, Clone)]
pub struct Params {
    /// Option 54 and `siaddr`: this box's address on the bridge.
    pub server_id: Ipv4Addr,
    pub net: NetworkConfig,
    /// Option 26, when the bridge is a mesh overlay.
    pub mtu: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Send { reply: Reply, dst: SocketAddrV4 },
    Ignore(Ignored),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ignored {
    /// This box is not the active control plane; let the leader answer.
    Standby,
    /// No VM record carries this MAC.
    UnknownMac,
    /// The client is talking to a different server (option 54 mismatch).
    OtherServer,
    /// DECLINE / RELEASE: logged, nothing to do with fixed bindings.
    Noted,
    /// OFFER/ACK/NAK from someone else, or otherwise not for us.
    NotAClientMessage,
}

impl Ignored {
    pub fn name(self) -> &'static str {
        match self {
            Self::Standby => "standby",
            Self::UnknownMac => "unknown_mac",
            Self::OtherServer => "other_server",
            Self::Noted => "noted",
            Self::NotAClientMessage => "not_client",
        }
    }
}

pub fn respond(p: &Params, req: &Request, active: bool, binding: Option<&Binding>) -> Outcome {
    use MsgType::*;
    match req.msg_type {
        Offer | Ack | Nak => return Outcome::Ignore(Ignored::NotAClientMessage),
        Decline | Release => return Outcome::Ignore(Ignored::Noted),
        _ => {}
    }
    if !active {
        return Outcome::Ignore(Ignored::Standby);
    }
    let Some(b) = binding else {
        return Outcome::Ignore(Ignored::UnknownMac);
    };
    match req.msg_type {
        Discover => offer(p, req, b),
        Request => {
            if let Some(sid) = req.server_id
                && sid != p.server_id
            {
                return Outcome::Ignore(Ignored::OtherServer);
            }
            // SELECTING/INIT-REBOOT carry option 50; RENEWING/REBINDING carry
            // ciaddr. Either must match the record, otherwise NAK so the
            // client starts over and gets the right address.
            let wanted = req.requested_ip.or_else(|| (!req.ciaddr.is_unspecified()).then_some(req.ciaddr));
            match wanted {
                Some(ip) if ip == b.ip => ack(p, req, b, true),
                Some(_) | None => nak(p, req),
            }
        }
        Inform => ack(p, req, b, false),
        Offer | Ack | Nak | Decline | Release => unreachable!(),
    }
}

fn base(p: &Params, req: &Request, msg_type: MsgType, yiaddr: Ipv4Addr) -> Reply {
    Reply {
        msg_type,
        xid: req.xid,
        flags: req.flags,
        ciaddr: req.ciaddr,
        yiaddr,
        giaddr: req.giaddr,
        chaddr: req.chaddr,
        server_id: p.server_id,
        options: Vec::new(),
    }
}

fn net_options(p: &Params, b: &Binding, with_lease: bool) -> Vec<(u8, Vec<u8>)> {
    let n = &p.net;
    let mut o = Vec::new();
    if with_lease {
        let lease = n.lease_secs;
        o.push((opt::LEASE_TIME, wire::u32_opt(lease)));
        o.push((opt::T1, wire::u32_opt(lease / 2)));
        o.push((opt::T2, wire::u32_opt(lease / 8 * 7)));
    }
    o.push((opt::SUBNET_MASK, wire::ip_opt(n.cidr.mask())));
    o.push((opt::BROADCAST, wire::ip_opt(n.cidr.broadcast())));
    o.push((opt::ROUTER, wire::ip_opt(n.gateway())));
    o.push((opt::DNS, wire::ips_opt(&n.dns())));
    o.push((opt::HOSTNAME, b.name.as_bytes().to_vec()));
    if let Some(d) = &n.domain {
        o.push((opt::DOMAIN, d.as_bytes().to_vec()));
    }
    if let Some(m) = p.mtu {
        o.push((opt::MTU, wire::u16_opt(m)));
    }
    o
}

/// Where a reply goes (RFC 2131 §4.1). We always broadcast when the client
/// has no address yet instead of injecting an ARP entry for `yiaddr`.
fn dest(req: &Request, msg_type: MsgType) -> SocketAddrV4 {
    if !req.giaddr.is_unspecified() {
        return SocketAddrV4::new(req.giaddr, wire::SERVER_PORT);
    }
    if msg_type != MsgType::Nak && !req.ciaddr.is_unspecified() {
        return SocketAddrV4::new(req.ciaddr, wire::CLIENT_PORT);
    }
    SocketAddrV4::new(Ipv4Addr::BROADCAST, wire::CLIENT_PORT)
}

fn offer(p: &Params, req: &Request, b: &Binding) -> Outcome {
    let mut r = base(p, req, MsgType::Offer, b.ip);
    r.ciaddr = Ipv4Addr::UNSPECIFIED;
    r.options = net_options(p, b, true);
    Outcome::Send { dst: dest(req, MsgType::Offer), reply: r }
}

fn ack(p: &Params, req: &Request, b: &Binding, with_lease: bool) -> Outcome {
    // INFORM: client already has an address; yiaddr stays 0, no lease.
    let yiaddr = if with_lease { b.ip } else { Ipv4Addr::UNSPECIFIED };
    let mut r = base(p, req, MsgType::Ack, yiaddr);
    r.options = net_options(p, b, with_lease);
    Outcome::Send { dst: dest(req, MsgType::Ack), reply: r }
}

fn nak(p: &Params, req: &Request) -> Outcome {
    let mut r = base(p, req, MsgType::Nak, Ipv4Addr::UNSPECIFIED);
    r.ciaddr = Ipv4Addr::UNSPECIFIED;
    r.flags |= 0x8000; // NAKs are always broadcast
    r.options = vec![(opt::MESSAGE, b"address is not bound to this client".to_vec())];
    Outcome::Send { dst: dest(req, MsgType::Nak), reply: r }
}

#[cfg(test)]
mod tests {
    use super::super::model::DEFAULT_LEASE_SECS;
    use super::super::wire::{build_request, parse};
    use super::*;

    const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x01, 0x02, 0x03];

    fn params(mtu: Option<u16>) -> Params {
        Params {
            server_id: Ipv4Addr::new(10, 23, 0, 2),
            net: NetworkConfig {
                cidr: "10.23.0.0/16".parse().unwrap(),
                gateway: Some(Ipv4Addr::new(10, 23, 0, 1)),
                dns: vec![Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)],
                pool_start: None,
                pool_end: None,
                reserved: vec![],
                upstream_dns: vec![],
                dhcp: true,
                bridge: None,
                lease_secs: DEFAULT_LEASE_SECS,
                domain: Some("vm.example".into()),
                leader_only: false,
            },
            mtu,
        }
    }
    fn binding() -> Binding {
        Binding { vm: Uuid::nil(), name: "mesh-g1".into(), ip: Ipv4Addr::new(10, 23, 0, 42) }
    }
    fn req(msg: MsgType, extra: &[(u8, Vec<u8>)]) -> Request {
        parse(&build_request(msg, MAC, 0x1234, extra)).unwrap()
    }
    fn opt_of(r: &Reply, code: u8) -> Option<&[u8]> {
        r.options.iter().find(|(c, _)| *c == code).map(|(_, v)| v.as_slice())
    }

    #[test]
    fn discover_gets_offer_with_everything() {
        let p = params(Some(1370));
        let out = respond(&p, &req(MsgType::Discover, &[]), true, Some(&binding()));
        let Outcome::Send { reply, dst } = out else { panic!("{out:?}") };
        assert_eq!(reply.msg_type, MsgType::Offer);
        assert_eq!(reply.yiaddr, Ipv4Addr::new(10, 23, 0, 42));
        assert_eq!(reply.server_id, Ipv4Addr::new(10, 23, 0, 2));
        assert_eq!(dst, SocketAddrV4::new(Ipv4Addr::BROADCAST, 68));
        assert_eq!(opt_of(&reply, opt::SUBNET_MASK), Some(&[255, 255, 0, 0][..]));
        assert_eq!(opt_of(&reply, opt::ROUTER), Some(&[10, 23, 0, 1][..]));
        assert_eq!(opt_of(&reply, opt::DNS), Some(&[1, 1, 1, 1, 8, 8, 8, 8][..]));
        assert_eq!(opt_of(&reply, opt::BROADCAST), Some(&[10, 23, 255, 255][..]));
        assert_eq!(opt_of(&reply, opt::HOSTNAME), Some(&b"mesh-g1"[..]));
        assert_eq!(opt_of(&reply, opt::DOMAIN), Some(&b"vm.example"[..]));
        assert_eq!(opt_of(&reply, opt::MTU), Some(&[5, 90][..]));
        assert_eq!(opt_of(&reply, opt::LEASE_TIME), Some(&86400u32.to_be_bytes()[..]));
        assert_eq!(opt_of(&reply, opt::T1), Some(&43200u32.to_be_bytes()[..]));
        assert_eq!(opt_of(&reply, opt::T2), Some(&75600u32.to_be_bytes()[..]));
        // encodes; a full option set is longer than the 300-byte minimum
        let bytes = reply.to_bytes();
        assert!(bytes.len() >= 300 && bytes.len() < 400, "{}", bytes.len());
        assert_eq!(bytes[0], 2);
        assert_eq!(*bytes.iter().rev().find(|b| **b != 0).unwrap(), 255);
    }

    #[test]
    fn no_mtu_option_off_the_mesh() {
        let p = params(None);
        let Outcome::Send { reply, .. } = respond(&p, &req(MsgType::Discover, &[]), true, Some(&binding())) else {
            panic!()
        };
        assert_eq!(opt_of(&reply, opt::MTU), None);
    }

    #[test]
    fn request_right_address_is_acked_wrong_is_nakked() {
        let p = params(None);
        let good = req(MsgType::Request, &[(opt::REQUESTED_IP, vec![10, 23, 0, 42]), (opt::SERVER_ID, vec![10, 23, 0, 2])]);
        let Outcome::Send { reply, dst } = respond(&p, &good, true, Some(&binding())) else { panic!() };
        assert_eq!(reply.msg_type, MsgType::Ack);
        assert_eq!(reply.yiaddr, Ipv4Addr::new(10, 23, 0, 42));
        assert_eq!(dst, SocketAddrV4::new(Ipv4Addr::BROADCAST, 68));

        let stale = req(MsgType::Request, &[(opt::REQUESTED_IP, vec![10, 23, 0, 142])]);
        let Outcome::Send { reply, dst } = respond(&p, &stale, true, Some(&binding())) else { panic!() };
        assert_eq!(reply.msg_type, MsgType::Nak);
        assert_eq!(reply.yiaddr, Ipv4Addr::UNSPECIFIED);
        assert!(reply.flags & 0x8000 != 0);
        assert_eq!(dst, SocketAddrV4::new(Ipv4Addr::BROADCAST, 68));
        assert!(opt_of(&reply, opt::MESSAGE).is_some());

        let none = req(MsgType::Request, &[]);
        let Outcome::Send { reply, .. } = respond(&p, &none, true, Some(&binding())) else { panic!() };
        assert_eq!(reply.msg_type, MsgType::Nak);
    }

    #[test]
    fn renewal_is_unicast_to_ciaddr() {
        let p = params(None);
        let mut r = req(MsgType::Request, &[]);
        r.ciaddr = Ipv4Addr::new(10, 23, 0, 42);
        let Outcome::Send { reply, dst } = respond(&p, &r, true, Some(&binding())) else { panic!() };
        assert_eq!(reply.msg_type, MsgType::Ack);
        assert_eq!(reply.ciaddr, Ipv4Addr::new(10, 23, 0, 42));
        assert_eq!(dst, SocketAddrV4::new(Ipv4Addr::new(10, 23, 0, 42), 68));
        // ...but a renewal of the wrong address is a NAK, broadcast
        r.ciaddr = Ipv4Addr::new(10, 23, 0, 142);
        let Outcome::Send { reply, dst } = respond(&p, &r, true, Some(&binding())) else { panic!() };
        assert_eq!(reply.msg_type, MsgType::Nak);
        assert_eq!(dst.ip(), &Ipv4Addr::BROADCAST);
    }

    #[test]
    fn inform_gets_options_without_a_lease() {
        let p = params(Some(1370));
        let mut r = req(MsgType::Inform, &[]);
        r.ciaddr = Ipv4Addr::new(10, 23, 0, 42);
        let Outcome::Send { reply, dst } = respond(&p, &r, true, Some(&binding())) else { panic!() };
        assert_eq!(reply.msg_type, MsgType::Ack);
        assert_eq!(reply.yiaddr, Ipv4Addr::UNSPECIFIED);
        assert_eq!(opt_of(&reply, opt::LEASE_TIME), None);
        assert_eq!(opt_of(&reply, opt::MTU), Some(&[5, 90][..]));
        assert_eq!(dst, SocketAddrV4::new(Ipv4Addr::new(10, 23, 0, 42), 68));
    }

    #[test]
    fn ignores_what_it_should() {
        let p = params(None);
        let b = binding();
        assert_eq!(respond(&p, &req(MsgType::Discover, &[]), false, Some(&b)), Outcome::Ignore(Ignored::Standby));
        assert_eq!(respond(&p, &req(MsgType::Discover, &[]), true, None), Outcome::Ignore(Ignored::UnknownMac));
        let other = req(MsgType::Request, &[(opt::REQUESTED_IP, vec![10, 23, 0, 42]), (opt::SERVER_ID, vec![10, 23, 0, 9])]);
        assert_eq!(respond(&p, &other, true, Some(&b)), Outcome::Ignore(Ignored::OtherServer));
        assert_eq!(respond(&p, &req(MsgType::Release, &[]), true, Some(&b)), Outcome::Ignore(Ignored::Noted));
        assert_eq!(respond(&p, &req(MsgType::Decline, &[]), true, Some(&b)), Outcome::Ignore(Ignored::Noted));
        assert_eq!(respond(&p, &req(MsgType::Offer, &[]), true, Some(&b)), Outcome::Ignore(Ignored::NotAClientMessage));
        // standby wins over unknown: a follower never even looks
        assert_eq!(respond(&p, &req(MsgType::Discover, &[]), false, None), Outcome::Ignore(Ignored::Standby));
    }

    #[test]
    fn relayed_requests_go_back_to_the_relay() {
        let p = params(None);
        let mut r = req(MsgType::Discover, &[]);
        r.giaddr = Ipv4Addr::new(10, 23, 7, 1);
        let Outcome::Send { reply, dst } = respond(&p, &r, true, Some(&binding())) else { panic!() };
        assert_eq!(reply.giaddr, Ipv4Addr::new(10, 23, 7, 1));
        assert_eq!(dst, SocketAddrV4::new(Ipv4Addr::new(10, 23, 7, 1), 67));
    }
}
