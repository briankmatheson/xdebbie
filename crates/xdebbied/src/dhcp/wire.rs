// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal DHCPv4 wire format (RFC 2131/2132): the BOOTP fixed header plus
//! the handful of options a fixed-address responder needs. Deliberately not
//! a general-purpose DHCP library.

use std::net::Ipv4Addr;

pub const SERVER_PORT: u16 = 67;
pub const CLIENT_PORT: u16 = 68;
const MAGIC: [u8; 4] = [99, 130, 83, 99];
const MIN_LEN: usize = 236 + 4;
/// RFC 2131 §2: a reply shorter than 300 bytes is padded to 300.
const MIN_REPLY: usize = 300;

pub mod opt {
    pub const PAD: u8 = 0;
    pub const SUBNET_MASK: u8 = 1;
    pub const ROUTER: u8 = 3;
    pub const DNS: u8 = 6;
    pub const HOSTNAME: u8 = 12;
    pub const DOMAIN: u8 = 15;
    pub const MTU: u8 = 26;
    pub const BROADCAST: u8 = 28;
    pub const REQUESTED_IP: u8 = 50;
    pub const LEASE_TIME: u8 = 51;
    pub const MSG_TYPE: u8 = 53;
    pub const SERVER_ID: u8 = 54;
    pub const PARAM_LIST: u8 = 55;
    pub const MESSAGE: u8 = 56;
    pub const T1: u8 = 58;
    pub const T2: u8 = 59;
    pub const CLIENT_ID: u8 = 61;
    pub const END: u8 = 255;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    Discover,
    Offer,
    Request,
    Decline,
    Ack,
    Nak,
    Release,
    Inform,
}

impl MsgType {
    pub fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            1 => Self::Discover,
            2 => Self::Offer,
            3 => Self::Request,
            4 => Self::Decline,
            5 => Self::Ack,
            6 => Self::Nak,
            7 => Self::Release,
            8 => Self::Inform,
            _ => return None,
        })
    }
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Discover => 1,
            Self::Offer => 2,
            Self::Request => 3,
            Self::Decline => 4,
            Self::Ack => 5,
            Self::Nak => 6,
            Self::Release => 7,
            Self::Inform => 8,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Discover => "DISCOVER",
            Self::Offer => "OFFER",
            Self::Request => "REQUEST",
            Self::Decline => "DECLINE",
            Self::Ack => "ACK",
            Self::Nak => "NAK",
            Self::Release => "RELEASE",
            Self::Inform => "INFORM",
        }
    }
}

/// A parsed client message. Only the fields the responder looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub xid: u32,
    /// BOOTP `flags` (bit 15 = client asks for broadcast replies).
    pub flags: u16,
    pub ciaddr: Ipv4Addr,
    pub giaddr: Ipv4Addr,
    /// Ethernet hardware address (htype 1, hlen 6); other types are rejected.
    pub chaddr: [u8; 6],
    pub msg_type: MsgType,
    pub requested_ip: Option<Ipv4Addr>,
    pub server_id: Option<Ipv4Addr>,
    pub hostname: Option<String>,
    pub client_id: Option<Vec<u8>>,
    pub param_list: Vec<u8>,
}

impl Request {
    pub fn broadcast_requested(&self) -> bool {
        self.flags & 0x8000 != 0
    }
    pub fn mac(&self) -> String {
        let m = self.chaddr;
        format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    TooShort(usize),
    NotRequest,
    NotEthernet,
    BadMagic,
    NoMsgType,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort(n) => write!(f, "packet too short ({n} bytes)"),
            Self::NotRequest => write!(f, "not a BOOTREQUEST"),
            Self::NotEthernet => write!(f, "not an ethernet client (htype/hlen)"),
            Self::BadMagic => write!(f, "missing DHCP magic cookie"),
            Self::NoMsgType => write!(f, "no DHCP message type option"),
        }
    }
}

fn ip4(b: &[u8]) -> Ipv4Addr {
    Ipv4Addr::new(b[0], b[1], b[2], b[3])
}

