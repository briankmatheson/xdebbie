// SPDX-License-Identifier: GPL-3.0-or-later
//! The enrolment queue (`xdctl enroll ls`, `xdctl approve HOST`,
//! `xdctl enroll deny ID`): unknown keys wait, approval signs, a new key
//! for a known host is never accepted on its own, denial sticks.

use xdebbie_core::{EnrollStatus, Enrollment};

use super::support::{CaBox, TOKEN, box_req, queue};
use crate::enroll::{Decision, Enrollments};

/// (issuer CN, subject CN, DNS SANs, server+client EKU, signature checks
/// out against `ca_pem`).
fn inspect(cert_pem: &str, ca_pem: &str) -> (String, String, Vec<String>, bool, bool) {
    let (_, c) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).unwrap();
    let (_, x) = x509_parser::parse_x509_certificate(&c.contents).unwrap();
    let (_, ca) = x509_parser::pem::parse_x509_pem(ca_pem.as_bytes()).unwrap();
    let (_, cax) = x509_parser::parse_x509_certificate(&ca.contents).unwrap();
    let cn = |n: &x509_parser::x509::X509Name| {
        n.iter_common_name()
            .next()
            .and_then(|c| c.as_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let sans = x
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|s| {
            s.value
                .general_names
                .iter()
                .filter_map(|g| match g {
                    x509_parser::extensions::GeneralName::DNSName(d) => Some(d.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let eku = x
        .extended_key_usage()
        .ok()
        .flatten()
        .is_some_and(|e| e.value.server_auth && e.value.client_auth);
    let signed = x.verify_signature(Some(cax.public_key())).is_ok();
    (cn(x.issuer()), cn(x.subject()), sans, eku, signed)
}

#[tokio::test]
async fn an_unknown_key_waits_with_nothing_issued() {
    let ca = CaBox::start().await;
    let (r, csr) = box_req("joiner", None);
    let resp = ca
        .anon()
        .post(format!("{}/v1/enroll", ca.url))
        .json(&r)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let e: Enrollment = resp.json().await.unwrap();
    assert_eq!(e.status, EnrollStatus::Pending);
    assert_eq!(e.fingerprint, csr.fingerprint);
    assert!(e.cert.is_none() && e.ca.is_none());
    assert_eq!(
        e.from.as_deref(),
        Some("127.0.0.1"),
        "who asked is recorded"
    );
    // polling it, anonymously, still shows nothing issued
    for _ in 0..3 {
        let p: Enrollment = ca
            .anon()
            .get(format!("{}/v1/enroll/{}", ca.url, e.id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(p.status, EnrollStatus::Pending);
        assert!(p.cert.is_none());
    }
    // `xdctl enroll ls` (token) lists it; anonymous callers cannot
    let ls: Vec<Enrollment> = ca
        .anon()
        .get(format!("{}/v1/enroll", ca.url))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ls.len(), 1);
    assert_eq!(ls[0].hostname, "joiner");
    // nor approve it
    let r = ca
        .anon()
        .post(format!("{}/v1/enroll/joiner/approve", ca.url))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(
        ca.enroll.get(&e.id).await.unwrap().status,
        EnrollStatus::Pending
    );
}

#[tokio::test]
async fn approval_signs_the_joiners_own_key_with_the_cluster_ca() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    let ca_pem = e.ca_pem().unwrap().to_string();
    let (r, csr) = box_req("joiner.example", None);
    e.submit(r).await.unwrap();
    let ok = e.decide("joiner.example", Decision::Approve).await.unwrap();
    assert_eq!(ok.status, EnrollStatus::Approved);
    assert!(ok.decided_at.is_some());
    assert_eq!(ok.ca.as_deref(), Some(ca_pem.as_str()));
    let cert = ok.cert.unwrap();
    let (issuer, subject, sans, eku, signed) = inspect(&cert, &ca_pem);
    assert!(signed, "signed by the cluster CA");
    assert_eq!(subject, "joiner.example");
    assert_ne!(issuer, subject);
    assert!(sans.iter().any(|s| s == "joiner.example"), "{sans:?}");
    assert!(eku, "a member cert serves and identifies");
    // the certificate is for the key the joiner holds: rustls accepts the pair
    reqwest::Identity::from_pem(format!("{cert}\n{}", csr.key_pem).as_bytes()).unwrap();
    // approving again is a no-op answering the same certificate
    let again = e.decide("joiner.example", Decision::Approve).await.unwrap();
    assert_eq!(again.cert.as_deref(), Some(cert.as_str()));
}

/// The host is known and approved; a request arrives with the same hostname
/// and a different key (a reinstall — or someone else). It waits like any
/// unknown key, showing the new fingerprint; the old approval is untouched.
#[tokio::test]
async fn a_new_key_for_an_approved_hostname_waits_for_approval() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    let (r1, k1) = box_req("gram", None);
    let first = e.submit(r1).await.unwrap();
    e.decide("gram", Decision::Approve).await.unwrap();

    let (r2, k2) = box_req("gram", None);
    assert_ne!(k1.fingerprint, k2.fingerprint);
    let second = e.submit(r2).await.unwrap();
    assert_ne!(second.id, first.id);
    assert_eq!(
        second.status,
        EnrollStatus::Pending,
        "not silently approved"
    );
    assert!(second.cert.is_none());
    assert_eq!(second.fingerprint, k2.fingerprint);
    let pending = e.list(false).await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].fingerprint, k2.fingerprint, "enroll ls shows it");
    let old = e.get(&first.id).await.unwrap();
    assert_eq!(old.status, EnrollStatus::Approved);
    assert_eq!(old.fingerprint, k1.fingerprint);

    // denying the newcomer leaves the approved box alone
    e.decide(&second.id, Decision::Deny).await.unwrap();
    assert_eq!(
        e.get(&first.id).await.unwrap().status,
        EnrollStatus::Approved
    );
}

/// A second key for a hostname that is still *pending* replaces the first
/// request (docs/api.md). The first joiner then finds its enrolment gone
/// and stops ("disappeared ... re-run with --force") instead of being
/// handed a certificate for someone else's key.
#[tokio::test]
async fn a_new_key_for_a_pending_hostname_replaces_it_and_the_first_joiner_is_told() {
    let ca = CaBox::start().await;
    let (r1, _) = box_req("joiner", None);
    let first: Enrollment = ca
        .anon()
        .post(format!("{}/v1/enroll", ca.url))
        .json(&r1)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let (r2, k2) = box_req("joiner", None);
    let second: Enrollment = ca
        .anon()
        .post(format!("{}/v1/enroll", ca.url))
        .json(&r2)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second.box_id, first.box_id);
    let r = ca
        .anon()
        .get(format!("{}/v1/enroll/{}", ca.url, first.id))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let ok = ca.enroll.decide("joiner", Decision::Approve).await.unwrap();
    assert_eq!((ok.id, ok.fingerprint), (second.id, k2.fingerprint));
}

/// The CA signs the CN and SANs in the CSR, and the cluster identifies a
/// member by that CN. A request whose CSR names another host than the one
/// the operator is shown (and approves) is refused up front.
#[ignore = "bug: the CA signs the CSR CN even when it names another host than the request"]
#[tokio::test]
async fn a_csr_naming_another_host_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    let (mut r, _) = box_req("ca-box", None);
    r.hostname = "innocent-newbox".into();
    let err = e.submit(r).await.unwrap_err().to_string();
    assert!(err.contains("ca-box"), "{err}");
    assert!(e.list(true).await.is_empty());
}

