// SPDX-License-Identifier: GPL-3.0-or-later
//! `xdebbied bootstrap --join` on a box that already has an identity.
//!
//! The package bootstraps every new box as the first box of a cluster of its
//! own: its own CA, its own certificate, its own token. With discovery off
//! nothing later tells it that a cluster already exists, so an operator then
//! runs `bootstrap --join https://<ca-box>:23443` -- and before 0.22 that run
//! saw "certificate and token present", said "Already enrolled" and kept the
//! box in its private cluster for ever.
//!
//! What a box's identity is worth decides what `--join` may do with it:
//!
//! - **Its own cluster, still empty** (it holds the CA key, and no VM, no
//!   volume, no other host, no approved box, no configured mesh peer): that
//!   cluster is only the package's placeholder. Its identity files are moved
//!   aside (renamed, never deleted) and the box enrols with the named CA.
//! - **Its own cluster, in use**: never thrown away behind the operator's
//!   back. `--join` refuses and says what is in it; `--force` is the
//!   explicit way through.
//! - **Already enrolled with that CA** (`join.json` names it): reused, as
//!   before -- re-running a join is harmless.
//! - **Enrolled with a different CA**: refused; `--force` re-enrols.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use xdebbie_core::{EnrollKind, EnrollStatus, Enrollment};

use super::api_tls::ApiTlsPaths;
use super::join::JoinState;

/// What is in this box's own cluster (it holds the CA key).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OwnCluster {
    pub vms: usize,
    pub volumes: usize,
    /// Hosts in the state file other than this one.
    pub other_hosts: Vec<String>,
    /// Boxes this CA has approved.
    pub boxes: usize,
    /// `[[mesh.peers]]` in this box's config.
    pub peers: usize,
}

impl OwnCluster {
    pub fn is_empty(&self) -> bool {
        self.vms == 0
            && self.volumes == 0
            && self.other_hosts.is_empty()
            && self.boxes == 0
            && self.peers == 0
    }

    pub fn describe(&self) -> String {
        let mut parts = vec![];
        let n =
            |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
        if self.vms > 0 {
            parts.push(n(self.vms, "VM", "VMs"));
        }
        if self.volumes > 0 {
            parts.push(n(self.volumes, "volume", "volumes"));
        }
        if !self.other_hosts.is_empty() {
            parts.push(format!(
                "{} ({})",
                n(self.other_hosts.len(), "other host", "other hosts"),
                self.other_hosts.join(", ")
            ));
        }
        if self.boxes > 0 {
            parts.push(n(self.boxes, "approved box", "approved boxes"));
        }
        if self.peers > 0 {
            parts.push(n(self.peers, "mesh peer", "mesh peers"));
        }
        if parts.is_empty() {
            "nothing".into()
        } else {
            parts.join(", ")
        }
    }
}

