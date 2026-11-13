// SPDX-License-Identifier: GPL-3.0-or-later
//! TLS certificate generation for the xdebbied API server.
//!
//! Generates a self-signed CA and a server certificate signed by it, stored
//! in `/etc/xdebbie/`:
//!
//! ```text
//! /etc/xdebbie/api-ca.pem      root:xdebbie 0440   CA cert (distribute to peers / xdctl)
//! /etc/xdebbie/api-cert.pem    root:xdebbie 0440   server cert
//! /etc/xdebbie/api-key.pem     root:root    0400   server private key
//! ```
//!
//! The CA cert is group-readable so members of the `xdebbie` group can hand
//! it to `xdctl` (`XDEBBIE_CA`) without sudo. The key is root-only.
//!
//! All crypto is done in-process via `rcgen` (backed by `aws-lc-rs`) — no
//! openssl binary required.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use tracing::info;

pub const API_CA_CERT: &str = "/etc/xdebbie/api-ca.pem";
pub const API_SERVER_CERT: &str = "/etc/xdebbie/api-cert.pem";
pub const API_SERVER_KEY: &str = "/etc/xdebbie/api-key.pem";

/// CA validity: 10 years. Server cert: 2 years.
const CA_DAYS: i64 = 3650;
const SERVER_DAYS: i64 = 730;

/// Paths for the three API TLS files.
pub struct ApiTlsPaths {
    pub ca_cert: PathBuf,
    pub server_cert: PathBuf,
    pub server_key: PathBuf,
}

impl Default for ApiTlsPaths {
    fn default() -> Self {
        Self {
            ca_cert: PathBuf::from(API_CA_CERT),
            server_cert: PathBuf::from(API_SERVER_CERT),
            server_key: PathBuf::from(API_SERVER_KEY),
        }
    }
}

impl ApiTlsPaths {
    pub fn all_exist(&self) -> bool {
        self.ca_cert.exists() && self.server_cert.exists() && self.server_key.exists()
    }
}

/// Generate (or regenerate) the CA and server cert for the xdebbie API.
///
/// If `force` is false and all three files already exist, this is a no-op.
pub fn ensure_api_tls(hostname: &str, force: bool, paths: &ApiTlsPaths) -> Result<()> {
    if paths.all_exist() && !force {
        info!(
            "API TLS certs already exist ({}, {}, {}); skipping",
            paths.ca_cert.display(),
            paths.server_cert.display(),
            paths.server_key.display(),
        );
        return Ok(());
    }
    generate(hostname, &server_sans(hostname), paths)
}

/// Generate CA + server cert with the given SANs and write them to `paths`.
fn generate(hostname: &str, sans: &[SanType], paths: &ApiTlsPaths) -> Result<()> {
    info!("Generating API TLS certificates for {}", hostname);
    let now = time::OffsetDateTime::now_utc();
    // A minute of slack so a peer with a slightly-behind clock accepts it.
    let not_before = now - time::Duration::minutes(1);

    // ---- CA ----
    let ca_key = KeyPair::generate().context("generating CA key")?;
    let mut ca_params = CertificateParams::default();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.distinguished_name.push(DnType::CommonName, format!("xdebbie API CA ({hostname})"));
    ca_params.distinguished_name.push(DnType::OrganizationName, "xdebbie");
    ca_params.not_before = not_before;
    ca_params.not_after = now + time::Duration::days(CA_DAYS);
    let ca_cert = ca_params.self_signed(&ca_key).context("self-signing CA cert")?;
    let issuer = Issuer::new(ca_params, ca_key);

    // ---- Server cert ----
    let server_key = KeyPair::generate().context("generating server key")?;
    let mut srv_params = CertificateParams::default();
    srv_params.subject_alt_names = sans.to_vec();
    srv_params.is_ca = IsCa::NoCa;
    srv_params.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
    srv_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    srv_params.distinguished_name.push(DnType::CommonName, hostname);
    srv_params.distinguished_name.push(DnType::OrganizationName, "xdebbie");
    srv_params.not_before = not_before;
    srv_params.not_after = now + time::Duration::days(SERVER_DAYS);
    let server_cert = srv_params.signed_by(&server_key, &issuer).context("signing server cert")?;

    // ---- Write files ----
    crate::token::write_group_readable(&paths.ca_cert, &ca_cert.pem())
        .with_context(|| format!("writing {}", paths.ca_cert.display()))?;
    crate::token::write_group_readable(&paths.server_cert, &server_cert.pem())
        .with_context(|| format!("writing {}", paths.server_cert.display()))?;
    crate::token::write_private(&paths.server_key, &server_key.serialize_pem())
        .with_context(|| format!("writing {}", paths.server_key.display()))?;

    info!(
        "API TLS: CA={} cert={} key={} SANs={}",
        paths.ca_cert.display(),
        paths.server_cert.display(),
        paths.server_key.display(),
        sans.iter().map(san_label).collect::<Vec<_>>().join(","),
    );
    Ok(())
}

