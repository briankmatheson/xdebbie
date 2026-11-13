// SPDX-License-Identifier: GPL-3.0-or-later
//! `xdebbied bootstrap --join <url>`: enrol this box with the cluster CA.
//!
//! 1. Fetch the CA certificate from the CA box and pin it: the operator
//!    confirms its fingerprint (or passes `--ca-fingerprint`). Everything
//!    after this verifies against that CA.
//! 2. Generate a key and CSR, `POST /v1/enroll`, remember the id.
//! 3. Poll until an operator runs `xdctl approve <hostname>`; install the
//!    issued certificate and the CA. The approval also carries the box id
//!    the CA assigned (the bridge MAC and underlay address derive from it).
//! 4. Fetch the cluster token over mTLS (`GET /v1/enroll/token`).
//!
//! Re-running resumes a pending enrolment (the id is kept in
//! `<state_dir>/join.json`); `--force` starts over with a new key.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use xdebbie_core::{EnrollRequest, EnrollStatus, Enrollment, TokenSet};

use super::api_tls::{self, ApiTlsPaths};

const POLL: Duration = Duration::from_secs(3);
const REMIND_EVERY: u32 = 20; // polls

/// What a previous run left behind, so a restart resumes instead of
/// queuing a second request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinState {
    pub ca_url: String,
    pub ca_fingerprint: String,
    pub enrollment_id: String,
    pub key_fingerprint: String,
}

pub struct JoinOptions<'a> {
    pub ca_url: &'a str,
    pub expect_fingerprint: Option<&'a str>,
    pub hostname: &'a str,
    /// Box id to ask for; `None` lets the CA box pick the next free one.
    pub box_id: Option<u8>,
    pub api_url: &'a str,
    pub force: bool,
    pub paths: &'a ApiTlsPaths,
    pub state_dir: &'a Path,
}

fn norm_fp(s: &str) -> String {
    s.trim().trim_start_matches("sha256:").to_ascii_lowercase()
}

fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("join.json")
}

fn load_state(state_dir: &Path) -> Option<JoinState> {
    let b = std::fs::read(state_path(state_dir)).ok()?;
    serde_json::from_slice(&b).ok()
}

fn save_state(state_dir: &Path, st: &JoinState) -> Result<()> {
    std::fs::create_dir_all(state_dir)?;
    std::fs::write(state_path(state_dir), serde_json::to_vec_pretty(st)?)?;
    Ok(())
}

/// Fetch the CA from `ca_url` without verifying anything, and decide
/// whether to trust it: pinned fingerprint, a fingerprint accepted on an
/// earlier run, or the operator at the terminal.
fn fetch_and_pin_ca(ca_url: &str, expect: Option<&str>, prior: Option<&JoinState>) -> Result<(String, String)> {
    let insecure = Client::builder()
        .use_rustls_tls()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(15))
        .build()?;
    let r = insecure
        .get(format!("{ca_url}/v1/ca"))
        .send()
        .with_context(|| format!("contacting {ca_url} (is xdebbied running there, over https?)"))?;
    if !r.status().is_success() {
        bail!("{ca_url}/v1/ca: {} — is that the box bootstrapped first (it holds the CA key)?", r.status());
    }
    let pem = r.text()?;
    let fp = api_tls::cert_fingerprint(&pem).context("CA certificate from the CA box")?;

    if let Some(want) = expect {
        if norm_fp(want) != norm_fp(&fp) {
            bail!("cluster CA at {ca_url} has fingerprint {fp}, expected {want}: not joining");
        }
        info!("Cluster CA fingerprint {fp} matches --ca-fingerprint");
        return Ok((pem, fp));
    }
    if prior.is_some_and(|p| p.ca_url == ca_url && norm_fp(&p.ca_fingerprint) == norm_fp(&fp)) {
        info!("Cluster CA fingerprint {fp} accepted on an earlier run");
        return Ok((pem, fp));
    }
    // SAFETY: plain libc query on fd 0.
    let tty = unsafe { libc::isatty(0) } == 1;
    if !tty {
        bail!(
            "cluster CA at {ca_url} is not yet trusted (fingerprint {fp}). Re-run with \
             --ca-fingerprint {fp} after checking it against `xdebbied bootstrap` output or \
             `xdctl ca` on the CA box"
        );
    }
    eprintln!();
    eprintln!("Cluster CA at {ca_url}");
    eprintln!("  fingerprint {fp}");
    eprintln!("Compare with `xdctl ca` (or the bootstrap output) on that box.");
    eprint!("Trust this CA and ask it to sign this box's certificate? [y/N] ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    if !matches!(line.trim(), "y" | "Y" | "yes") {
        bail!("not joining");
    }
    Ok((pem, fp))
}

