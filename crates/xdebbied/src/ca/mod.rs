// SPDX-License-Identifier: GPL-3.0-or-later
//! Certificate Authority for libvirt TLS and cluster enrollment.
//!
//! **CA host** (one per cluster): holds `/etc/internal-ca/{ca-cert,ca-key}.pem`,
//! runs the approval API, signs CSRs with 14-day validity.
//!
//! **Joining nodes**: generate a keypair, submit CSR, poll for approval, install
//! signed cert. Daily renewal task keeps certs fresh.
//!
//! **SAN entries**: hostname, hostname.domain, hostname.localnet, mesh IP, LAN IP(s).

use std::{
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

pub mod authority;
pub mod enroll;

pub use authority::CertAuthority;
pub use enroll::EnrollmentClient;

/// Pending enrollment request awaiting approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEnrollment {
    /// Hostname from CSR subject CN
    pub hostname: String,
    /// CSR in PEM format
    pub csr_pem: String,
    /// When the request was received
    pub requested_at: chrono::DateTime<chrono::Utc>,
    /// Existing client cert fingerprint, if reconnecting
    pub existing_cert_fingerprint: Option<String>,
}

/// Approved enrollment with signed certificate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedEnrollment {
    pub hostname: String,
    /// Signed certificate in PEM format
    pub cert_pem: String,
    /// CA certificate in PEM format
    pub ca_cert_pem: String,
    pub approved_at: chrono::DateTime<chrono::Utc>,
}

/// Certificate validity period: 14 days, renewed daily.
pub const CERT_VALIDITY_DAYS: i64 = 14;

/// CA configuration in `xdebbied.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaConfig {
    /// Path to CA certificate (public)
    pub ca_cert: PathBuf,
    /// Path to CA private key
    pub ca_key: PathBuf,
    /// Domain for the cluster (e.g., "harlem.bmath.nyc")
    pub domain: String,
}
