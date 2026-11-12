// SPDX-License-Identifier: GPL-3.0-or-later
//! Mesh metrics (section 8). X11 (a metrics endpoint) has not landed, so
//! these live here and are included in `GET /v1/mesh` as `status.metrics`.
//! `render_prometheus` is ready for whenever an exporter appears.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::model::PeerState;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PeerMetrics {
    pub up: u8,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_seconds: Option<f64>,
    pub probe_failures_total: u64,
    pub tunnel_rx_bytes_total: u64,
    pub tunnel_tx_bytes_total: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub generation: u64,
    pub observed_generation: u64,
    pub mtu_bytes: u16,
    /// keyed by `result`: ok | error | noop
    pub reconcile_total: BTreeMap<String, u64>,
    /// keyed by action kind
    pub reconcile_actions_total: BTreeMap<String, u64>,
    /// keyed by peer box id (as string, for JSON)
    pub peers: BTreeMap<String, PeerMetrics>,
}

impl Snapshot {
    pub fn inc_reconcile(&mut self, result: &str) {
        *self.reconcile_total.entry(result.into()).or_default() += 1;
    }
    pub fn inc_action(&mut self, kind: &str) {
        *self.reconcile_actions_total.entry(kind.into()).or_default() += 1;
    }
    pub fn peer(&mut self, box_id: u8) -> &mut PeerMetrics {
        self.peers.entry(box_id.to_string()).or_default()
    }
    pub fn set_peer_state(&mut self, box_id: u8, state: PeerState, rtt_ms: Option<f64>) {
        let p = self.peer(box_id);
        p.up = u8::from(state == PeerState::Up);
        p.state = state.as_str().into();
        p.rtt_seconds = rtt_ms.map(|m| m / 1000.0);
    }
    /// Drop peers that are no longer configured.
    pub fn retain_peers(&mut self, keep: &[u8]) {
        let keep: Vec<String> = keep.iter().map(|b| b.to_string()).collect();
        self.peers.retain(|k, _| keep.contains(k));
    }

    /// Prometheus text exposition, for a future `/metrics` (docs/mesh.md).
    #[allow(dead_code)]
    pub fn render_prometheus(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!("xdebbie_mesh_generation {}\n", self.generation));
        o.push_str(&format!("xdebbie_mesh_observed_generation {}\n", self.observed_generation));
        o.push_str(&format!("xdebbie_mesh_mtu_bytes {}\n", self.mtu_bytes));
        for (r, n) in &self.reconcile_total {
            o.push_str(&format!("xdebbie_mesh_reconcile_total{{result=\"{r}\"}} {n}\n"));
        }
        for (a, n) in &self.reconcile_actions_total {
            o.push_str(&format!("xdebbie_mesh_reconcile_actions_total{{action=\"{a}\"}} {n}\n"));
        }
        for (peer, m) in &self.peers {
            o.push_str(&format!("xdebbie_mesh_peer_up{{peer=\"{peer}\"}} {}\n", m.up));
            for st in ["Pending", "Up", "Down", "Error"] {
                o.push_str(&format!(
                    "xdebbie_mesh_peer_state{{peer=\"{peer}\",state=\"{st}\"}} {}\n",
                    u8::from(m.state == st)
                ));
            }
            if let Some(r) = m.rtt_seconds {
                o.push_str(&format!("xdebbie_mesh_peer_rtt_seconds{{peer=\"{peer}\"}} {r}\n"));
            }
            o.push_str(&format!("xdebbie_mesh_probe_failures_total{{peer=\"{peer}\"}} {}\n", m.probe_failures_total));
            o.push_str(&format!("xdebbie_mesh_tunnel_rx_bytes_total{{peer=\"{peer}\"}} {}\n", m.tunnel_rx_bytes_total));
            o.push_str(&format!("xdebbie_mesh_tunnel_tx_bytes_total{{peer=\"{peer}\"}} {}\n", m.tunnel_tx_bytes_total));
        }
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_and_render() {
        let mut s = Snapshot::default();
        s.inc_reconcile("ok");
        s.inc_reconcile("ok");
        s.inc_action("create_tunnel");
        s.set_peer_state(1, PeerState::Up, Some(2.5));
        s.peer(1).probe_failures_total = 3;
        let p = s.render_prometheus();
        assert!(p.contains("xdebbie_mesh_reconcile_total{result=\"ok\"} 2"));
        assert!(p.contains("xdebbie_mesh_peer_up{peer=\"1\"} 1"));
        assert!(p.contains("xdebbie_mesh_peer_state{peer=\"1\",state=\"Up\"} 1"));
        assert!(p.contains("xdebbie_mesh_peer_rtt_seconds{peer=\"1\"} 0.0025"));
        s.retain_peers(&[]);
        assert!(s.peers.is_empty());
    }
}
