// SPDX-License-Identifier: GPL-3.0-or-later
//! TLS certificate generation for the xdebbied API server.
//!
//! One CA per cluster. The first box bootstrapped generates it and keeps
//! the key; every other box gets a certificate signed by it through
//! enrolment (`xdebbied bootstrap --join`, `xdctl approve`). Files in
//! `/etc/xdebbie/`:
//!
//! ```text
//! /etc/xdebbie/api-ca.pem      root:xdebbie 0440   cluster CA cert (xdctl: XDEBBIE_CA)
//! /etc/xdebbie/api-ca-key.pem  root:root    0400   CA key (first box only)
//! /etc/xdebbie/api-cert.pem    root:xdebbie 0440   this box's cert (serverAuth + clientAuth)
//! /etc/xdebbie/api-key.pem     root:root    0400   this box's private key
//! ```
//!
//! The CA cert is group-readable so members of the `xdebbie` group can hand
//! it to `xdctl` (`XDEBBIE_CA`) without sudo. Keys are root-only.
//!
//! All crypto is done in-process via `rcgen` (backed by `aws-lc-rs`) — no
//! openssl binary required.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, PublicKeyData, SanType,
};
use tracing::info;

pub const API_CA_CERT: &str = "/etc/xdebbie/api-ca.pem";
pub const API_SERVER_CERT: &str = "/etc/xdebbie/api-cert.pem";
pub const API_SERVER_KEY: &str = "/etc/xdebbie/api-key.pem";
pub const API_CA_KEY: &str = "/etc/xdebbie/api-ca-key.pem";

/// CA validity: 10 years. Server cert: 2 years.
const CA_DAYS: i64 = 3650;
const SERVER_DAYS: i64 = 730;

/// Paths for the API TLS files.
pub struct ApiTlsPaths {
    pub ca_cert: PathBuf,
    pub ca_key: PathBuf,
    pub server_cert: PathBuf,
    pub server_key: PathBuf,
}

impl Default for ApiTlsPaths {
    fn default() -> Self {
        Self {
            ca_cert: PathBuf::from(API_CA_CERT),
            ca_key: PathBuf::from(API_CA_KEY),
            server_cert: PathBuf::from(API_SERVER_CERT),
            server_key: PathBuf::from(API_SERVER_KEY),
        }
    }
}

impl ApiTlsPaths {
    /// CA cert, server cert and server key are present (the CA key need
    /// not be: a joined box never has it).
    pub fn all_exist(&self) -> bool {
        self.ca_cert.exists() && self.server_cert.exists() && self.server_key.exists()
    }
    /// This box holds the cluster CA key and can sign enrolments.
    pub fn is_ca(&self) -> bool {
        self.ca_key.exists()
    }
}

