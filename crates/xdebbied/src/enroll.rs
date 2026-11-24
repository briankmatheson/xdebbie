// SPDX-License-Identifier: GPL-3.0-or-later
//! Enrolment queue: boxes asking the cluster CA to sign their certificate.
//!
//! A joiner (`xdebbied bootstrap --join`) posts a CSR; it sits here as
//! `pending` until an operator runs `xdctl approve <host>` (or denies it).
//! Approval signs the CSR with the CA key held by this box; the joiner polls
//! for the result, installs the certificate, and from then on authenticates
//! to every box with it (see `tls::ClientCert`).
//!
//! Only the box holding `api-ca-key.pem` can approve. Others forward
//! enrolment calls to it (`ca_url`), so `xdctl approve` works from anywhere.
//!
//! Persisted in `<state_dir>/enroll.json`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use xdebbie_core::{EnrollKind, EnrollRequest, EnrollStatus, Enrollment};

use crate::bootstrap::api_tls::{CaSigner, csr_fingerprint};

/// Pending requests kept at most; beyond that `submit` refuses, so a
/// misbehaving LAN cannot fill the disk.
const MAX_PENDING: usize = 64;

/// An xdj client's request nobody decided within this long is denied
/// (an admin approves it on the aviary page while the person waits).
const CLIENT_PENDING_MINUTES: i64 = 15;

/// A client name: what goes in its certificate's CN.
fn client_name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 63
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
}

/// Deny client requests left pending too long. True when it changed any.
fn expire(recs: &mut [Record]) -> bool {
    let cutoff = Utc::now() - chrono::Duration::minutes(CLIENT_PENDING_MINUTES);
    let mut changed = false;
    for r in recs.iter_mut() {
        if r.view.kind == EnrollKind::Client
            && r.view.status == EnrollStatus::Pending
            && r.view.requested_at < cutoff
        {
            r.view.status = EnrollStatus::Denied;
            r.view.decided_at = Some(Utc::now());
            changed = true;
        }
    }
    changed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    #[serde(flatten)]
    view: Enrollment,
    csr: String,
}

pub struct Enrollments {
    path: PathBuf,
    recs: RwLock<Vec<Record>>,
    signer: Option<CaSigner>,
    /// Box ids in use before any enrolment: this box's own and its
    /// configured mesh peers.
    reserved: Vec<u8>,
}

/// Highest mesh box id (`10.22.<box>.1`, `52:54:00:23:<box>:fe`).
pub const MAX_BOX: u8 = 253;

pub enum Decision {
    Approve,
    Deny,
}