fn trusted_client(ca_pem: &str, identity: Option<reqwest::Identity>) -> Result<Client> {
    let mut b = Client::builder()
        .use_rustls_tls()
        .add_root_certificate(reqwest::Certificate::from_pem(ca_pem.as_bytes())?)
        .timeout(Duration::from_secs(15));
    if let Some(id) = identity {
        b = b.identity(id);
    }
    Ok(b.build()?)
}

fn get_enrollment(c: &Client, ca_url: &str, id: &str) -> Result<Option<Enrollment>> {
    let r = c.get(format!("{ca_url}/v1/enroll/{id}")).send().context("polling enrolment")?;
    match r.status() {
        s if s.is_success() => Ok(Some(r.json()?)),
        reqwest::StatusCode::NOT_FOUND => Ok(None),
        s => bail!("{ca_url}/v1/enroll/{id}: {s}: {}", r.text().unwrap_or_default()),
    }
}

/// What enrolment hands back: the cluster token set and the box id the CA
/// assigned (kept across re-enrolments of the same host).
pub struct Joined {
    pub tokens: TokenSet,
    pub box_id: u8,
}

/// Run the whole enrolment.
pub fn join(o: &JoinOptions<'_>) -> Result<Joined> {
    let ca_url = o.ca_url.trim_end_matches('/');
    if !ca_url.starts_with("https://") {
        bail!("--join must be an https:// URL (got {ca_url}); enrolment never runs over plain http");
    }
    let prior = load_state(o.state_dir);
    let (ca_pem, ca_fp) = fetch_and_pin_ca(ca_url, o.expect_fingerprint, prior.as_ref())?;
    let c = trusted_client(&ca_pem, None)?;

    // Resume a pending request if the key it was made with is still here.
    let mut resumed = None;
    if !o.force
        && let Some(p) = &prior
        && p.ca_url == ca_url
        && o.paths.server_key.exists()
        && let Some(e) = get_enrollment(&c, ca_url, &p.enrollment_id)?
        && e.fingerprint == p.key_fingerprint
        && e.status != EnrollStatus::Denied
    {
        info!("Resuming enrolment {} ({})", e.id, e.status);
        resumed = Some(e);
    }

    let mut e = match resumed {
        Some(e) => e,
        None => {
            // Never silently re-key: a cert the CA already issued for the
            // key here would become an orphan needing a second approval.
            if !o.force && o.paths.server_key.exists() {
                bail!(
                    "{} exists but no enrolment to resume (state {} unreadable, different CA, or denied); \
                     pass --force to start over with a new key",
                    o.paths.server_key.display(),
                    state_path(o.state_dir).display()
                );
            }
            let csr = api_tls::make_csr(o.hostname)?;
            crate::token::write_private(&o.paths.server_key, &csr.key_pem)
                .with_context(|| format!("writing {}", o.paths.server_key.display()))?;
            let req = EnrollRequest {
                hostname: o.hostname.to_string(),
                box_id: o.box_id,
                api_url: o.api_url.to_string(),
                csr: csr.csr_pem,
                wg_pubkey: None,
            };
            let r = c.post(format!("{ca_url}/v1/enroll")).json(&req).send().context("submitting enrolment")?;
            if !r.status().is_success() {
                bail!("{ca_url}/v1/enroll: {}: {}", r.status(), r.text().unwrap_or_default());
            }
            let e: Enrollment = r.json().context("parsing enrolment reply")?;
            if e.fingerprint != csr.fingerprint {
                bail!("CA box echoed key fingerprint {} for our {}: not continuing", e.fingerprint, csr.fingerprint);
            }
            save_state(
                o.state_dir,
                &JoinState { ca_url: ca_url.into(), ca_fingerprint: ca_fp.clone(), enrollment_id: e.id.clone(), key_fingerprint: e.fingerprint.clone() },
            )?;
            e
        }
    };

    if e.status == EnrollStatus::Pending {
        eprintln!();
        eprintln!("Enrolment {} is waiting for approval. On the CA box (or any member) run:", e.id);
        eprintln!();
        eprintln!("    xdctl approve {}", o.hostname);
        eprintln!();
        eprintln!("  key fingerprint {}", e.fingerprint);
        eprintln!("(Ctrl-C and re-run `xdebbied bootstrap --join` later to pick up where this left off.)");
        let mut n = 0u32;
        loop {
            std::thread::sleep(POLL);
            n += 1;
            match get_enrollment(&c, ca_url, &e.id) {
                Ok(Some(cur)) => {
                    e = cur;
                    if e.status != EnrollStatus::Pending {
                        break;
                    }
                }
                Ok(None) => bail!("enrolment {} disappeared on {ca_url}; re-run with --force", e.id),
                Err(err) => warn!("{err:#}; retrying"),
            }
            if n % REMIND_EVERY == 0 {
                info!("still waiting for `xdctl approve {}` ({}s)", o.hostname, n as u64 * POLL.as_secs());
            }
        }
    }
    match e.status {
        EnrollStatus::Approved => {}
        EnrollStatus::Denied => bail!("enrolment {} was denied on {ca_url}; re-run with --force to ask again", e.id),
        EnrollStatus::Pending => unreachable!(),
    }
    let (Some(cert), Some(ca)) = (&e.cert, &e.ca) else {
        bail!("enrolment {} approved but no certificate was returned", e.id);
    };
    if api_tls::cert_fingerprint(ca)? != ca_fp {
        bail!("CA returned with the approval differs from the one pinned; not installing");
    }
    api_tls::install_issued(o.paths, cert, ca)?;
    info!("Enrolment {} approved as box {}; certificate installed", e.id, e.box_id);

    // Now we are a member: fetch the token with our certificate.
    let key = std::fs::read_to_string(&o.paths.server_key)?;
    let identity = reqwest::Identity::from_pem(format!("{cert}\n{key}").as_bytes()).context("client identity")?;
    let m = trusted_client(&ca_pem, Some(identity))?;
    let r = m.get(format!("{ca_url}/v1/enroll/token")).send().context("fetching cluster token")?;
    if !r.status().is_success() {
        bail!("{ca_url}/v1/enroll/token: {}: {}", r.status(), r.text().unwrap_or_default());
    }
    let set: TokenSet = r.json().context("parsing token")?;
    info!("Received the cluster token ({})", crate::token::fingerprint(&set.current));
    Ok(Joined { tokens: set, box_id: e.box_id })
}

/// The CA box's `[network]` as it serves it (`GET /v1/network`), for a
/// joiner to copy the overlay from. `None` when that box has no `[network]`.
/// Uses the certificate installed by [`join`]; call after it.
pub fn fetch_network(ca_url: &str, paths: &ApiTlsPaths) -> Result<Option<crate::dhcp::NetworkStatus>> {
    let ca_url = ca_url.trim_end_matches('/');
    let ca_pem = std::fs::read_to_string(&paths.ca_cert)?;
    let cert = std::fs::read_to_string(&paths.server_cert)?;
    let key = std::fs::read_to_string(&paths.server_key)?;
    let identity = reqwest::Identity::from_pem(format!("{cert}\n{key}").as_bytes()).context("client identity")?;
    let c = trusted_client(&ca_pem, Some(identity))?;
    let r = c.get(format!("{ca_url}/v1/network")).send().context("fetching the cluster network")?;
    match r.status() {
        s if s.is_success() => Ok(Some(r.json().context("parsing /v1/network")?)),
        reqwest::StatusCode::NOT_FOUND => Ok(None),
        s => bail!("{ca_url}/v1/network: {s}: {}", r.text().unwrap_or_default()),
    }
}