/// Make sure this box has a CA and a server cert signed by it.
///
/// - Nothing yet: generate both.
/// - CA key present: keep the CA (members' certs chain to it); reissue the
///   server cert only with `force` or when it is missing.
/// - Certs present but no CA key (pre-0.14 bootstrap, or a box that was
///   enrolled by another CA): leave alone unless `force`, which starts a
///   fresh CA — every other box must then re-enrol.
///
/// A box joining an existing cluster does not come here; see
/// [`make_csr`] and [`install_issued`].
pub fn ensure_api_tls(hostname: &str, force: bool, paths: &ApiTlsPaths) -> Result<()> {
    let sans = server_sans(hostname);
    if paths.is_ca() {
        if paths.all_exist() && !force {
            info!(
                "API TLS certs already exist (CA key at {}); skipping",
                paths.ca_key.display()
            );
            return Ok(());
        }
        info!(
            "Reissuing server cert for {} from the existing CA",
            hostname
        );
        let signer = CaSigner::load(&paths.ca_cert, &paths.ca_key)?
            .ok_or_else(|| anyhow::anyhow!("{} vanished", paths.ca_key.display()))?;
        let key = KeyPair::generate().context("generating server key")?;
        let cert = signer.issue(hostname, &sans, &key)?;
        crate::token::write_private(&paths.server_key, &key.serialize_pem())
            .with_context(|| format!("writing {}", paths.server_key.display()))?;
        crate::token::write_group_readable(&paths.server_cert, &cert)
            .with_context(|| format!("writing {}", paths.server_cert.display()))?;
        return Ok(());
    }
    if paths.all_exist() && !force {
        info!(
            "API TLS certs already exist ({}, {}, {}) but there is no CA key at {}; \
             keeping them. Run `bootstrap --force` on the box that should hold the \
             cluster CA, or `bootstrap --join` on the others",
            paths.ca_cert.display(),
            paths.server_cert.display(),
            paths.server_key.display(),
            paths.ca_key.display(),
        );
        return Ok(());
    }
    if paths.server_cert.exists() {
        tracing::warn!(
            "Generating a NEW cluster CA (--force); boxes enrolled under the old one must re-run `bootstrap --join`"
        );
    }
    generate(hostname, &sans, paths)
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
    ca_params.distinguished_name.push(
        DnType::CommonName,
        format!("xdebbie cluster CA ({hostname})"),
    );
    ca_params
        .distinguished_name
        .push(DnType::OrganizationName, "xdebbie");
    ca_params.not_before = not_before;
    ca_params.not_after = now + time::Duration::days(CA_DAYS);
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .context("self-signing CA cert")?;
    let ca_key_pem = ca_key.serialize_pem();
    let signer = CaSigner {
        cert_pem: ca_cert.pem(),
        issuer: Issuer::new(ca_params, ca_key),
    };

    // ---- Server cert ----
    let server_key = KeyPair::generate().context("generating server key")?;
    let server_cert = signer.issue(hostname, sans, &server_key)?;

    // ---- Write files ----
    crate::token::write_group_readable(&paths.ca_cert, &signer.cert_pem)
        .with_context(|| format!("writing {}", paths.ca_cert.display()))?;
    crate::token::write_private(&paths.ca_key, &ca_key_pem)
        .with_context(|| format!("writing {}", paths.ca_key.display()))?;
    crate::token::write_group_readable(&paths.server_cert, &server_cert)
        .with_context(|| format!("writing {}", paths.server_cert.display()))?;
    crate::token::write_private(&paths.server_key, &server_key.serialize_pem())
        .with_context(|| format!("writing {}", paths.server_key.display()))?;

    info!(
        "API TLS: CA={} (key kept: this box signs enrolments) cert={} key={} SANs={}",
        paths.ca_cert.display(),
        paths.server_cert.display(),
        paths.server_key.display(),
        sans.iter().map(san_label).collect::<Vec<_>>().join(","),
    );
    Ok(())
}

/// The cluster CA: cert plus the key that signs member certificates. Only
/// the first-bootstrapped box has one.
pub struct CaSigner {
    cert_pem: String,
    issuer: Issuer<'static, KeyPair>,
}

impl CaSigner {
    /// `Ok(None)` when there is no CA key here (a member box).
    pub fn load(ca_cert: &Path, ca_key: &Path) -> Result<Option<Self>> {
        if !ca_key.exists() {
            return Ok(None);
        }
        let cert_pem = std::fs::read_to_string(ca_cert)
            .with_context(|| format!("reading {}", ca_cert.display()))?;
        let key_pem = std::fs::read_to_string(ca_key)
            .with_context(|| format!("reading {}", ca_key.display()))?;
        let key =
            KeyPair::from_pem(&key_pem).with_context(|| format!("parsing {}", ca_key.display()))?;
        let issuer = Issuer::from_ca_cert_pem(&cert_pem, key)
            .with_context(|| format!("parsing {}", ca_cert.display()))?;
        Ok(Some(Self { cert_pem, issuer }))
    }

    /// PEM of the CA certificate.
    pub fn ca_pem(&self) -> &str {
        &self.cert_pem
    }

    /// Sign a member's CSR. The subject and SANs come from the request;
    /// usage is fixed here: a member cert serves its API (serverAuth) and
    /// identifies the daemon to other boxes (clientAuth), never signs.
    pub fn sign_csr(&self, csr_pem: &str) -> Result<String> {
        let csr =
            rcgen::CertificateSigningRequestParams::from_pem(csr_pem).context("parsing CSR")?;
        let hostname = csr_hostname(&csr).unwrap_or_default();
        let sans = csr.params.subject_alt_names.clone();
        self.issue(&hostname, &sans, &csr.public_key)
    }