/// Read what this box's own cluster holds: the state file (VMs, volumes,
/// hosts), `<state_dir>/enroll.json` (approved boxes) and the number of
/// configured mesh peers. `own_hosts`: every name this box goes by (its
/// hostname, its config's `[[hosts]]` names) -- those are not other hosts. A file that is missing counts as empty; one that
/// cannot be parsed is an error -- never guess that a cluster is empty.
pub fn survey(
    state_file: &Path,
    state_dir: &Path,
    peers: usize,
    own_hosts: &[String],
) -> Result<OwnCluster> {
    let mut own = OwnCluster {
        peers,
        ..Default::default()
    };
    match std::fs::read(state_file) {
        Ok(b) => {
            let st: crate::store::State = serde_json::from_slice(&b).with_context(|| {
                format!(
                    "parsing {} (cannot tell whether this box's cluster is in use)",
                    state_file.display()
                )
            })?;
            own.vms = st.vms.len();
            own.volumes = st.volumes.len();
            own.other_hosts = st
                .hosts
                .keys()
                .filter(|h| !own_hosts.contains(h))
                .cloned()
                .collect();
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {}", state_file.display())),
    }
    let enroll = state_dir.join("enroll.json");
    match std::fs::read(&enroll) {
        Ok(b) => {
            let recs: Vec<Enrollment> = serde_json::from_slice(&b).with_context(|| {
                format!(
                    "parsing {} (cannot tell whether this box's cluster is in use)",
                    enroll.display()
                )
            })?;
            own.boxes = recs
                .iter()
                .filter(|r| r.status == EnrollStatus::Approved && r.kind == EnrollKind::Box)
                .count();
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {}", enroll.display())),
    }
    Ok(own)
}

/// What `bootstrap --join` does with the identity it finds.
#[derive(Debug, PartialEq, Eq)]
pub enum JoinStart {
    /// No identity (or `--force`): enrol.
    Enrol,
    /// Enrolled with this CA already: keep it.
    Reuse,
    /// The box's own, empty cluster: move its identity aside, then enrol.
    TakeOverOwn(OwnCluster),
}

/// Decide; `own` is only asked when the box holds its own CA key.
pub fn decide(
    enrolled: bool,
    force: bool,
    is_ca: bool,
    prior: Option<&JoinState>,
    ca_url: &str,
    own: impl FnOnce() -> Result<OwnCluster>,
) -> Result<JoinStart> {
    if !enrolled || force {
        return Ok(JoinStart::Enrol);
    }
    let ca_url = ca_url.trim_end_matches('/');
    if is_ca {
        let own = own()?;
        if own.is_empty() {
            return Ok(JoinStart::TakeOverOwn(own));
        }
        bail!(
            "this box is the CA of a cluster of its own, and that cluster is in use ({}); \
             joining {ca_url} would leave it behind. Move or remove what is in it first, or \
             re-run with --force to join anyway (its CA and certificates are then replaced)",
            own.describe()
        );
    }
    match prior {
        Some(p) if p.ca_url.trim_end_matches('/') == ca_url => Ok(JoinStart::Reuse),
        Some(p) => bail!(
            "this box is enrolled with {} already, not {ca_url}; re-run with --force to enrol with {ca_url} instead",
            p.ca_url
        ),
        // Enrolled before join.json existed: keep what is there, as before.
        None => Ok(JoinStart::Reuse),
    }
}

/// Rename the box's own identity files out of the way (`<file>.own-cluster-<stamp>`),
/// so the join starts with a new key. Returns where each went.
pub fn set_aside(paths: &ApiTlsPaths, token_path: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let stamp = chrono::Local::now().format("%Y%m%d%H%M%S");
    let mut moved = vec![];
    for p in [
        &paths.ca_key,
        &paths.ca_cert,
        &paths.server_cert,
        &paths.server_key,
        &token_path.to_path_buf(),
    ] {
        if !p.exists() {
            continue;
        }
        let mut to = p.as_os_str().to_owned();
        to.push(format!(".own-cluster-{stamp}"));
        let to = PathBuf::from(to);
        std::fs::rename(p, &to)
            .with_context(|| format!("moving {} to {}", p.display(), to.display()))?;
        moved.push((p.clone(), to));
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prior(url: &str) -> JoinState {
        JoinState {
            ca_url: url.into(),
            ca_fingerprint: "sha256:aa".into(),
            enrollment_id: "e1".into(),
            key_fingerprint: "sha256:bb".into(),
        }
    }

    fn me() -> Vec<String> {
        vec!["me".into()]
    }

    fn empty() -> Result<OwnCluster> {
        Ok(OwnCluster::default())
    }

    #[test]
    fn fresh_box_or_force_enrols() {
        assert_eq!(
            decide(false, false, false, None, "https://ca:23443", empty).unwrap(),
            JoinStart::Enrol
        );
        assert_eq!(
            decide(true, true, true, None, "https://ca:23443", || bail!(
                "not asked"
            ))
            .unwrap(),
            JoinStart::Enrol
        );
    }

    /// The bug: the package made the box a cluster of its own; `--join`
    /// said "Already enrolled" and never enrolled. Now it takes over.
    #[test]
    fn own_empty_cluster_is_taken_over() {
        let d = decide(true, false, true, None, "https://ca:23443", empty).unwrap();
        assert_eq!(d, JoinStart::TakeOverOwn(OwnCluster::default()));
    }

    #[test]
    fn own_cluster_in_use_is_never_left_behind() {
        let busy = || {
            Ok(OwnCluster {
                vms: 2,
                volumes: 1,
                ..Default::default()
            })
        };
        let e = decide(true, false, true, None, "https://ca:23443", busy)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("2 VMs, 1 volume") && e.contains("--force"),
            "{e}"
        );
    }

    #[test]
    fn same_ca_reused_other_ca_refused() {
        let p = prior("https://ca:23443/");
        assert_eq!(
            decide(true, false, false, Some(&p), "https://ca:23443", empty).unwrap(),
            JoinStart::Reuse
        );
        let e = decide(true, false, false, Some(&p), "https://other:23443", empty)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("enrolled with https://ca:23443/ already") && e.contains("--force"),
            "{e}"
        );
        assert_eq!(
            decide(true, false, false, None, "https://ca:23443", empty).unwrap(),
            JoinStart::Reuse
        );
    }

    #[test]
    fn survey_counts_what_is_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state.json");
        // missing files: empty
        assert!(survey(&state, dir.path(), 0, &me()).unwrap().is_empty());
        std::fs::write(
            &state,
            r#"{"hosts":{"me":{"name":"me","uri":"qemu:///system"}},"vms":{}}"#,
        )
        .unwrap();
        assert!(
            survey(&state, dir.path(), 0, &me()).unwrap().is_empty(),
            "its own host does not count"
        );
        assert_eq!(survey(&state, dir.path(), 1, &me()).unwrap().peers, 1);
        std::fs::write(&state, "{not json").unwrap();
        assert!(
            survey(&state, dir.path(), 0, &me()).is_err(),
            "unreadable state is never taken as empty"
        );
    }

    #[test]
    fn survey_counts_approved_boxes_only() {
        let dir = tempfile::tempdir().unwrap();
        let rec = |id: &str, status: &str| {
            format!(
                r#"{{"id":"{id}","hostname":"h{id}","box":3,"api_url":"https://h:23443","fingerprint":"sha256:00","status":"{status}","requested_at":"2026-11-24T00:00:00Z","csr":"x"}}"#
            )
        };
        std::fs::write(
            dir.path().join("enroll.json"),
            format!("[{},{}]", rec("a", "approved"), rec("b", "denied")),
        )
        .unwrap();
        let own = survey(&dir.path().join("state.json"), dir.path(), 0, &me()).unwrap();
        assert_eq!(own.boxes, 1);
        assert_eq!(own.describe(), "1 approved box");
    }

    #[test]
    fn set_aside_renames_never_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let f = |n: &str| dir.path().join(n);
        let paths = ApiTlsPaths {
            ca_cert: f("ca.crt"),
            ca_key: f("ca.key"),
            server_cert: f("api.crt"),
            server_key: f("api.key"),
        };
        for n in ["ca.crt", "ca.key", "api.crt", "api.key", "token"] {
            std::fs::write(f(n), n).unwrap();
        }
        let moved = set_aside(&paths, &f("token")).unwrap();
        assert_eq!(moved.len(), 5);
        for (from, to) in moved {
            assert!(!from.exists());
            assert_eq!(
                std::fs::read_to_string(&to).unwrap(),
                from.file_name().unwrap().to_str().unwrap()
            );
        }
        assert!(!paths.all_exist() && !paths.is_ca());
    }
}
