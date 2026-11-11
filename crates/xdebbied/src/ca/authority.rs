// SPDX-License-Identifier: GPL-3.0-or-later
//! CA authority: approves enrollment requests and signs certificates.

use super::*;
use rcgen::{
    CertificateSigningRequestParams, DistinguishedName,
    DnType, ExtendedKeyUsagePurpose, Issuer, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use std::collections::BTreeMap;
use time::OffsetDateTime;

/// The CA authority that signs certificates.
pub struct CertAuthority {
    config: CaConfig,
    ca_key: KeyPair,
    ca_cert_pem: String,
    /// hostname -> PendingEnrollment
    pending: Arc<RwLock<BTreeMap<String, PendingEnrollment>>>,
}

impl CertAuthority {
    /// Load the CA from disk.
    pub fn load(config: CaConfig) -> Result<Self> {
        let ca_key_pem = std::fs::read_to_string(&config.ca_key)
            .with_context(|| format!("reading {}", config.ca_key.display()))?;
        let ca_key = KeyPair::from_pem(&ca_key_pem)
            .context("parsing CA private key")?;
        
        let ca_cert_pem = std::fs::read_to_string(&config.ca_cert)
            .with_context(|| format!("reading {}", config.ca_cert.display()))?;
        
        Ok(Self {
            config,
            ca_key,
            ca_cert_pem,
            pending: Arc::new(RwLock::new(BTreeMap::new())),
        })
    }

    /// Accept a CSR. If the requester has a valid client cert, approve immediately.
    /// Otherwise, queue for approval.
    pub async fn submit_csr(
        &self,
        csr_pem: &str,
        existing_cert_fingerprint: Option<String>,
    ) -> Result<Option<ApprovedEnrollment>> {
        let csr = CertificateSigningRequestParams::from_pem(csr_pem)
            .context("parsing CSR")?;
        
        let hostname = extract_hostname(&csr)?;
        
        // Auto-approve if reconnecting with a valid cert
        if existing_cert_fingerprint.is_some() {
            // TODO: verify the fingerprint against known hosts
            return Ok(Some(self.sign_csr(&csr, &hostname).await?));
        }
        
        // Queue for manual approval
        let pending = PendingEnrollment {
            hostname: hostname.clone(),
            csr_pem: csr_pem.to_string(),
            requested_at: chrono::Utc::now(),
            existing_cert_fingerprint,
        };
        
        self.pending.write().await.insert(hostname, pending);
        Ok(None)
    }

    /// List pending enrollments.
    pub async fn list_pending(&self) -> Vec<PendingEnrollment> {
        self.pending.read().await.values().cloned().collect()
    }

    /// Approve a pending enrollment by hostname.
    pub async fn approve(&self, hostname: &str) -> Result<ApprovedEnrollment> {
        let pending = self.pending.write().await.remove(hostname)
            .ok_or_else(|| anyhow::anyhow!("no pending enrollment for {}", hostname))?;
        
        let csr = CertificateSigningRequestParams::from_pem(&pending.csr_pem)?;
        self.sign_csr(&csr, hostname).await
    }

    /// Reject a pending enrollment.
    pub async fn reject(&self, hostname: &str) -> Result<()> {
        self.pending.write().await.remove(hostname)
            .ok_or_else(|| anyhow::anyhow!("no pending enrollment for {}", hostname))?;
        Ok(())
    }

    /// Sign a CSR with the CA key.
    async fn sign_csr(
        &self,
        csr: &CertificateSigningRequestParams,
        hostname: &str,
    ) -> Result<ApprovedEnrollment> {
        // Build SAN from CSR attributes + hostname + domain
        let mut san = vec![
            SanType::DnsName(hostname.to_string().try_into()?),
            SanType::DnsName(format!("{}.localnet", hostname).try_into()?),
        ];
        
        if !self.config.domain.is_empty() {
            san.push(SanType::DnsName(
                format!("{}.{}", hostname, self.config.domain).try_into()?
            ));
        }
        
        // Extract IPs from CSR SAN if present
        for existing_san in &csr.params.subject_alt_names {
            if let SanType::IpAddress(ip) = existing_san {
                san.push(SanType::IpAddress(*ip));
            }
        }
        
        let issuer = Issuer::from_ca_cert_pem(&self.ca_cert_pem, &self.ca_key)?;
        
        let mut params = csr.params.clone();
        params.subject_alt_names = san;
        params.not_before = OffsetDateTime::now_utc();
        params.not_after = OffsetDateTime::now_utc() 
            + time::Duration::days(CERT_VALIDITY_DAYS);
        params.is_ca = IsCa::NoCa;
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        
        let csr_with_updated_params = CertificateSigningRequestParams {
            params,
            public_key: csr.public_key.clone(),
        };
        
        let cert = csr_with_updated_params.signed_by(&issuer)?;
        
        Ok(ApprovedEnrollment {
            hostname: hostname.to_string(),
            cert_pem: cert.pem(),
            ca_cert_pem: self.ca_cert_pem.clone(),
            approved_at: chrono::Utc::now(),
        })
    }

    /// Get the CA certificate in PEM format.
    pub fn ca_cert_pem(&self) -> &str {
        &self.ca_cert_pem
    }
}

fn extract_hostname(csr: &CertificateSigningRequestParams) -> Result<String> {
    // CN from subject - handle common types (PrintableString, Utf8String, TeletexString have AsRef<str>)
    for (ty, val) in csr.params.distinguished_name.iter() {
        if matches!(ty, DnType::CommonName) {
            use rcgen::DnValue;
            let s = match val {
                DnValue::PrintableString(ps) => ps.as_ref().to_string(),
                DnValue::Utf8String(utf8) => utf8.clone(),
                DnValue::TeletexString(ts) => ts.as_ref().to_string(),
                // UniversalString and BmpString are rare for CN; bail if encountered
                _ => bail!("unsupported DN string type in CN"),
            };
            return Ok(s);
        }
    }
    
    // First DNS SAN without dots (bare hostname)
    for san in &csr.params.subject_alt_names {
        if let SanType::DnsName(name) = san {
            let s: &str = name.as_ref();
            if !s.contains('.') {
                return Ok(s.to_string());
            }
        }
    }
    
    bail!("CSR has no hostname in CN or SAN")
}
