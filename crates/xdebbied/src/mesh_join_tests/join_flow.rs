// SPDX-License-Identifier: GPL-3.0-or-later
//! `xdebbied bootstrap --join URL` against a CA box in-process: pin the CA,
//! enrol, wait for approval, install, fetch the cluster token over mTLS.

use std::sync::Arc;

use axum::{Json, Router, routing::get, routing::post};
use xdebbie_core::{EnrollKind, EnrollRequest, EnrollStatus, Enrollment, TokenSet};

use super::support::{CLUSTER_NETWORK, CaBox, Joiner, TOKEN, approve_when_pending, box_req};
use crate::{
    bootstrap::{
        api_tls,
        join::{self, JoinOptions, Joined},
    },
    enroll::Decision,
};

/// Run `join` (blocking HTTP) off the async runtime.
async fn run_join(
    url: &str,
    fp: Option<&str>,
    host: &str,
    want: Option<u8>,
    force: bool,
    j: &Joiner,
) -> anyhow::Result<Joined> {
    let (url, fp, host) = (url.to_string(), fp.map(str::to_string), host.to_string());
    let (paths, state_dir) = (super::support::paths_in(j.dir.path()), j.state_dir.clone());
    tokio::task::spawn_blocking(move || {
        join::join(&JoinOptions {
            ca_url: &url,
            expect_fingerprint: fp.as_deref(),
            hostname: &host,
            box_id: want,
            api_url: &format!("https://{host}:23443"),
            force,
            paths: &paths,
            state_dir: &state_dir,
        })
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn join_waits_for_approval_then_installs_and_gets_the_token() {
    let ca = CaBox::start_with(Some(CLUSTER_NETWORK)).await;
    let j = Joiner::new();
    let approver = tokio::spawn(approve_when_pending(ca.enroll.clone(), "joiner".into()));
    let joined = run_join(&ca.url, Some(&ca.ca_fingerprint), "joiner", None, false, &j)
        .await
        .unwrap();
    approver.await.unwrap();
    assert_eq!(joined.tokens.current, TOKEN);
    assert_eq!(joined.box_id, 1, "the CA box is 0");
    // installed: the cluster CA and a cert for the key that never left
    assert_eq!(
        std::fs::read_to_string(&j.paths.ca_cert).unwrap(),
        ca.ca_pem
    );
    let cert = std::fs::read_to_string(&j.paths.server_cert).unwrap();
    let key = std::fs::read_to_string(&j.paths.server_key).unwrap();
    let approved = ca.enroll.list(true).await;
    assert_eq!(approved.len(), 1);
    assert_eq!(approved[0].cert.as_deref(), Some(cert.as_str()));
    reqwest::Identity::from_pem(format!("{cert}\n{key}").as_bytes()).unwrap();
    assert!(!j.paths.ca_key.exists(), "a joiner never holds the CA key");
    // where a restart resumes from
    let st: join::JoinState =
        serde_json::from_slice(&std::fs::read(j.state_dir.join("join.json")).unwrap()).unwrap();
    assert_eq!(st.enrollment_id, approved[0].id);
    assert_eq!(st.ca_fingerprint, ca.ca_fingerprint);

    // then it copies the cluster's overlay over the same mTLS identity
    let (url, paths) = (ca.url.clone(), super::support::paths_in(j.dir.path()));
    let net = tokio::task::spawn_blocking(move || join::fetch_network(&url, &paths))
        .await
        .unwrap()
        .unwrap()
        .expect("the CA box has a [network]");
    assert_eq!(net.cidr.to_string(), "10.23.0.0/16");
    assert_eq!(net.ntp.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_join_resumes_with_the_same_box_and_no_second_request() {
    let ca = CaBox::start().await;
    let j = Joiner::new();
    let approver = tokio::spawn(approve_when_pending(ca.enroll.clone(), "joiner".into()));
    let first = run_join(&ca.url, Some(&ca.ca_fingerprint), "joiner", None, false, &j)
        .await
        .unwrap();
    approver.await.unwrap();
    // Re-run without --ca-fingerprint and without a terminal: the CA pinned
    // last time is trusted again; the approved enrolment is picked up.
    let again = run_join(&ca.url, None, "joiner", None, false, &j)
        .await
        .unwrap();
    assert_eq!(again.box_id, first.box_id);
    assert_eq!(again.tokens.current, TOKEN);
    assert_eq!(ca.enroll.list(true).await.len(), 1, "no second request");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ca_fingerprint_mismatch_is_refused_before_anything_is_sent() {
    let ca = CaBox::start().await;
    let j = Joiner::new();
    let wrong = format!("sha256:{}", "00".repeat(32));
    let err = run_join(&ca.url, Some(&wrong), "joiner", None, false, &j)
        .await
        .err()
        .expect("must refuse")
        .to_string();
    assert!(err.contains("not joining"), "{err}");
    assert!(err.contains(&ca.ca_fingerprint), "shows what it got: {err}");
    assert!(!j.paths.server_key.exists(), "no key generated");
    assert!(!j.state_dir.join("join.json").exists());
    assert!(ca.enroll.list(true).await.is_empty(), "nothing queued");

    // the pin is compared without the prefix and case-insensitively
    let approver = tokio::spawn(approve_when_pending(ca.enroll.clone(), "joiner".into()));
    let bare = ca
        .ca_fingerprint
        .trim_start_matches("sha256:")
        .to_ascii_uppercase();
    run_join(&ca.url, Some(&bare), "joiner", None, false, &j)
        .await
        .unwrap();
    approver.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pin_from_another_ca_url_is_not_reused() {
    let ca = CaBox::start().await;
    let other = CaBox::start().await;
    let j = Joiner::new();
    let approver = tokio::spawn(approve_when_pending(ca.enroll.clone(), "joiner".into()));
    run_join(&ca.url, Some(&ca.ca_fingerprint), "joiner", None, false, &j)
        .await
        .unwrap();
    approver.await.unwrap();
    // another cluster: its CA was never accepted, and without a terminal
    // nobody can accept it now
    if unsafe { libc::isatty(0) } == 0 {
        let err = run_join(&other.url, None, "joiner", None, false, &j)
            .await
            .err()
            .expect("must refuse")
            .to_string();
        assert!(err.contains("not yet trusted"), "{err}");
    }
    assert!(other.enroll.list(true).await.is_empty());
}

#[tokio::test]
async fn plain_http_is_refused() {
    let j = Joiner::new();
    let err = run_join("http://127.0.0.1:9", None, "joiner", None, false, &j)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("https://"), "{err}");
    assert!(!j.paths.server_key.exists());
}

/// A key on disk with nothing to resume (state lost, or denied) is never
/// silently replaced: the CA may already have a cert for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_existing_key_is_not_rekeyed_without_force() {
    let ca = CaBox::start().await;
    let j = Joiner::new();
    std::fs::write(&j.paths.server_key, "old key").unwrap();
    let err = run_join(&ca.url, Some(&ca.ca_fingerprint), "joiner", None, false, &j)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("--force"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&j.paths.server_key).unwrap(),
        "old key"
    );
    assert!(ca.enroll.list(true).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_denied_join_stops_and_force_asks_again() {
    let ca = CaBox::start().await;
    let j = Joiner::new();
    let q = ca.enroll.clone();
    let denier = tokio::spawn(async move {
        loop {
            if !q.list(false).await.is_empty() {
                q.decide("joiner", Decision::Deny).await.unwrap();
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    });
    let err = run_join(&ca.url, Some(&ca.ca_fingerprint), "joiner", None, false, &j)
        .await
        .err()
        .unwrap()
        .to_string();
    denier.await.unwrap();
    assert!(err.contains("denied"), "{err}");
    assert!(!j.paths.server_cert.exists(), "nothing installed");
    // without --force: refused (the key is there, the enrolment denied)
    let err = run_join(&ca.url, Some(&ca.ca_fingerprint), "joiner", None, false, &j)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("--force"), "{err}");
    // with --force: a new key, a new request, which can be approved
    let approver = tokio::spawn(approve_when_pending(ca.enroll.clone(), "joiner".into()));
    let ok = run_join(&ca.url, Some(&ca.ca_fingerprint), "joiner", None, true, &j)
        .await
        .unwrap();
    approver.await.unwrap();
    assert_eq!(ok.tokens.current, TOKEN);
}

/// The cluster token is only ever handed to a box certificate the cluster CA
/// signed: not anonymously, not for the bearer token, not for an xdj client
/// certificate, not for a certificate from another CA.
#[tokio::test]
async fn the_cluster_token_goes_only_to_an_approved_box_certificate() {
    let ca = CaBox::start().await;
    let token_url = format!("{}/v1/enroll/token", ca.url);
    let status = |r: Result<reqwest::Response, reqwest::Error>| r.map(|r| r.status().as_u16());

    // pending: the joiner has a key but no certificate yet
    let (r, csr) = box_req("joiner", None);
    let p = ca.enroll.submit(r).await.unwrap();
    assert_eq!(status(ca.anon().get(&token_url).send().await).unwrap(), 401);
    assert_eq!(
        status(ca.anon().get(&token_url).bearer_auth(TOKEN).send().await).unwrap(),
        403
    );

    // a certificate for the same name from another CA gets nowhere
    let rogue_dir = tempfile::tempdir().unwrap();
    let (_, rogue) = super::support::ca_in(rogue_dir.path());
    let forged_csr = api_tls::make_csr("joiner").unwrap();
    let forged_cert = rogue.sign_csr(&forged_csr.csr_pem).unwrap();
    let s = status(
        ca.with_cert(&forged_cert, &forged_csr.key_pem)
            .get(&token_url)
            .send()
            .await,
    );
    assert!(
        !matches!(s, Ok(200)),
        "a foreign CA's certificate got the token: {s:?}"
    );

    // an approved xdj client is not a box
    let client_csr = api_tls::make_csr("laptop").unwrap();
    let c = ca
        .enroll
        .submit(EnrollRequest {
            kind: EnrollKind::Client,
            hostname: "laptop".into(),
            box_id: None,
            api_url: String::new(),
            csr: client_csr.csr_pem.clone(),
            wg_pubkey: None,
        })
        .await
        .unwrap();
    let c = ca.enroll.decide(&c.id, Decision::Approve).await.unwrap();
    assert_eq!(
        status(
            ca.with_cert(c.cert.as_deref().unwrap(), &client_csr.key_pem)
                .get(&token_url)
                .send()
                .await
        )
        .unwrap(),
        403
    );

    // the approved box
    let ok = ca.enroll.decide(&p.id, Decision::Approve).await.unwrap();
    let set: TokenSet = ca
        .with_cert(ok.cert.as_deref().unwrap(), &csr.key_pem)
        .get(&token_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(set.current, TOKEN);
}

/// A CA box under the attacker's control for one step: serves CA `a` (which
/// the joiner pinned) but answers the enrolment on its own terms.
async fn lying_ca(
    reply: impl Fn(EnrollRequest) -> Enrollment + Send + Sync + 'static,
) -> (
    String,
    String,
    tempfile::TempDir,
    axum_server::Handle<std::net::SocketAddr>,
) {
    let dir = tempfile::tempdir().unwrap();
    let paths = api_tls::generate_localhost(dir.path());
    let ca_pem = std::fs::read_to_string(&paths.ca_cert).unwrap();
    let fp = api_tls::cert_fingerprint(&ca_pem).unwrap();
    let reply = Arc::new(reply);
    let app = Router::new()
        .route("/v1/ca", get(move || async move { ca_pem.clone() }))
        .route(
            "/v1/enroll",
            post(move |Json(req): Json<EnrollRequest>| {
                let reply = reply.clone();
                async move { (axum::http::StatusCode::ACCEPTED, Json(reply(req))) }
            }),
        );
    let roots = crate::tls::client_roots(&paths.ca_cert, &dir.path().join("none"));
    let sc = crate::tls::server_config(&paths.server_cert, &paths.server_key, roots).unwrap();
    let handle: axum_server::Handle<std::net::SocketAddr> = axum_server::Handle::new();
    let h = handle.clone();
    tokio::spawn(async move {
        axum_server::bind("127.0.0.1:0".parse().unwrap())
            .acceptor(crate::tls::PeerCertAcceptor::new(sc))
            .handle(h)
            .serve(app.into_make_service())
            .await
            .unwrap();
    });
    let addr = handle.listening().await.unwrap();
    (
        format!("https://127.0.0.1:{}", addr.port()),
        fp,
        dir,
        handle,
    )
}

fn enrollment(req: &EnrollRequest, fingerprint: String) -> Enrollment {
    Enrollment {
        id: "fake0001".into(),
        kind: EnrollKind::Box,
        hostname: req.hostname.clone(),
        box_id: 1,
        api_url: req.api_url.clone(),
        fingerprint,
        status: EnrollStatus::Approved,
        requested_at: chrono::Utc::now(),
        decided_at: Some(chrono::Utc::now()),
        wg_pubkey: None,
        from: None,
        cert: None,
        ca: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_approval_carrying_a_different_ca_is_not_installed() {
    let other_dir = tempfile::tempdir().unwrap();
    let (_, other) = super::support::ca_in(other_dir.path());
    let other = Arc::new(other);
    let (url, fp, _d, h) = lying_ca(move |req| {
        let mut e = enrollment(&req, api_tls::csr_fingerprint(&req.csr).unwrap());
        e.cert = Some(other.sign_csr(&req.csr).unwrap());
        e.ca = Some(other.ca_pem().to_string());
        e
    })
    .await;
    let j = Joiner::new();
    let err = run_join(&url, Some(&fp), "joiner", None, false, &j)
        .await
        .err()
        .unwrap()
        .to_string();
    h.shutdown();
    assert!(err.contains("differs from the one pinned"), "{err}");
    assert!(!j.paths.server_cert.exists() && !j.paths.ca_cert.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_enrolment_echoing_another_key_is_not_continued() {
    let (url, fp, _d, h) = lying_ca(|req| {
        let mut e = enrollment(&req, format!("sha256:{}", "ab".repeat(32)));
        e.status = EnrollStatus::Pending;
        e
    })
    .await;
    let j = Joiner::new();
    let err = run_join(&url, Some(&fp), "joiner", None, false, &j)
        .await
        .err()
        .unwrap()
        .to_string();
    h.shutdown();
    assert!(err.contains("echoed key fingerprint"), "{err}");
    assert!(!j.state_dir.join("join.json").exists());
}