    /// Issue a member/server certificate for `public_key`.
    fn issue(
        &self,
        hostname: &str,
        sans: &[SanType],
        public_key: &impl PublicKeyData,
    ) -> Result<String> {
        let now = time::OffsetDateTime::now_utc();
        let mut p = CertificateParams::default();
        p.subject_alt_names = sans.to_vec();
        p.is_ca = IsCa::NoCa;
        // RFC 5280 wants an Authority Key Identifier on issued certs;
        // OpenSSL 3 in strict mode (Python >= 3.13's default verify flags)
        // rejects a chain without one: "Missing Authority Key Identifier".
        p.use_authority_key_identifier_extension = true;
        p.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        p.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        p.distinguished_name.push(DnType::CommonName, hostname);
        p.distinguished_name
            .push(DnType::OrganizationName, "xdebbie");
        p.not_before = now - time::Duration::minutes(1);
        p.not_after = now + time::Duration::days(SERVER_DAYS);
        Ok(p.signed_by(public_key, &self.issuer)
            .context("signing certificate")?
            .pem())
    }
}

fn csr_hostname(csr: &rcgen::CertificateSigningRequestParams) -> Option<String> {
    csr.params
        .distinguished_name
        .get(&DnType::CommonName)
        .map(|v| match v {
            rcgen::DnValue::PrintableString(s) => s.as_str().to_string(),
            rcgen::DnValue::Utf8String(s) => s.clone(),
            rcgen::DnValue::Ia5String(s) => s.as_str().to_string(),
            other => format!("{other:?}"),
        })
}

/// A new private key and a CSR for this box: CN = hostname, SANs as for a
/// locally generated server cert. The key PEM goes to `api-key.pem`; the
/// CSR to `POST /v1/enroll` on the CA box.
pub fn make_csr(hostname: &str) -> Result<Csr> {
    let key = KeyPair::generate().context("generating key")?;
    let mut p = CertificateParams::default();
    p.subject_alt_names = server_sans(hostname);
    p.distinguished_name.push(DnType::CommonName, hostname);
    p.distinguished_name
        .push(DnType::OrganizationName, "xdebbie");
    let csr = p.serialize_request(&key).context("building CSR")?;
    Ok(Csr {
        fingerprint: spki_fingerprint(key.subject_public_key_info().as_slice()),
        key_pem: key.serialize_pem(),
        csr_pem: csr.pem().context("encoding CSR")?,
    })
}

pub struct Csr {
    pub key_pem: String,
    pub csr_pem: String,
    /// `sha256:<hex>` of the public key, shown on both sides of an enrolment.
    pub fingerprint: String,
}

/// `sha256:<hex>` of a CSR's public key, as [`make_csr`] computed it on
/// the joiner.
pub fn csr_fingerprint(csr_pem: &str) -> Result<String> {
    let csr = rcgen::CertificateSigningRequestParams::from_pem(csr_pem).context("parsing CSR")?;
    Ok(spki_fingerprint(
        csr.public_key.subject_public_key_info().as_slice(),
    ))
}

fn spki_fingerprint(spki: &[u8]) -> String {
    use sha2::Digest;
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(spki)))
}

/// `sha256:<hex>` of a PEM certificate's DER — what an operator compares
/// when `bootstrap --join` asks whether to trust a CA.
pub fn cert_fingerprint(pem: &str) -> Result<String> {
    use sha2::Digest;
    let (_, p) =
        x509_parser::pem::parse_x509_pem(pem.as_bytes()).context("parsing certificate PEM")?;
    Ok(format!(
        "sha256:{}",
        hex::encode(sha2::Sha256::digest(&p.contents))
    ))
}

/// One certificate of a chain as `GET /v1/cert` reports it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CertInfo {
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
    /// `sha256:<hex>` of the DER.
    pub fingerprint: String,
    /// `true` when the certificate signed itself (a root).
    pub self_signed: bool,
    pub pem: String,
}

