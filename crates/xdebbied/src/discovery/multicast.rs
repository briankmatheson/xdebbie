// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use tracing::{debug, warn};

use super::messages::DiscoveryMessage;

pub const DEFAULT_MULTICAST_ADDR: &str = "239.0.0.42:7442";
const MAX_PACKET_SIZE: usize = 65536;

/// Multicast socket for discovery messages
pub struct MulticastSocket {
    socket: UdpSocket,
    multicast_addr: SocketAddr,
}

impl MulticastSocket {
    pub fn new(multicast_addr: &str, bind_addr: &str) -> Result<Self> {
        let multicast_addr: SocketAddr = multicast_addr
            .parse()
            .context("Invalid multicast address")?;

        let bind_addr: SocketAddr = bind_addr.parse().context("Invalid bind address")?;

        let socket = UdpSocket::bind(bind_addr).context("Failed to bind UDP socket")?;

        // Join multicast group
        if let IpAddr::V4(multicast_ip) = multicast_addr.ip() {
            socket
                .join_multicast_v4(&multicast_ip, &Ipv4Addr::UNSPECIFIED)
                .context("Failed to join multicast group")?;

            // Set multicast TTL
            socket
                .set_multicast_ttl_v4(1)
                .context("Failed to set multicast TTL")?;

            // Set multicast loop (receive our own messages for testing)
            socket
                .set_multicast_loop_v4(true)
                .context("Failed to set multicast loop")?;
        } else {
            anyhow::bail!("Only IPv4 multicast is supported");
        }

        // Set non-blocking mode
        socket
            .set_nonblocking(true)
            .context("Failed to set non-blocking")?;

        debug!(
            "Multicast socket bound to {} and joined group {}",
            bind_addr, multicast_addr
        );

        Ok(Self {
            socket,
            multicast_addr,
        })
    }

    pub fn send(&self, msg: &DiscoveryMessage) -> Result<()> {
        let json = msg.to_json()?;
        let bytes = json.as_bytes();

        if bytes.len() > MAX_PACKET_SIZE {
            anyhow::bail!("Message too large: {} bytes", bytes.len());
        }

        self.socket
            .send_to(bytes, self.multicast_addr)
            .context("Failed to send multicast message")?;

        debug!(
            "Sent {} message to {} ({} bytes)",
            match msg {
                DiscoveryMessage::Announce { .. } => "ANNOUNCE",
                DiscoveryMessage::Heartbeat { .. } => "HEARTBEAT",
                DiscoveryMessage::ElectionPropose { .. } => "ELECTION_PROPOSE",
                DiscoveryMessage::ElectionVote { .. } => "ELECTION_VOTE",
            },
            self.multicast_addr,
            bytes.len()
        );

        Ok(())
    }

    pub fn recv(&self) -> Result<Option<(DiscoveryMessage, SocketAddr)>> {
        let mut buf = vec![0u8; MAX_PACKET_SIZE];

        match self.socket.recv_from(&mut buf) {
            Ok((len, addr)) => {
                let json = std::str::from_utf8(&buf[..len])
                    .context("Received non-UTF8 message")?;

                match DiscoveryMessage::from_json(json) {
                    Ok(msg) => {
                        debug!(
                            "Received {} message from {} ({} bytes)",
                            match msg {
                                DiscoveryMessage::Announce { .. } => "ANNOUNCE",
                                DiscoveryMessage::Heartbeat { .. } => "HEARTBEAT",
                                DiscoveryMessage::ElectionPropose { .. } => "ELECTION_PROPOSE",
                                DiscoveryMessage::ElectionVote { .. } => "ELECTION_VOTE",
                            },
                            addr,
                            len
                        );
                        Ok(Some((msg, addr)))
                    }
                    Err(e) => {
                        warn!("Failed to parse message from {}: {}", addr, e);
                        Ok(None)
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e).context("Failed to receive from multicast socket"),
        }
    }
}
