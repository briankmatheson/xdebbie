// SPDX-License-Identifier: GPL-3.0-or-later
//! Enrollment client: generate keys, submit CSRs, poll for approval, install certs.

use super::*;
use rcgen::{CertificateParams, KeyPair, SanType};
use std::net::IpAddr;

/// Client for enrolling with the CA.
pub struct EnrollmentClient {
    ca_url: String,
    token: Option<String>,
    domain: String,
}

impl EnrollmentClient {
    pub fn new(ca_url: impl Into<String>, token: Option<String>, domain: impl Into<String>) -> Self {
        Self {
            ca_url: ca_url.into(),
            token,
            domain: domain.into(),
        }
    }

    /// Generate a CSR for this host with the given IPs in the SAN.
    pub fn generate_csr(
        &self,
        hostname: &str,
        ips: &[IpAddr],
    ) -> Result<(String, String)> {
        let key = KeyPair::generate()?;
        let key_pem = key.serialize_pem();
        
        let mut params = CertificateParams::new(vec![hostname.to_string()])?;
        params.distinguished_name.push(
            rcgen::DnType::CommonName,
            hostname,
        );
        params.distinguished_name.push(
            rcgen::DnType::OrganizationName,
            &self.domain,
        );
        
        // SAN: hostname, hostname.localnet, hostname.domain, IPs
        let mut san = vec![
            SanType::DnsName(hostname.to_string().try_into()?),
            SanType::DnsName(format!("{}.localnet", hostname).try_into()?),
        ];
        
        if !self.domain.is_empty() {
            san.push(SanType::DnsName(
                format!("{}.{}", hostname, self.domain).try_into()?
            ));
        }
        
        for ip in ips {
            san.push(SanType::IpAddress(*ip));
        }
        
        params.subject_alt_names = san;
        
        let csr = params.serialize_request(&key)?;
        Ok((csr.pem()?, key_pem))
    }

    /// Submit a CSR to the CA. Returns Some(cert) if auto-approved, None if pending.
    pub async fn submit_csr(
        &self,
        csr_pem: &str,
        existing_cert_fingerprint: Option<String>,
    ) -> Result<Option<ApprovedEnrollment>> {
        // TODO: HTTP POST to CA endpoint
        // For now, return None (pending)
        Ok(None)
    }

    /// Poll for approval of a pending enrollment.
    pub async fn poll_approval(&self, hostname: &str) -> Result<Option<ApprovedEnrollment>> {
        // TODO: HTTP GET to CA endpoint
        Ok(None)
    }

    /// Fetch the CA certificate.
    pub async fn fetch_ca_cert(&self) -> Result<String> {
        // TODO: HTTP GET /v1/ca/cert
        bail!("not implemented")
    }
}