fn san_label(s: &SanType) -> String {
    match s {
        SanType::DnsName(n) => format!("DNS:{}", n.as_str()),
        SanType::IpAddress(ip) => format!("IP:{ip}"),
        other => format!("{other:?}"),
    }
}

/// SubjectAltName list for the server cert: short hostname, FQDN, and every
/// global-scope IPv4 address currently assigned to this host — so the cert
/// verifies whether xdctl is pointed at a name or an address.
fn server_sans(hostname: &str) -> Vec<SanType> {
    let mut sans: Vec<SanType> = vec![];
    let mut push_dns = |name: &str| {
        if let Ok(n) = name.try_into() {
            let s = SanType::DnsName(n);
            if !sans.contains(&s) {
                sans.push(s);
            }
        }
    };
    push_dns(hostname);
    if let Some(short) = hostname.split('.').next() {
        push_dns(short);
    }
    if let Ok(out) = std::process::Command::new("hostname").arg("-f").output() {
        let fqdn = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !fqdn.is_empty() {
            push_dns(&fqdn);
        }
    }
    if let Ok(out) = std::process::Command::new("ip").args(["-4", "-o", "addr", "show", "scope", "global"]).output() {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut f = line.split_whitespace();
            if f.any(|w| w == "inet")
                && let Some(addr) = f.next().and_then(|s| s.split('/').next())
                && let Ok(ip) = addr.parse::<std::net::IpAddr>()
            {
                sans.push(SanType::IpAddress(ip));
            }
        }
    }
    sans
}

/// PEM-encoded CA cert at `path` as a `reqwest` root certificate; `None`
/// (with a warning) if it cannot be read or parsed.
pub fn load_ca_cert(path: &Path) -> Option<reqwest::tls::Certificate> {
    let pem = std::fs::read(path)
        .map_err(|e| tracing::warn!("cannot read CA cert {}: {e}", path.display()))
        .ok()?;
    reqwest::tls::Certificate::from_pem(&pem)
        .map_err(|e| tracing::warn!("cannot parse CA cert {}: {e}", path.display()))
        .ok()
}

/// Every PEM certificate in `dir` matching `*.pem` that parses as a CA
/// bundle entry. Lets an operator drop peer CA certs into
/// `/etc/xdebbie/peers/` without editing config.
pub fn load_ca_dir(dir: &Path) -> Vec<reqwest::tls::Certificate> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "pem")
            && let Some(c) = load_ca_cert(&p)
        {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ApiTlsPaths {
            ca_cert: dir.path().join("ca.pem"),
            server_cert: dir.path().join("cert.pem"),
            server_key: dir.path().join("key.pem"),
        };
        let sans = vec![
            SanType::DnsName("testhost".try_into().unwrap()),
            SanType::IpAddress("192.0.2.1".parse().unwrap()),
        ];
        generate("testhost", &sans, &paths).unwrap();
        assert!(paths.all_exist());

        // Idempotent through the public entry point.
        ensure_api_tls("testhost", false, &paths).unwrap();

        let ca = std::fs::read_to_string(&paths.ca_cert).unwrap();
        assert!(ca.contains("BEGIN CERTIFICATE"));
        let cert = std::fs::read_to_string(&paths.server_cert).unwrap();
        assert!(cert.contains("BEGIN CERTIFICATE"));
        let key = std::fs::read_to_string(&paths.server_key).unwrap();
        assert!(key.contains("PRIVATE KEY"));

        // reqwest and rustls both accept the CA; the server cert chains to it
        // and carries the SANs.
        assert!(load_ca_cert(&paths.ca_cert).is_some());
        let (_, pem) = x509_parser::pem::parse_x509_pem(cert.as_bytes()).unwrap();
        let (_, x) = x509_parser::parse_x509_certificate(&pem.contents).unwrap();
        let san = x.subject_alternative_name().unwrap().unwrap();
        assert_eq!(san.value.general_names.len(), 2);
        assert!(x.validity().is_valid());
    }
}