pub fn parse(buf: &[u8]) -> Result<Request, ParseError> {
    if buf.len() < MIN_LEN {
        return Err(ParseError::TooShort(buf.len()));
    }
    if buf[0] != 1 {
        return Err(ParseError::NotRequest);
    }
    if buf[1] != 1 || buf[2] != 6 {
        return Err(ParseError::NotEthernet);
    }
    if buf[236..240] != MAGIC {
        return Err(ParseError::BadMagic);
    }
    let mut req = Request {
        xid: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
        flags: u16::from_be_bytes([buf[10], buf[11]]),
        ciaddr: ip4(&buf[12..16]),
        giaddr: ip4(&buf[24..28]),
        chaddr: [buf[28], buf[29], buf[30], buf[31], buf[32], buf[33]],
        msg_type: MsgType::Discover,
        requested_ip: None,
        server_id: None,
        hostname: None,
        client_id: None,
        param_list: Vec::new(),
    };
    let mut msg_type = None;
    let mut i = 240;
    while i < buf.len() {
        let code = buf[i];
        i += 1;
        match code {
            opt::PAD => continue,
            opt::END => break,
            _ => {}
        }
        if i >= buf.len() {
            break;
        }
        let len = buf[i] as usize;
        i += 1;
        if i + len > buf.len() {
            break;
        }
        let v = &buf[i..i + len];
        i += len;
        match code {
            opt::MSG_TYPE if len == 1 => msg_type = MsgType::from_u8(v[0]),
            opt::REQUESTED_IP if len == 4 => req.requested_ip = Some(ip4(v)),
            opt::SERVER_ID if len == 4 => req.server_id = Some(ip4(v)),
            opt::HOSTNAME => {
                let s = String::from_utf8_lossy(v).trim_end_matches('\0').to_string();
                if !s.is_empty() {
                    req.hostname = Some(s);
                }
            }
            opt::CLIENT_ID => req.client_id = Some(v.to_vec()),
            opt::PARAM_LIST => req.param_list = v.to_vec(),
            _ => {}
        }
    }
    req.msg_type = msg_type.ok_or(ParseError::NoMsgType)?;
    Ok(req)
}

/// Everything that goes into a server reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub msg_type: MsgType,
    pub xid: u32,
    pub flags: u16,
    pub ciaddr: Ipv4Addr,
    pub yiaddr: Ipv4Addr,
    pub giaddr: Ipv4Addr,
    pub chaddr: [u8; 6],
    pub server_id: Ipv4Addr,
    /// `(code, value)` in the order they will be emitted.
    pub options: Vec<(u8, Vec<u8>)>,
}

impl Reply {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = vec![0u8; 240];
        b[0] = 2; // BOOTREPLY
        b[1] = 1;
        b[2] = 6;
        b[4..8].copy_from_slice(&self.xid.to_be_bytes());
        b[10..12].copy_from_slice(&self.flags.to_be_bytes());
        b[12..16].copy_from_slice(&self.ciaddr.octets());
        b[16..20].copy_from_slice(&self.yiaddr.octets());
        b[20..24].copy_from_slice(&self.server_id.octets()); // siaddr
        b[24..28].copy_from_slice(&self.giaddr.octets());
        b[28..34].copy_from_slice(&self.chaddr);
        b[236..240].copy_from_slice(&MAGIC);
        b.push(opt::MSG_TYPE);
        b.push(1);
        b.push(self.msg_type.as_u8());
        b.push(opt::SERVER_ID);
        b.push(4);
        b.extend_from_slice(&self.server_id.octets());
        for (code, v) in &self.options {
            // Options longer than 255 bytes would need splitting; nothing we
            // emit gets anywhere near that.
            let v = &v[..v.len().min(255)];
            b.push(*code);
            b.push(v.len() as u8);
            b.extend_from_slice(v);
        }
        b.push(opt::END);
        if b.len() < MIN_REPLY {
            b.resize(MIN_REPLY, 0);
        }
        b
    }
}

