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
use xdebbie_core::{EnrollRequest, EnrollStatus, Enrollment};

use crate::bootstrap::api_tls::{CaSigner, csr_fingerprint};

/// Pending requests kept at most; beyond that `submit` refuses, so a
/// misbehaving LAN cannot fill the disk.
const MAX_PENDING: usize = 64;

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
    pub async fn submit(&self, req: EnrollRequest) -> Result<Enrollment> {
        if req.hostname.trim().is_empty() {
            bail!("hostname must not be empty");
        }
        let fingerprint = csr_fingerprint(&req.csr)?;
        let mut recs = self.recs.write().await;
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
        let box_id = self.allocate(&recs, &req.hostname, req.box_id)?;
        let view = Enrollment {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            hostname: req.hostname,
            box_id,
            api_url: req.api_url,
            fingerprint,
            status: EnrollStatus::Pending,
            requested_at: Utc::now(),
            decided_at: None,
            wg_pubkey: req.wg_pubkey,
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

    /// Approve or deny by id or hostname (pending ones first; an exact id
    /// always wins).
    pub async fn decide(&self, key: &str, d: Decision) -> Result<Enrollment> {
        let Some(signer) = &self.signer else {
            bail!(
                "this box does not hold the cluster CA key; approve on the box that was bootstrapped first"
            );
        };
        let mut recs = self.recs.write().await;
        let idx = recs
            .iter()
            .position(|r| r.view.id == key)
            .or_else(|| {
                recs.iter()
                    .position(|r| r.view.hostname == key && r.view.status == EnrollStatus::Pending)
            })
            .or_else(|| recs.iter().position(|r| r.view.hostname == key))
            .ok_or_else(|| anyhow::anyhow!("no enrolment {key:?}"))?;
        let r = &mut recs[idx];
        match d {
            Decision::Approve => {
                if r.view.status == EnrollStatus::Approved {
                    return Ok(r.view.clone());
                }
                let cert = signer.sign_csr(&r.csr)?;
                r.view.cert = Some(cert);
                r.view.ca = Some(signer.ca_pem().to_string());
                r.view.status = EnrollStatus::Approved;
            }
            Decision::Deny => {
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
                hostname: host.into(),
                box_id: None,
                api_url: format!("https://{host}:7443"),
                csr: csr.csr_pem,
                wg_pubkey: None,
            },
            csr.fingerprint,
        )
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