#[tokio::test]
async fn deny_is_final_for_that_key() {
    let ca = CaBox::start().await;
    let (r, _) = box_req("bad", None);
    let p = ca.enroll.submit(r.clone()).await.unwrap();
    let resp = ca
        .anon()
        .post(format!("{}/v1/enroll/{}/deny", ca.url, p.id))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let d: Enrollment = resp.json().await.unwrap();
    assert_eq!(d.status, EnrollStatus::Denied);
    assert!(d.cert.is_none() && d.ca.is_none());
    // approving afterwards is refused
    let resp = ca
        .anon()
        .post(format!("{}/v1/enroll/bad/approve", ca.url))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert!(resp.text().await.unwrap().contains("ask again"));
    // the same key asking again gets the denial back, not a new request
    let again = ca.enroll.submit(r).await.unwrap();
    assert_eq!((again.id, again.status), (p.id, EnrollStatus::Denied));
    assert!(ca.enroll.list(false).await.is_empty());
    // `bootstrap --join --force` (new key) asks again, and that can be approved
    let fresh = ca.enroll.submit(box_req("bad", None).0).await.unwrap();
    assert_eq!(fresh.status, EnrollStatus::Pending);
    let ok = ca.enroll.decide("bad", Decision::Approve).await.unwrap();
    assert_eq!(ok.id, fresh.id);
}

/// Denied once, re-asked with a new key and approved: `xdctl approve HOST`
/// run again later answers the approved enrolment, not the old denial.
#[ignore = "bug: approve by hostname falls back to the oldest record (a stale denial)"]
#[tokio::test]
async fn approve_by_hostname_answers_the_latest_enrolment() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    e.submit(box_req("h", None).0).await.unwrap();
    e.decide("h", Decision::Deny).await.unwrap();
    let second = e.submit(box_req("h", None).0).await.unwrap();
    e.decide("h", Decision::Approve).await.unwrap();
    let again = e.decide("h", Decision::Approve).await.unwrap();
    assert_eq!(
        (again.id, again.status),
        (second.id, EnrollStatus::Approved)
    );
}

#[tokio::test]
async fn unknown_names_and_member_boxes_cannot_decide() {
    let ca = CaBox::start().await;
    let resp = ca
        .anon()
        .post(format!("{}/v1/enroll/nobody/approve", ca.url))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    // a box without the CA key holds the queue read-only
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    e.submit(box_req("x", None).0).await.unwrap();
    let member = Enrollments::load(dir.path().join("enroll.json"), None, vec![]).unwrap();
    assert!(!member.can_sign());
    let err = member
        .decide("x", Decision::Approve)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not hold the cluster CA key"), "{err}");
}

#[tokio::test]
async fn the_pending_queue_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    for i in 0..64 {
        e.submit(box_req(&format!("q{i}"), None).0).await.unwrap();
    }
    let err = e
        .submit(box_req("one-too-many", None).0)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("too many pending"), "{err}");
    // deciding one makes room
    e.decide("q0", Decision::Deny).await.unwrap();
    e.submit(box_req("one-too-many", None).0).await.unwrap();
}

#[tokio::test]
async fn an_empty_hostname_or_a_garbage_csr_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    let (mut r, _) = box_req("x", None);
    r.hostname = "  ".into();
    assert!(e.submit(r).await.is_err());
    let (mut r, _) = box_req("x", None);
    r.csr = "-----BEGIN CERTIFICATE REQUEST-----\nnope\n-----END CERTIFICATE REQUEST-----\n".into();
    assert!(e.submit(r).await.is_err());
    assert!(e.list(true).await.is_empty());
}
