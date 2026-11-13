// SPDX-License-Identifier: GPL-3.0-or-later
//! Overlay addressing and the built-in DHCP responder.
//!
//! The scheduler allocates a VM's overlay IPv4 address when the VM is
//! created and stores it in the VM record next to the deterministic MAC
//! (`Vm::mac()`). The responder here answers DHCP for exactly those
//! `(MAC, IP)` pairs and nothing else: there is no lease database to sync,
//! no expiry, and every box holding the same VM records would give the same
//! answer. Which box answers is a policy knob (`leader_only`), not a data
//! question.
//!
//! * `model` — `[network]` config, CIDR maths, the allocator.
//! * `wire`  — just enough DHCPv4 encoding.
//! * `respond` — the pure request → reply function.
//! * this file — the UDP socket on the bridge, store lookup, status.

pub mod model;
pub mod respond;
pub mod wire;

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

pub use model::{Cidr, NetworkConfig};
pub use respond::{Binding, Ignored, Outcome, Params};

use crate::store::Store;

const REBIND_DELAY: Duration = Duration::from_secs(5);
const MAX_SEEN: usize = 1024;

/// How the responder decides whether this box should answer at all.
pub struct Gate {
    /// True when this process is the active control plane.
    pub leader: Arc<AtomicBool>,
    /// `[network] leader_only`: when false every box with the VM records
    /// answers (correct while control planes keep independent stores).
    pub leader_only: bool,
}

impl Gate {
    pub fn active(&self) -> bool {
        !self.leader_only || self.leader.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Counters {
    pub offers: u64,
    pub acks: u64,
    pub naks: u64,
    pub ignored_unknown: u64,
    pub ignored_standby: u64,
    pub ignored_other: u64,
    pub parse_errors: u64,
    pub send_errors: u64,
}

/// Last thing we heard from a MAC on the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Seen {
    pub mac: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<Ipv4Addr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vm: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Hostname the client sent (option 12), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_hostname: Option<String>,
    /// e.g. `DISCOVER→OFFER`, `REQUEST→NAK`, `DISCOVER→unknown_mac`.
    pub last: String,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DhcpStatus {
    pub enabled: bool,
    pub bridge: String,
    pub bound: bool,
    pub active: bool,
    pub leader_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<Ipv4Addr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub counters: Counters,
    pub seen: Vec<Seen>,
}

/// Full `GET /v1/network` body: config + responder status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub cidr: Cidr,
    pub gateway: Ipv4Addr,
    pub dns: Vec<Ipv4Addr>,
    pub pool_start: Ipv4Addr,
    pub pool_end: Ipv4Addr,
    pub reserved: Vec<Ipv4Addr>,
    #[serde(default)]
    pub upstream_dns: Vec<Ipv4Addr>,
    pub lease_secs: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    pub dhcp: DhcpStatus,
}

#[derive(Default)]
struct Inner {
    bound: bool,
    server_id: Option<Ipv4Addr>,
    error: Option<String>,
    counters: Counters,
    seen: BTreeMap<String, Seen>,
}

pub type MtuFn = Arc<dyn Fn(&str) -> Option<u16> + Send + Sync>;

pub struct DhcpServer {
    net: NetworkConfig,
    bridge: String,
    store: Store,
    mtu: MtuFn,
    gate: Gate,
    inner: Mutex<Inner>,
}

impl DhcpServer {
    pub fn new(net: NetworkConfig, bridge: String, store: Store, mtu: MtuFn, gate: Gate) -> Self {
        Self {
            net,
            bridge,
            store,
            mtu,
            gate,
            inner: Mutex::new(Inner::default()),
        }
    }

    pub fn status(&self) -> NetworkStatus {
        let g = self.inner.lock().unwrap();
        let (pool_start, pool_end) = self.net.pool();
        NetworkStatus {
            cidr: self.net.cidr,
            gateway: self.net.gateway(),
            dns: self.net.dns(),
            pool_start,
            pool_end,
            reserved: self.net.reserved.clone(),
            upstream_dns: self.net.upstream_dns.clone(),
            lease_secs: self.net.lease_secs,
            domain: self.net.domain.clone(),
            dhcp: DhcpStatus {
                enabled: self.net.dhcp,
                bridge: self.bridge.clone(),
                bound: g.bound,
                active: self.gate.active(),
                leader_only: self.gate.leader_only,
                server_id: g.server_id,
                mtu: (self.mtu)(&self.bridge),
                error: g.error.clone(),
                counters: g.counters.clone(),
                seen: g.seen.values().cloned().collect(),
            },
        }
    }