/// Describe every certificate in a PEM bundle, in file order.
pub fn describe_chain(pem: &str) -> Result<Vec<CertInfo>> {
    use sha2::Digest;
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let mut out = vec![];
    let mut rest = pem;
    while let Some(b) = rest.find(BEGIN) {
        let Some(e) = rest[b..].find(END) else {
            anyhow::bail!("certificate PEM without an END line");
        };
        let block = &rest[b..b + e + END.len()];
        rest = &rest[b + e + END.len()..];
        let (_, p) = x509_parser::pem::parse_x509_pem(block.as_bytes())
            .context("parsing certificate PEM")?;
        let (_, x) =
            x509_parser::parse_x509_certificate(&p.contents).context("parsing certificate DER")?;
        let v = x.validity();
        out.push(CertInfo {
            subject: x.subject().to_string(),
            issuer: x.issuer().to_string(),
            serial: x.raw_serial_as_string(),
            not_before: v.not_before.to_rfc2822().unwrap_or_default(),
            not_after: v.not_after.to_rfc2822().unwrap_or_default(),
            fingerprint: format!("sha256:{}", hex::encode(sha2::Sha256::digest(&p.contents))),
            self_signed: x.subject() == x.issuer(),
            pem: format!("{block}\n"),
        });
    }
    Ok(out)
}