/// Helpers for building option values.
pub fn ip_opt(ip: Ipv4Addr) -> Vec<u8> {
    ip.octets().to_vec()
}
pub fn ips_opt(ips: &[Ipv4Addr]) -> Vec<u8> {
    ips.iter().flat_map(|i| i.octets()).collect()
}
pub fn u32_opt(v: u32) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}
pub fn u16_opt(v: u16) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}

#[cfg(test)]
pub(crate) fn build_request(msg: MsgType, chaddr: [u8; 6], xid: u32, extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut b = vec![0u8; 240];
    b[0] = 1;
    b[1] = 1;
    b[2] = 6;
    b[4..8].copy_from_slice(&xid.to_be_bytes());
    b[28..34].copy_from_slice(&chaddr);
    b[236..240].copy_from_slice(&MAGIC);
    b.extend_from_slice(&[opt::MSG_TYPE, 1, msg.as_u8()]);
    for (c, v) in extra {
        b.push(*c);
        b.push(v.len() as u8);
        b.extend_from_slice(v);
    }
    b.push(opt::END);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0xaa, 0xbb, 0xcc];

    #[test]
    fn parses_discover_with_options() {
        let raw = build_request(
            MsgType::Discover,
            MAC,
            0xdeadbeef,
            &[
                (opt::HOSTNAME, b"mesh-g1".to_vec()),
                (opt::PARAM_LIST, vec![1, 3, 6, 26]),
                (opt::REQUESTED_IP, vec![10, 23, 0, 142]),
                (opt::CLIENT_ID, vec![1, 0x52, 0x54, 0, 0xaa, 0xbb, 0xcc]),
            ],
        );
        let r = parse(&raw).unwrap();
        assert_eq!(r.msg_type, MsgType::Discover);
        assert_eq!(r.xid, 0xdeadbeef);
        assert_eq!(r.chaddr, MAC);
        assert_eq!(r.mac(), "52:54:00:aa:bb:cc");
        assert_eq!(r.hostname.as_deref(), Some("mesh-g1"));
        assert_eq!(r.param_list, vec![1, 3, 6, 26]);
        assert_eq!(r.requested_ip, Some(Ipv4Addr::new(10, 23, 0, 142)));
        assert!(!r.broadcast_requested());
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse(&[0u8; 10]).unwrap_err(), ParseError::TooShort(10));
        let mut raw = build_request(MsgType::Discover, MAC, 1, &[]);
        raw[0] = 2;
        assert_eq!(parse(&raw).unwrap_err(), ParseError::NotRequest);
        let mut raw = build_request(MsgType::Discover, MAC, 1, &[]);
        raw[238] = 0;
        assert_eq!(parse(&raw).unwrap_err(), ParseError::BadMagic);
        let mut raw = build_request(MsgType::Discover, MAC, 1, &[]);
        raw[242] = 99; // bogus message type
        assert_eq!(parse(&raw).unwrap_err(), ParseError::NoMsgType);
    }

    #[test]
    fn reply_round_trips_and_pads() {
        let rep = Reply {
            msg_type: MsgType::Offer,
            xid: 7,
            flags: 0x8000,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            yiaddr: Ipv4Addr::new(10, 23, 0, 20),
            giaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: MAC,
            server_id: Ipv4Addr::new(10, 23, 0, 1),
            options: vec![(opt::SUBNET_MASK, ip_opt(Ipv4Addr::new(255, 255, 0, 0))), (opt::MTU, u16_opt(1370))],
        };
        let b = rep.to_bytes();
        assert_eq!(b.len(), 300);
        assert_eq!(b[0], 2);
        assert_eq!(&b[16..20], &[10, 23, 0, 20]);
        assert_eq!(&b[20..24], &[10, 23, 0, 1]);
        assert_eq!(&b[236..240], &MAGIC);
        // 53,1,2  54,4,srv  1,4,mask  26,2,mtu  255
        assert_eq!(&b[240..243], &[53, 1, 2]);
        assert_eq!(&b[243..249], &[54, 4, 10, 23, 0, 1]);
        assert_eq!(&b[249..255], &[1, 4, 255, 255, 0, 0]);
        assert_eq!(&b[255..259], &[26, 2, 5, 90]);
        assert_eq!(b[259], 255);
    }
}