    fn params(&self, server_id: Ipv4Addr) -> Params {
        Params {
            server_id,
            net: self.net.clone(),
            mtu: (self.mtu)(&self.bridge),
        }
    }

    async fn binding_for(&self, chaddr: &[u8; 6]) -> Option<Binding> {
        let st = self.store.read().await;
        st.vms
            .values()
            .find(|v| v.ip.is_some() && v.mac_bytes() == *chaddr)
            .map(|v| Binding {
                vm: v.id,
                name: v.spec.name.clone(),
                ip: v.ip.unwrap(),
            })
    }

    fn note(&self, req: &wire::Request, b: Option<&Binding>, outcome: &str) {
        let mut g = self.inner.lock().unwrap();
        let mac = req.mac();
        if g.seen.len() >= MAX_SEEN && !g.seen.contains_key(&mac) {
            let oldest = g
                .seen
                .values()
                .min_by_key(|s| s.last_seen)
                .map(|s| s.mac.clone());
            if let Some(k) = oldest {
                g.seen.remove(&k);
            }
        }
        g.seen.insert(
            mac.clone(),
            Seen {
                mac,
                ip: b.map(|b| b.ip),
                vm: b.map(|b| b.vm),
                name: b.map(|b| b.name.clone()),
                client_hostname: req.hostname.clone(),
                last: format!("{}→{}", req.msg_type.name(), outcome),
                last_seen: Utc::now(),
            },
        );
    }

    /// Serve until the process exits. Rebinds whenever the bridge goes away
    /// or is recreated (libvirt `net-destroy`/`net-start` changes the ifindex
    /// and `SO_BINDTODEVICE` remembers the old one).
    pub async fn run(self: Arc<Self>) {
        loop {
            let (sock, ifindex) = match self.bind().await {
                Ok(v) => v,
                Err(e) => {
                    let msg = format!("{e:#}");
                    let changed = {
                        let mut g = self.inner.lock().unwrap();
                        let changed = g.error.as_deref() != Some(&msg);
                        g.bound = false;
                        g.error = Some(msg.clone());
                        changed
                    };
                    if changed {
                        warn!(bridge = %self.bridge, "dhcp: {msg}");
                    }
                    tokio::time::sleep(REBIND_DELAY).await;
                    continue;
                }
            };
            let server_id = detect_addr(&self.bridge, &self.net.cidr)
                .await
                .unwrap_or_else(|| self.net.gateway());
            {
                let mut g = self.inner.lock().unwrap();
                g.bound = true;
                g.error = None;
                g.server_id = Some(server_id);
            }
            info!(bridge = %self.bridge, %server_id, active = self.gate.active(), "dhcp: listening on udp/67");
            self.serve(sock, ifindex, server_id).await;
            self.inner.lock().unwrap().bound = false;
            tokio::time::sleep(REBIND_DELAY).await;
        }
    }