/// Install a certificate issued by the cluster CA (and the CA itself) on a
/// joined box. The key was written by the caller when the CSR was made.
pub fn install_issued(paths: &ApiTlsPaths, cert_pem: &str, ca_pem: &str) -> Result<()> {
    crate::token::write_group_readable(&paths.ca_cert, ca_pem)
        .with_context(|| format!("writing {}", paths.ca_cert.display()))?;
    crate::token::write_group_readable(&paths.server_cert, cert_pem)
        .with_context(|| format!("writing {}", paths.server_cert.display()))?;
    // A joined box never signs; make sure a stale local CA key cannot
    // confuse `is_ca`.
    if paths.ca_key.exists() {
        std::fs::remove_file(&paths.ca_key)
            .with_context(|| format!("removing {}", paths.ca_key.display()))?;
    }
    info!(
        "Installed cluster CA at {} and issued cert at {}",
        paths.ca_cert.display(),
        paths.server_cert.display()
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

/// SubjectAltName list for the server cert: short hostname, FQDN, loopback
/// (so `xdctl` with its default `https://127.0.0.1:7443` verifies on the
/// box itself), and every global-scope IPv4 address currently assigned to
/// this host — so the cert verifies whether xdctl is pointed at a name or
/// an address.
fn server_sans(hostname: &str) -> Vec<SanType> {
    let mut sans: Vec<SanType> = vec![
        SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        SanType::IpAddress(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
    ];
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
    if let Ok(out) = std::process::Command::new("ip")
        .args(["-4", "-o", "addr", "show", "scope", "global"])
        .output()
    {
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
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
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

/// Test hook: a CA plus a server cert for `localhost`/127.0.0.1 in `dir`.
#[cfg(test)]
pub(crate) fn generate_localhost(dir: &Path) -> ApiTlsPaths {
    let paths = tests::test_paths(dir);
    let sans = vec![
        SanType::DnsName("localhost".try_into().unwrap()),
        SanType::IpAddress("127.0.0.1".parse().unwrap()),
    ];
    generate("localhost", &sans, &paths).unwrap();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn test_paths(dir: &Path) -> ApiTlsPaths {
        ApiTlsPaths {
            ca_cert: dir.join("ca.pem"),
            ca_key: dir.join("ca-key.pem"),
            server_cert: dir.join("cert.pem"),
            server_key: dir.join("key.pem"),
        }
    }

    fn parse(pem: &str) -> (Vec<u8>,) {
        let (_, p) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).unwrap();
        (p.contents,)
    }

    #[test]
    fn server_sans_include_loopback_and_names() {
        let sans = server_sans("gram.example");
        let has = |s: SanType| sans.contains(&s);
        assert!(has(SanType::IpAddress("127.0.0.1".parse().unwrap())));
        assert!(has(SanType::IpAddress("::1".parse().unwrap())));
        assert!(has(SanType::DnsName("gram.example".try_into().unwrap())));
        assert!(has(SanType::DnsName("gram".try_into().unwrap())));
    }

    #[test]
    fn csr_signed_by_ca_chains_and_has_both_ekus() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(dir.path());
        generate("ca-box", &server_sans("ca-box"), &paths).unwrap();
        assert!(paths.is_ca());

        // Joiner side.
        let csr = make_csr("joiner.example").unwrap();
        assert!(csr.csr_pem.contains("CERTIFICATE REQUEST"));
        assert!(csr.fingerprint.starts_with("sha256:"));
        assert_eq!(csr_fingerprint(&csr.csr_pem).unwrap(), csr.fingerprint);

        // CA side.
        let signer = CaSigner::load(&paths.ca_cert, &paths.ca_key)
            .unwrap()
            .unwrap();
        let cert_pem = signer.sign_csr(&csr.csr_pem).unwrap();
        let (der,) = parse(&cert_pem);
        let (_, x) = x509_parser::parse_x509_certificate(&der).unwrap();
        assert_eq!(
            x.subject()
                .iter_common_name()
                .next()
                .unwrap()
                .as_str()
                .unwrap(),
            "joiner.example"
        );
        let eku = x.extended_key_usage().unwrap().unwrap().value;
        assert!(eku.server_auth && eku.client_auth);
        assert!(!x.is_ca());
        // Verifies with the CA's public key.
        let (ca_der,) = parse(signer.ca_pem());
        let (_, ca) = x509_parser::parse_x509_certificate(&ca_der).unwrap();
        x.verify_signature(Some(ca.public_key())).unwrap();

        // Installing on the joiner replaces cert + CA and drops any CA key.
        let jdir = tempfile::tempdir().unwrap();
        let jpaths = test_paths(jdir.path());
        std::fs::write(&jpaths.ca_key, "stale").unwrap();
        std::fs::write(&jpaths.server_key, &csr.key_pem).unwrap();
        install_issued(&jpaths, &cert_pem, signer.ca_pem()).unwrap();
        assert!(jpaths.all_exist() && !jpaths.is_ca());
        assert_eq!(
            cert_fingerprint(signer.ca_pem()).unwrap().len(),
            "sha256:".len() + 64
        );
    }

    #[test]
    fn force_keeps_the_ca_and_reissues_the_server_cert() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(dir.path());
        ensure_api_tls("box", false, &paths).unwrap();
        let ca1 = std::fs::read_to_string(&paths.ca_cert).unwrap();
        let cert1 = std::fs::read_to_string(&paths.server_cert).unwrap();
        ensure_api_tls("box", true, &paths).unwrap();
        assert_eq!(std::fs::read_to_string(&paths.ca_cert).unwrap(), ca1);
        assert_ne!(std::fs::read_to_string(&paths.server_cert).unwrap(), cert1);
        // Without a CA key and without force: untouched.
        std::fs::remove_file(&paths.ca_key).unwrap();
        let cert2 = std::fs::read_to_string(&paths.server_cert).unwrap();
        ensure_api_tls("box", false, &paths).unwrap();
        assert_eq!(std::fs::read_to_string(&paths.server_cert).unwrap(), cert2);
        assert!(!paths.is_ca());
    }

    #[test]
    fn generate_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(dir.path());
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

        // What `xdctl ca --chain` shows: the box's cert, then the CA that
        // signed it; only the CA is self-signed.
        let chain = describe_chain(&format!("{cert}{ca}")).unwrap();
        assert_eq!(chain.len(), 2);
        assert!(!chain[0].self_signed);
        assert!(chain[1].self_signed);
        assert_eq!(chain[0].issuer, chain[1].subject);
        assert_eq!(chain[1].fingerprint, cert_fingerprint(&ca).unwrap());
        assert!(chain[0].pem.starts_with("-----BEGIN CERTIFICATE-----"));
    }
}