impl Enrollments {
    /// Load `path` (missing = empty). `signer` is `None` on a member box.
    /// `reserved`: box ids already taken on this box (own id, config peers).
    pub fn load(path: PathBuf, signer: Option<CaSigner>, reserved: Vec<u8>) -> Result<Self> {
        let recs = match std::fs::read(&path) {
            Ok(b) => {
                serde_json::from_slice(&b).with_context(|| format!("parsing {}", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            path,
            recs: RwLock::new(recs),
            signer,
            reserved,
        })
    }

    /// Pick a box id for `hostname`: the one it had when last approved, else
    /// the requested one if free, else the lowest free. Denied requests do
    /// not hold an id.
    fn allocate(&self, recs: &[Record], hostname: &str, want: Option<u8>) -> Result<u8> {
        if let Some(prev) = recs
            .iter()
            .rev()
            .find(|r| r.view.hostname == hostname && r.view.status == EnrollStatus::Approved)
        {
            return Ok(prev.view.box_id);
        }
        let taken = |b: u8| {
            self.reserved.contains(&b)
                || recs
                    .iter()
                    .any(|r| r.view.status != EnrollStatus::Denied && r.view.box_id == b)
        };
        if let Some(w) = want {
            if w > MAX_BOX {
                bail!("box {w} out of range (0-{MAX_BOX})");
            }
            if taken(w) {
                bail!("box {w} is already in use in this cluster");
            }
            return Ok(w);
        }
        (0..=MAX_BOX)
            .find(|b| !taken(*b))
            .ok_or_else(|| anyhow::anyhow!("no free box id left (0-{MAX_BOX})"))
    }

    /// This box holds the CA key.
    pub fn can_sign(&self) -> bool {
        self.signer.is_some()
    }

    /// PEM of the cluster CA, when this box has it.
    pub fn ca_pem(&self) -> Option<&str> {
        self.signer.as_ref().map(|s| s.ca_pem())
    }

    pub async fn list(&self, all: bool) -> Vec<Enrollment> {
        self.recs
            .read()
            .await
            .iter()
            .filter(|r| all || r.view.status == EnrollStatus::Pending)
            .map(|r| r.view.clone())
            .collect()
    }

    /// Queue a request. Re-posting the same key returns the existing
    /// enrolment (so a restarted bootstrap resumes); a new key for a
    /// hostname with a pending request replaces that request.
    #[allow(dead_code)] // the API submits with the caller's address (submit_from)
    pub async fn submit(&self, req: EnrollRequest) -> Result<Enrollment> {
        self.submit_from(req, None).await
    }

    /// `submit`, noting the address the request came from.
    pub async fn submit_from(
        &self,
        req: EnrollRequest,
        from: Option<String>,
    ) -> Result<Enrollment> {
        if req.hostname.trim().is_empty() {
            bail!("hostname must not be empty");
        }
        if req.kind == EnrollKind::Client && !client_name_ok(&req.hostname) {
            bail!("a client name is letters, digits, '-', '.' and '_' (at most 63)");
        }
        let fingerprint = csr_fingerprint(&req.csr)?;
        let mut recs = self.recs.write().await;
        expire(&mut recs);
        if let Some(r) = recs.iter().find(|r| r.view.fingerprint == fingerprint) {
            return Ok(r.view.clone());
        }
        recs.retain(|r| {
            !(r.view.status == EnrollStatus::Pending && r.view.hostname == req.hostname)
        });
        if recs
            .iter()
            .filter(|r| r.view.status == EnrollStatus::Pending)
            .count()
            >= MAX_PENDING
        {
            bail!("too many pending enrolments ({MAX_PENDING}); deny some first");
        }
        // a client is no box: it takes no box id
        let box_id = match req.kind {
            EnrollKind::Box => self.allocate(&recs, &req.hostname, req.box_id)?,
            EnrollKind::Client => 0,
        };
        let view = Enrollment {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            kind: req.kind,
            hostname: req.hostname,
            box_id,
            api_url: req.api_url,
            fingerprint,
            status: EnrollStatus::Pending,
            requested_at: Utc::now(),
            decided_at: None,
            wg_pubkey: req.wg_pubkey,
            from,
            cert: None,
            ca: None,
        };
        recs.push(Record {
            view: view.clone(),
            csr: req.csr,
        });
        self.persist(&recs)?;
        Ok(view)
    }

    pub async fn get(&self, id: &str) -> Option<Enrollment> {
        self.recs
            .read()
            .await
            .iter()
            .find(|r| r.view.id == id)
            .map(|r| r.view.clone())
    }

    /// Approve or deny by id or hostname (pending ones first, then the
    /// host's latest; an exact id always wins).
    #[allow(dead_code)] // the API decides by kind (decide_kind); tests and callers by key
    pub async fn decide(&self, key: &str, d: Decision) -> Result<Enrollment> {
        self.decide_kind(key, d, None).await
    }

    /// `decide`, limited to one kind when `only` is set (the enroll-only
    /// token decides xdj clients and nothing else).
    pub async fn decide_kind(
        &self,
        key: &str,
        d: Decision,
        only: Option<EnrollKind>,
    ) -> Result<Enrollment> {
        let Some(signer) = &self.signer else {
            bail!(
                "this box does not hold the cluster CA key; approve on the box that was bootstrapped first"
            );
        };
        let mut recs = self.recs.write().await;
        if expire(&mut recs) {
            self.persist(&recs)?;
        }
        let idx = recs
            .iter()
            .position(|r| r.view.id == key)
            .or_else(|| {
                recs.iter()
                    .position(|r| r.view.hostname == key && r.view.status == EnrollStatus::Pending)
            })
            // no pending one: the host's latest enrolment, not a stale
            // denial from before it asked again
            .or_else(|| recs.iter().rposition(|r| r.view.hostname == key))
            .ok_or_else(|| anyhow::anyhow!("no enrolment {key:?}"))?;
        let r = &mut recs[idx];
        if only.is_some_and(|k| k != r.view.kind) {
            bail!("no enrolment {key:?} of that kind");
        }
        match d {
            Decision::Approve => {
                if r.view.status == EnrollStatus::Approved {
                    return Ok(r.view.clone());
                }
                if r.view.status == EnrollStatus::Denied {
                    bail!("enrolment {key:?} was denied (or ran out of time): ask again");
                }
                let cert = match r.view.kind {
                    EnrollKind::Box => signer.sign_csr(&r.csr)?,
                    EnrollKind::Client => signer.sign_client_csr(&r.csr)?.0,
                };
                r.view.cert = Some(cert);
                r.view.ca = Some(signer.ca_pem().to_string());
                r.view.status = EnrollStatus::Approved;
            }
            Decision::Deny => {
                // The box has its certificate and runs with its id; there is
                // no revocation, so "denied" would only free that id for the
                // next joiner while this box still uses it.
                if r.view.status == EnrollStatus::Approved && r.view.kind == EnrollKind::Box {
                    bail!(
                        "{} is already approved as box {}: its certificate stays valid and the id stays taken",
                        r.view.hostname,
                        r.view.box_id
                    );
                }
                r.view.cert = None;
                r.view.ca = None;
                r.view.status = EnrollStatus::Denied;
            }
        }
        r.view.decided_at = Some(Utc::now());
        let out = r.view.clone();
        self.persist(&recs)?;
        Ok(out)
    }

    /// The next certificate for a client whose current one is still good:
    /// the CSR must name the same client (`cn`). No approval, no record.
    pub fn renew_client(&self, cn: &str, csr: &str) -> Result<(String, String)> {
        let Some(signer) = &self.signer else {
            bail!(
                "this box does not hold the cluster CA key; renew on the box that was bootstrapped first"
            );
        };
        let (cert, name) = signer.sign_client_csr(csr)?;
        if name != cn {
            bail!("the CSR names {name:?}, not {cn:?}: a client renews only itself");
        }
        Ok((cert, signer.ca_pem().to_string()))
    }

    fn persist(&self, recs: &[Record]) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(recs)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming to {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::api_tls::{self, ApiTlsPaths};

    pub(crate) fn ca_in(dir: &std::path::Path) -> (ApiTlsPaths, CaSigner) {
        let paths = ApiTlsPaths {
            ca_cert: dir.join("ca.pem"),
            ca_key: dir.join("ca-key.pem"),
            server_cert: dir.join("cert.pem"),
            server_key: dir.join("key.pem"),
        };
        api_tls::ensure_api_tls("ca-box", false, &paths).unwrap();
        let signer = CaSigner::load(&paths.ca_cert, &paths.ca_key)
            .unwrap()
            .unwrap();
        (paths, signer)
    }

    fn req(host: &str) -> (EnrollRequest, String) {
        let csr = api_tls::make_csr(host).unwrap();
        (
            EnrollRequest {
                kind: xdebbie_core::EnrollKind::Box,
                hostname: host.into(),
                box_id: None,
                api_url: format!("https://{host}:23443"),
                csr: csr.csr_pem,
                wg_pubkey: None,
            },
            csr.fingerprint,
        )
    }

    fn client_req(name: &str) -> EnrollRequest {
        let csr = api_tls::make_csr(name).unwrap();
        EnrollRequest {
            kind: xdebbie_core::EnrollKind::Client,
            hostname: name.into(),
            box_id: None,
            api_url: String::new(),
            csr: csr.csr_pem,
            wg_pubkey: None,
        }
    }

    fn subject_and_life(pem: &str) -> (Vec<String>, i64, bool) {
        use rustls::pki_types::pem::PemObject;
        let der = rustls::pki_types::CertificateDer::from_pem_slice(pem.as_bytes()).unwrap();
        let (_, x) = x509_parser::parse_x509_certificate(&der).unwrap();
        let orgs = x
            .subject()
            .iter_organization()
            .filter_map(|o| o.as_str().ok().map(str::to_string))
            .collect();
        let life = x.validity().not_after.timestamp() - x.validity().not_before.timestamp();
        let server = x
            .extended_key_usage()
            .ok()
            .flatten()
            .is_some_and(|e| e.value.server_auth);
        (orgs, life, server)
    }

    #[tokio::test]
    async fn a_client_gets_an_hour_long_client_only_cert_and_no_box() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signer) = ca_in(dir.path());
        let e = Enrollments::load(dir.path().join("enroll.json"), Some(signer), vec![0]).unwrap();
        let p = e
            .submit_from(client_req("bmath-laptop"), Some("10.22.9.2".into()))
            .await
            .unwrap();
        assert_eq!(p.kind, xdebbie_core::EnrollKind::Client);
        assert_eq!(p.box_id, 0, "a client takes no box id");
        assert_eq!(p.from.as_deref(), Some("10.22.9.2"));
        // the enroll-only token cannot decide a box, nor a client as one
        let (boxreq, _) = req("joiner");
        let b = e.submit(boxreq).await.unwrap();
        assert!(
            e.decide_kind(
                &b.id,
                Decision::Approve,
                Some(xdebbie_core::EnrollKind::Client)
            )
            .await
            .is_err()
        );
        let ok = e
            .decide_kind(
                &p.id,
                Decision::Approve,
                Some(xdebbie_core::EnrollKind::Client),
            )
            .await
            .unwrap();
        let (orgs, life, server) = subject_and_life(ok.cert.as_deref().unwrap());
        assert_eq!(orgs, vec![api_tls::CLIENT_ORG.to_string()]);
        assert!(!server, "a client cert is client-only");
        assert_eq!(
            life,
            (api_tls::CLIENT_MINUTES + 1) * 60,
            "an hour (from a minute back)"
        );
    }

    #[tokio::test]
    async fn a_client_renews_only_itself() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signer) = ca_in(dir.path());
        let e = Enrollments::load(dir.path().join("enroll.json"), Some(signer), vec![0]).unwrap();
        let mine = api_tls::make_csr("bmath-laptop").unwrap().csr_pem;
        let theirs = api_tls::make_csr("someone-else").unwrap().csr_pem;
        assert!(e.renew_client("bmath-laptop", &mine).is_ok());
        assert!(e.renew_client("bmath-laptop", &theirs).is_err());
    }

    #[tokio::test]
    async fn a_client_request_runs_out_after_fifteen_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signer) = ca_in(dir.path());
        let e = Enrollments::load(dir.path().join("enroll.json"), Some(signer), vec![0]).unwrap();
        let p = e.submit(client_req("late")).await.unwrap();
        {
            let mut recs = e.recs.write().await;
            recs[0].view.requested_at -= chrono::Duration::minutes(CLIENT_PENDING_MINUTES + 1);
        }
        let err = e
            .decide(&p.id, Decision::Approve)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("ran out of time"), "{err}");
        assert!(!client_name_ok("bad name"));
    }

    #[tokio::test]
    async fn submit_approve_persist() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signer) = ca_in(dir.path());
        let path = dir.path().join("state/enroll.json");
        let e = Enrollments::load(path.clone(), Some(signer), vec![0]).unwrap();
        assert!(e.can_sign());

        let (r, fp) = req("joiner");
        let a = e.submit(r.clone()).await.unwrap();
        assert_eq!(a.status, EnrollStatus::Pending);
        assert_eq!(a.fingerprint, fp);
        assert_eq!(a.box_id, 1, "0 is the CA box's");
        // Same key again: same enrolment, not a duplicate.
        assert_eq!(e.submit(r).await.unwrap().id, a.id);
        // New key, same host, still pending: replaces.
        let (r2, _) = req("joiner");
        let b = e.submit(r2).await.unwrap();
        assert_eq!(e.list(false).await.len(), 1);
        assert_ne!(a.id, b.id);

        let approved = e.decide("joiner", Decision::Approve).await.unwrap();
        assert_eq!(approved.status, EnrollStatus::Approved);
        assert!(
            approved
                .cert
                .as_deref()
                .unwrap()
                .contains("BEGIN CERTIFICATE")
        );
        assert!(
            approved
                .ca
                .as_deref()
                .unwrap()
                .contains("BEGIN CERTIFICATE")
        );
        assert!(e.list(false).await.is_empty());
        assert_eq!(e.list(true).await.len(), 1);

        // Survives a reload (without the signer, as a member would).
        let e2 = Enrollments::load(path, None, vec![]).unwrap();
        assert_eq!(e2.get(&b.id).await.unwrap().status, EnrollStatus::Approved);
        assert!(e2.decide(&b.id, Decision::Deny).await.is_err());
        assert!(e.decide("nobody", Decision::Deny).await.is_err());
    }

    #[tokio::test]
    async fn deny_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signer) = ca_in(dir.path());
        let e = Enrollments::load(dir.path().join("enroll.json"), Some(signer), vec![0]).unwrap();
        let (r, _) = req("bad");
        let a = e.submit(r).await.unwrap();
        let d = e.decide(&a.id, Decision::Deny).await.unwrap();
        assert_eq!(d.status, EnrollStatus::Denied);
        assert!(d.cert.is_none());
    }

    #[tokio::test]
    async fn box_ids_are_allocated_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signer) = ca_in(dir.path());
        let e =
            Enrollments::load(dir.path().join("enroll.json"), Some(signer), vec![0, 2]).unwrap();
        // Lowest free, skipping reserved.
        let a = e.submit(req("a").0).await.unwrap();
        assert_eq!(a.box_id, 1);
        let b = e.submit(req("b").0).await.unwrap();
        assert_eq!(b.box_id, 3);
        // A wish is honoured when free, refused when not.
        let mut r = req("c").0;
        r.box_id = Some(7);
        assert_eq!(e.submit(r).await.unwrap().box_id, 7);
        let mut r = req("d").0;
        r.box_id = Some(2);
        assert!(
            e.submit(r)
                .await
                .unwrap_err()
                .to_string()
                .contains("already in use")
        );
        // A denied request frees its id; a re-enrolling approved host keeps its own.
        e.decide("b", Decision::Deny).await.unwrap();
        e.decide("a", Decision::Approve).await.unwrap();
        assert_eq!(e.submit(req("e").0).await.unwrap().box_id, 3);
        assert_eq!(e.submit(req("a").0).await.unwrap().box_id, 1);
    }
}