    async fn bind(&self) -> Result<(tokio::net::UdpSocket, u32)> {
        use socket2::{Domain, Protocol, Socket, Type};
        let ifindex = if_index(&self.bridge)
            .with_context(|| format!("bridge {} does not exist", self.bridge))?;
        let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).context("socket")?;
        s.set_reuse_address(true).context("SO_REUSEADDR")?;
        s.set_broadcast(true).context("SO_BROADCAST")?;
        s.bind_device(Some(self.bridge.as_bytes()))
            .with_context(|| format!("SO_BINDTODEVICE {}", self.bridge))?;
        s.set_nonblocking(true)?;
        s.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, wire::SERVER_PORT).into())
            .context("bind udp/67 (is another DHCP server — dhcpd, dnsmasq — on this bridge?)")?;
        let std: std::net::UdpSocket = s.into();
        Ok((tokio::net::UdpSocket::from_std(std)?, ifindex))
    }

    async fn serve(&self, sock: tokio::net::UdpSocket, ifindex: u32, server_id: Ipv4Addr) {
        let params = self.params(server_id);
        let mut buf = vec![0u8; 1500];
        loop {
            let (n, _from) =
                match tokio::time::timeout(REBIND_DELAY, sock.recv_from(&mut buf)).await {
                    Ok(Ok(v)) => v,
                    Ok(Err(e)) => {
                        warn!("dhcp: recv: {e}");
                        return;
                    }
                    Err(_) => {
                        if if_index(&self.bridge) != Some(ifindex) {
                            info!(bridge = %self.bridge, "dhcp: bridge changed, rebinding");
                            return;
                        }
                        continue;
                    }
                };
            let req = match wire::parse(&buf[..n]) {
                Ok(r) => r,
                Err(e) => {
                    debug!("dhcp: ignoring packet: {e}");
                    self.inner.lock().unwrap().counters.parse_errors += 1;
                    continue;
                }
            };
            let binding = self.binding_for(&req.chaddr).await;
            // MTU can change between ticks (mesh enabled later); refresh cheaply.
            let params = Params {
                mtu: (self.mtu)(&self.bridge),
                ..params.clone()
            };
            match respond::respond(&params, &req, self.gate.active(), binding.as_ref()) {
                Outcome::Send { reply, dst } => {
                    let bytes = reply.to_bytes();
                    let kind = reply.msg_type;
                    match sock.send_to(&bytes, std::net::SocketAddr::V4(dst)).await {
                        Ok(_) => {
                            let mut g = self.inner.lock().unwrap();
                            match kind {
                                wire::MsgType::Offer => g.counters.offers += 1,
                                wire::MsgType::Ack => g.counters.acks += 1,
                                wire::MsgType::Nak => g.counters.naks += 1,
                                _ => {}
                            }
                            drop(g);
                            info!(
                                mac = %req.mac(),
                                vm = binding.as_ref().map(|b| b.name.as_str()).unwrap_or("-"),
                                ip = %reply.yiaddr,
                                "dhcp: {} → {}",
                                req.msg_type.name(),
                                kind.name()
                            );
                            self.note(&req, binding.as_ref(), kind.name());
                        }
                        Err(e) => {
                            warn!(mac = %req.mac(), "dhcp: send {}: {e}", kind.name());
                            self.inner.lock().unwrap().counters.send_errors += 1;
                        }
                    }
                }
                Outcome::Ignore(why) => {
                    {
                        let mut g = self.inner.lock().unwrap();
                        match why {
                            Ignored::UnknownMac => g.counters.ignored_unknown += 1,
                            Ignored::Standby => g.counters.ignored_standby += 1,
                            Ignored::OtherServer => g.counters.ignored_other += 1,
                            Ignored::Noted | Ignored::NotAClientMessage => {}
                        }
                    }
                    debug!(mac = %req.mac(), "dhcp: {} ignored ({})", req.msg_type.name(), why.name());
                    if !matches!(why, Ignored::NotAClientMessage) {
                        self.note(&req, binding.as_ref(), why.name());
                    }
                }
            }
        }
    }
}

fn if_index(name: &str) -> Option<u32> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: valid NUL-terminated string; the call only reads it.
    let idx = unsafe { libc::if_nametoindex(c.as_ptr()) };
    (idx != 0).then_some(idx)
}

/// This box's address on `bridge` inside `cidr` (option 54 / `siaddr`).
async fn detect_addr(bridge: &str, cidr: &Cidr) -> Option<Ipv4Addr> {
    let out = tokio::process::Command::new("ip")
        .args(["-j", "-4", "addr", "show", "dev", bridge])
        .output()
        .await
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v.as_array()?
        .iter()
        .flat_map(|l| l["addr_info"].as_array().cloned().unwrap_or_default())
        .filter_map(|a| a["local"].as_str()?.parse::<Ipv4Addr>().ok())
        .find(|ip| cidr.contains(*ip))
}
