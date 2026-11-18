// SPDX-License-Identifier: GPL-3.0-or-later
//! Peer health (section 7): one ICMP echo per peer per interval over a
//! single in-process raw socket, fed through a hysteresis state machine.
//! The state machine is pure so every transition can be unit-tested; the
//! prober task just produces `ProbeEvent`s for the manager.

use std::{net::IpAddr, net::Ipv4Addr, sync::Arc, time::Duration};

use surge_ping::{Client, Config, ICMP, PingIdentifier, PingSequence};
use tokio::sync::mpsc;

use super::model::PeerState;

/// Per-peer hysteresis. Probes are ignored while in `Error`; the manager
/// clears that when the cause goes away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hysteresis {
    pub up_after: u32,
    pub down_after: u32,
    pub state: PeerState,
    ok_streak: u32,
    miss_streak: u32,
}

impl Hysteresis {
    pub fn new(up_after: u32, down_after: u32) -> Self {
        Self {
            up_after: up_after.max(1),
            down_after: down_after.max(1),
            state: PeerState::Pending,
            ok_streak: 0,
            miss_streak: 0,
        }
    }

    /// Feed one probe result. Returns the new state if it changed.
    pub fn observe(&mut self, ok: bool) -> Option<PeerState> {
        if self.state == PeerState::Error {
            return None;
        }
        if ok {
            self.ok_streak += 1;
            self.miss_streak = 0;
        } else {
            self.miss_streak += 1;
            self.ok_streak = 0;
        }
        let next = match self.state {
            PeerState::Pending | PeerState::Down if self.ok_streak >= self.up_after => {
                PeerState::Up
            }
            PeerState::Pending | PeerState::Up if self.miss_streak >= self.down_after => {
                PeerState::Down
            }
            s => s,
        };
        if next != self.state {
            self.state = next;
            Some(next)
        } else {
            None
        }
    }

    /// Tunnel can't be created, route mismatch, name conflict.
    pub fn set_error(&mut self) -> Option<PeerState> {
        if self.state == PeerState::Error {
            return None;
        }
        self.state = PeerState::Error;
        self.ok_streak = 0;
        self.miss_streak = 0;
        Some(PeerState::Error)
    }

    /// Cause cleared on reconcile → back to Pending.
    pub fn clear_error(&mut self) -> Option<PeerState> {
        if self.state != PeerState::Error {
            return None;
        }
        self.state = PeerState::Pending;
        self.ok_streak = 0;
        self.miss_streak = 0;
        Some(PeerState::Pending)
    }
}

#[derive(Debug, Clone)]
pub struct ProbeEvent {
    pub box_id: u8,
    pub ok: bool,
    pub rtt_ms: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct ProbeParams {
    pub interval: Duration,
    pub timeout: Duration,
}

/// Shared ICMP socket. Created once; cloning is cheap.
#[derive(Clone)]
pub struct IcmpClient(Arc<Client>);

impl IcmpClient {
    pub fn new() -> std::io::Result<Self> {
        let cfg = Config::builder().kind(ICMP::V4).build();
        Ok(Self(Arc::new(Client::new(&cfg)?)))
    }
}

/// How long to wait for one echo: about 15 round trips of the peer's
/// smoothed latency, never under `ADAPT_FLOOR` (scheduling jitter) and never
/// over the configured timeout. Before any reply, the configured timeout.
pub const ADAPT_RTTS: f64 = 15.0;
pub const ADAPT_FLOOR: Duration = Duration::from_millis(250);

pub fn adaptive_timeout(srtt_ms: Option<f64>, cap: Duration) -> Duration {
    match srtt_ms {
        Some(ms) => Duration::from_secs_f64(ms * ADAPT_RTTS / 1000.0)
            .max(ADAPT_FLOOR)
            .min(cap),
        None => cap,
    }
}

/// Run one peer's prober until the returned handle is aborted. Identifier
/// = 0x2300 | box, so replies are matched per peer; sequence wraps at u16.
pub fn spawn_prober(
    client: IcmpClient,
    box_id: u8,
    target: Ipv4Addr,
    params: ProbeParams,
    tx: mpsc::Sender<ProbeEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut pinger = client
            .0
            .pinger(IpAddr::V4(target), PingIdentifier(0x2300 | box_id as u16))
            .await;
        let mut srtt: Option<f64> = None;
        let mut tick = tokio::time::interval(params.interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let payload = [0u8; 32];
        let mut seq: u16 = 0;
        loop {
            tick.tick().await;
            pinger.timeout(adaptive_timeout(srtt, params.timeout));
            let ev = match pinger.ping(PingSequence(seq), &payload).await {
                Ok((_pkt, rtt)) => {
                    let ms = rtt.as_secs_f64() * 1000.0;
                    // RFC 6298's 1/8 gain: steady, but follows a peer that roams.
                    srtt = Some(srtt.map_or(ms, |s| s + (ms - s) / 8.0));
                    ProbeEvent {
                        box_id,
                        ok: true,
                        rtt_ms: Some(ms),
                    }
                }
                Err(_) => ProbeEvent {
                    box_id,
                    ok: false,
                    rtt_ms: None,
                },
            };
            seq = seq.wrapping_add(1);
            if tx.send(ev).await.is_err() {
                return; // manager gone
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_follows_the_latency() {
        let cap = Duration::from_millis(2000);
        assert_eq!(adaptive_timeout(None, cap), cap);
        assert_eq!(adaptive_timeout(Some(0.4), cap), ADAPT_FLOOR);
        assert_eq!(
            adaptive_timeout(Some(40.0), cap),
            Duration::from_millis(600)
        );
        assert_eq!(adaptive_timeout(Some(500.0), cap), cap);
    }

    fn feed(h: &mut Hysteresis, results: &[bool]) -> Vec<PeerState> {
        results.iter().filter_map(|r| h.observe(*r)).collect()
    }

    #[test]
    fn pending_to_up_after_n_ok() {
        let mut h = Hysteresis::new(3, 3);
        assert_eq!(feed(&mut h, &[true, true]), vec![]);
        assert_eq!(h.state, PeerState::Pending);
        assert_eq!(feed(&mut h, &[true]), vec![PeerState::Up]);
    }

    #[test]
    fn pending_to_down_after_n_misses() {
        let mut h = Hysteresis::new(3, 3);
        assert_eq!(feed(&mut h, &[false, false, false]), vec![PeerState::Down]);
    }

    #[test]
    fn up_to_down_and_back() {
        let mut h = Hysteresis::new(3, 3);
        feed(&mut h, &[true, true, true]);
        // Two misses are not enough; a success resets the streak.
        assert_eq!(feed(&mut h, &[false, false, true]), vec![]);
        assert_eq!(h.state, PeerState::Up);
        assert_eq!(feed(&mut h, &[false, false, false]), vec![PeerState::Down]);
        assert_eq!(feed(&mut h, &[true, true]), vec![]);
        assert_eq!(feed(&mut h, &[true]), vec![PeerState::Up]);
    }

    #[test]
    fn mixed_streaks_reset() {
        let mut h = Hysteresis::new(2, 2);
        assert_eq!(feed(&mut h, &[true, false, true, false]), vec![]);
        assert_eq!(h.state, PeerState::Pending);
        assert_eq!(feed(&mut h, &[true, true]), vec![PeerState::Up]);
    }

    #[test]
    fn error_overrides_and_clears_to_pending() {
        let mut h = Hysteresis::new(3, 3);
        feed(&mut h, &[true, true, true]);
        assert_eq!(h.set_error(), Some(PeerState::Error));
        assert_eq!(h.set_error(), None);
        // Probes are ignored while in Error.
        assert_eq!(feed(&mut h, &[true, true, true, true]), vec![]);
        assert_eq!(h.state, PeerState::Error);
        assert_eq!(h.clear_error(), Some(PeerState::Pending));
        assert_eq!(h.clear_error(), None);
        // Streaks were reset: needs a full up_after again.
        assert_eq!(feed(&mut h, &[true, true]), vec![]);
        assert_eq!(feed(&mut h, &[true]), vec![PeerState::Up]);
    }

    #[test]
    fn thresholds_of_one() {
        let mut h = Hysteresis::new(1, 1);
        assert_eq!(feed(&mut h, &[true]), vec![PeerState::Up]);
        assert_eq!(feed(&mut h, &[false]), vec![PeerState::Down]);
        assert_eq!(feed(&mut h, &[true]), vec![PeerState::Up]);
    }
}
