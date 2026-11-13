// SPDX-License-Identifier: GPL-3.0-or-later
//! REST API. Handlers only mutate desired state; all hypervisor I/O happens
//! in the reconciler. Every write is idempotent so controllers can retry.
//! (One exception: creating/deleting a standalone volume talks to ABE
//! inline, because the caller needs the id ABE assigns.)
//!
//! Only the elected leader answers cluster routes (hosts, VMs, volumes,
//! scheduling); a follower forwards them to the leader verbatim and relays
//! the reply (`forward_to_leader`). `/v1/leader`, `/v1/network`, `/v1/token`
//! and `/v1/mesh*` describe the box you asked, so they are served locally.
//! `/v1/enroll*` belongs to the box holding the cluster CA key and is
//! forwarded there (`forward_to_ca`).
//!
//! Authentication (`authenticate`): a valid bearer token, or a client
//! certificate signed by the cluster CA (another xdebbied). Submitting and
//! polling an enrolment needs neither, that is how a box gets its cert.

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Json, Router,
    body::Body,
    extract::{OriginalUri, Path, Query, Request, State},
    http::{HeaderName, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use xdebbie_core::*;

use crate::{
    abe,
    bootstrap::api_tls::CertInfo,
    config::Config,
    enroll::{Decision, Enrollments},
    keyed_lock::KeyedLock,
    leader::Leadership,
    mesh::MeshManager,
    reconcile::{AbeLedger, hosts_for_placement, ledger_totals},
    store::Store,
    tls::ClientCert,
    token::Tokens,
};

pub mod mesh;

/// Set on requests a follower forwards; a second hop means leadership moved
/// mid-request, so the receiver answers 503 instead of bouncing it on.
const FORWARDED_BY: HeaderName = HeaderName::from_static("x-xdebbie-forwarded-by");
/// Fingerprint of the cluster CA, sent with `GET /v1/ca`.
const CA_FINGERPRINT: HeaderName = HeaderName::from_static("x-xdebbie-ca-fingerprint");
/// Largest request body a follower will relay.
const FORWARD_BODY_LIMIT: usize = 16 << 20;

#[derive(Clone)]
pub struct App {
    pub store: Store,
    pub cfg: Arc<Config>,
    /// `None`: unauthenticated (no `token_file`).
    pub token: Option<Arc<Tokens>>,
    pub mesh: Arc<MeshManager>,
    /// Present when `[network]` is configured (even with `dhcp = false`).
    pub dhcp: Option<Arc<crate::dhcp::DhcpServer>>,
    /// For standalone volumes (`/v1/volumes`).
    pub abe: abe::Client,
    /// Who decides; followers forward cluster routes to the leader.
    pub leadership: Leadership,
    /// Client used to forward requests to the leader.
    pub http: reqwest::Client,
    /// Enrolment queue; signs only on the CA box.
    pub enroll: Arc<Enrollments>,
    /// One volume create per name at a time.
    pub volume_names: Arc<KeyedLock<String>>,
    /// Bytes promised to creates in flight, per host; the scheduler counts
    /// them (shared with the reconciler).
    pub abe_ledger: AbeLedger,
    /// Serves `/v1/agent/*` for the leader; `None` in tests.
    pub agent: Option<crate::agent::AgentApi>,
}

/// A ledger entry that is taken back when dropped, however the create
/// ends.
struct Reservation {
    ledger: AbeLedger,
    host: String,
    bytes: u64,
}

impl Reservation {
    fn take(ledger: &AbeLedger, host: &str, bytes: u64) -> Self {
        *ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(host.to_string())
            .or_default() += bytes;
        Self {
            ledger: ledger.clone(),
            host: host.to_string(),
            bytes,
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut l = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = l.get_mut(&self.host) {
            *b = b.saturating_sub(self.bytes);
            if *b == 0 {
                l.remove(&self.host);
            }
        }
    }
}

pub struct ApiErr(StatusCode, String);
impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(ApiError { error: self.1 })).into_response()
    }
}
impl ApiErr {
    pub fn internal(msg: impl Into<String>) -> Self {
        ApiErr(StatusCode::INTERNAL_SERVER_ERROR, msg.into())
    }
}
impl From<anyhow::Error> for ApiErr {
    fn from(e: anyhow::Error) -> Self {
        ApiErr(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}
type R<T> = Result<T, ApiErr>;
fn nf(what: &str) -> ApiErr {
    ApiErr(StatusCode::NOT_FOUND, format!("{what} not found"))
}
fn bad(m: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::BAD_REQUEST, m.into())
}
fn conflict(m: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::CONFLICT, m.into())
}

pub fn router(app: App) -> Router {
    // Cluster state: answered by the leader only.
    let v1 = Router::new()
        .route("/hosts", get(list_hosts))
        .route(
            "/hosts/{name}",
            get(get_host).put(put_host).delete(delete_host),
        )
        .route("/hosts/{name}/cordon", post(cordon))
        .route("/hosts/{name}/uncordon", post(uncordon))
        .route("/hosts/{name}/drain", post(drain))
        .route("/failure-domains", get(failure_domains))
        .route("/schedule", post(dry_run))
        .route("/vms", get(list_vms))
        .route("/vms/{id}", get(get_vm).put(put_vm).delete(delete_vm))
        .route("/vms/{id}/start", post(start))
        .route("/vms/{id}/stop", post(stop))
        .route("/vms/{id}/reboot", post(reboot))
        .route("/vms/{id}/migrate", post(migrate))
        .route("/volumes", get(list_volumes).post(create_volume))
        .route("/volumes/{id}", get(get_volume).delete(delete_volume))
        .route("/volumes/{id}/attach", post(attach_volume))
        .route("/volumes/{id}/detach", post(detach_volume))
        .route("/token/rotate", post(rotate_token))
        .layer(middleware::from_fn_with_state(
            app.clone(),
            forward_to_leader,
        ));
    // Enrolment: decided by the box holding the CA key.
    let enroll = Router::new()
        .route("/enroll", get(enroll_list).post(enroll_submit))
        .route("/enroll/{id}", get(enroll_get))
        .route("/enroll/{id}/approve", post(enroll_approve))
        .route("/enroll/{id}/deny", post(enroll_deny))
        .layer(middleware::from_fn_with_state(app.clone(), forward_to_ca));
    let v1 = v1
        .merge(enroll)
        // This box: served wherever you ask.
        .route("/leader", get(leader_status))
        .route("/token", get(token_status).put(put_token))
        .route("/enroll/token", get(enroll_token))
        .route("/ca", get(ca_cert))
        .route("/cert", get(cert_chain))
        .route("/network", get(network))
        .route("/version", get(version_info))
        .with_state(app.clone())
        .merge(mesh::router(mesh::MeshApi {
            mesh: app.mesh.clone(),
        }))
        // Host-local work for the leader (agent.rs): this box, never forwarded.
        .merge(match &app.agent {
            Some(a) => crate::agent::router(a.clone()),
            None => Router::new(),
        })
        .layer(middleware::from_fn_with_state(
            app.token.clone(),
            authenticate,
        ));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/v1", v1)
}

/// How a request proved who it is. Set on every request by [`authenticate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Authn {
    Token,
    /// Another xdebbied: client cert signed by the cluster CA.
    Cert(crate::tls::PeerIdentity),
    /// Only allowed on the public enrolment routes.
    Anonymous,
}

/// Routes a box may call before it has a token or a cert: ask to be
/// enrolled, poll the answer, fetch the CA to pin.
fn is_public(method: &axum::http::Method, path: &str) -> bool {
    let path = path.strip_prefix("/v1").unwrap_or(path);
    match (method, path) {
        (&axum::http::Method::POST, "/enroll") => true,
        (&axum::http::Method::GET, "/ca") => true,
        (&axum::http::Method::GET, "/cert") => true,
        (&axum::http::Method::GET, p) => p
            .strip_prefix("/enroll/")
            .is_some_and(|id| !id.is_empty() && id != "token" && !id.contains('/')),
        _ => false,
    }
}

/// Auth middleware usable by any router: bearer token, else a client
/// certificate the TLS layer verified, else 401 unless the route is public.
/// `None` tokens = unauthenticated API (lab use only).
pub async fn authenticate(
    State(tokens): State<Option<Arc<Tokens>>>,
    mut req: Request,
    next: Next,
) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let peer = req
        .extensions()
        .get::<ClientCert>()
        .and_then(|c| c.0.clone());
    let authn = match (&tokens, presented, peer) {
        (None, _, _) => Authn::Token,
        (Some(t), Some(p), _) if t.accepts(&p).await => Authn::Token,
        (Some(_), _, Some(id)) => Authn::Cert(id),
        _ => Authn::Anonymous,
    };
    if authn == Authn::Anonymous && !is_public(req.method(), req.uri().path()) {
        return ApiErr(
            StatusCode::UNAUTHORIZED,
            "missing or invalid bearer token (or client certificate)".into(),
        )
        .into_response();
    }
    req.extensions_mut().insert(authn);
    next.run(req).await
}

// ---------- tokens ----------

fn tokens(a: &App) -> R<&Arc<Tokens>> {
    a.token
        .as_ref()
        .ok_or_else(|| bad("no token_file configured: this API is unauthenticated"))
}

async fn token_status(State(a): State<App>) -> R<Json<TokenStatus>> {
    Ok(Json(tokens(&a)?.status().await))
}

/// The leader pushes its token set here; also fine to call by hand.
async fn put_token(State(a): State<App>, Json(set): Json<TokenSet>) -> R<Json<TokenStatus>> {
    if set.current.trim().is_empty() {
        return Err(bad("current token must not be empty"));
    }
    let t = tokens(&a)?;
    t.install(set).await?;
    Ok(Json(t.status().await))
}

/// Mint a new token. The old one keeps working until every peer has
/// confirmed the new one; the distributor then retires it everywhere.
async fn rotate_token(State(a): State<App>) -> R<(StatusCode, Json<TokenRotated>)> {
    let t = tokens(&a)?;
    if t.rotating().await {
        let pending: Vec<_> = t
            .status()
            .await
            .nodes
            .into_iter()
            .filter(|n| n.state != "current")
            .map(|n| n.hostname)
            .collect();
        return Err(conflict(format!(
            "a rotation is still being distributed (waiting on: {}); see GET /v1/token",
            if pending.is_empty() {
                "nobody yet checked".to_string()
            } else {
                pending.join(", ")
            }
        )));
    }
    let before = t.get().await.current;
    let token = t.rotate().await?;
    Ok((
        StatusCode::CREATED,
        Json(TokenRotated {
            id: crate::token::fingerprint(&token),
            previous: crate::token::fingerprint(&before),
            token,
        }),
    ))
}

// ---------- leader ----------

async fn leader_status(State(a): State<App>) -> Json<LeaderStatus> {
    Json(a.leadership.status().await)
}

/// Followers relay cluster routes to the leader; the leader (or a standalone
/// box) handles them itself.
async fn forward_to_leader(State(a): State<App>, req: Request, next: Next) -> Response {
    if a.leadership.is_leader().await {
        return next.run(req).await;
    }
    if let Some(from) = req.headers().get(&FORWARDED_BY) {
        let from = from.to_str().unwrap_or("?");
        return ApiErr(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("{from} forwarded this request here but leadership has moved; retry"),
        )
        .into_response();
    }
    let Some(leader) = a.leadership.leader().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "2")],
            Json(ApiError {
                error: "no leader elected yet; retry".into(),
            }),
        )
            .into_response();
    };
    let me = a.leadership.status().await.this.hostname;
    match forward(&a.http, &leader.api_url, &me, req).await {
        Ok(r) => r,
        Err(e) => ApiErr(
            StatusCode::BAD_GATEWAY,
            format!(
                "forwarding to leader {} ({}): {e:#}",
                leader.hostname, leader.api_url
            ),
        )
        .into_response(),
    }
}

async fn forward(
    http: &reqwest::Client,
    base: &str,
    me: &str,
    req: Request,
) -> anyhow::Result<Response> {
    let (parts, body) = req.into_parts();
    // Inside `nest("/v1")` the request URI is stripped; the original one
    // is what the leader must see.
    let uri = parts
        .extensions
        .get::<OriginalUri>()
        .map(|o| o.0.clone())
        .unwrap_or_else(|| parts.uri.clone());
    let pq = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let url = format!("{}{pq}", base.trim_end_matches('/'));
    let body = axum::body::to_bytes(body, FORWARD_BODY_LIMIT).await?;

    let mut out = http
        .request(parts.method.clone(), &url)
        .header(&FORWARDED_BY, me);
    for (k, v) in &parts.headers {
        if matches!(
            *k,
            header::HOST | header::CONTENT_LENGTH | header::CONNECTION | header::TRANSFER_ENCODING
        ) {
            continue;
        }
        out = out.header(k.as_str(), v.as_bytes());
    }
    let resp = out.body(body).send().await?;

    let mut r = Response::builder().status(resp.status().as_u16());
    for (k, v) in resp.headers() {
        if matches!(
            *k,
            header::CONTENT_LENGTH | header::CONNECTION | header::TRANSFER_ENCODING
        ) {
            continue;
        }
        r = r.header(k.as_str(), v.as_bytes());
    }
    Ok(r.body(Body::from(resp.bytes().await?))?)
}

// ---------- enrolment ----------

/// Enrolment routes belong to the box with the CA key. Elsewhere they are
/// forwarded to `ca_url` (set by `bootstrap --join`).
async fn forward_to_ca(State(a): State<App>, req: Request, next: Next) -> Response {
    if a.enroll.can_sign() {
        return next.run(req).await;
    }
    if let Some(from) = req.headers().get(&FORWARDED_BY) {
        let from = from.to_str().unwrap_or("?");
        return ApiErr(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("{from} forwarded this enrolment request here but this box has no CA key; check ca_url there"),
        )
        .into_response();
    }
    let Some(ca_url) = a.cfg.ca_url.clone() else {
        return ApiErr(
            StatusCode::CONFLICT,
            format!(
                "this box has no cluster CA key ({}) and no ca_url; run this against the box that was bootstrapped first",
                a.cfg.ca_key.display()
            ),
        )
        .into_response();
    };
    let me = a.leadership.status().await.this.hostname;
    match forward(&a.http, &ca_url, &me, req).await {
        Ok(r) => r,
        Err(e) => ApiErr(
            StatusCode::BAD_GATEWAY,
            format!("forwarding to CA box {ca_url}: {e:#}"),
        )
        .into_response(),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    all: bool,
}

async fn enroll_list(State(a): State<App>, Query(q): Query<ListQuery>) -> Json<Vec<Enrollment>> {
    Json(a.enroll.list(q.all).await)
}

/// A joiner asks for its certificate: queued until an operator approves.
async fn enroll_submit(
    State(a): State<App>,
    Json(req): Json<EnrollRequest>,
) -> R<(StatusCode, Json<Enrollment>)> {
    let e = a.enroll.submit(req).await.map_err(|e| bad(e.to_string()))?;
    if e.status == EnrollStatus::Pending {
        tracing::info!(id = %e.id, host = %e.hostname, fp = %e.fingerprint, "enrolment pending: `xdctl approve {}`", e.hostname);
    }
    Ok((StatusCode::ACCEPTED, Json(e)))
}

async fn enroll_get(State(a): State<App>, Path(id): Path<String>) -> R<Json<Enrollment>> {
    a.enroll
        .get(&id)
        .await
        .map(Json)
        .ok_or_else(|| nf("enrolment"))
}

async fn enroll_approve(State(a): State<App>, Path(id): Path<String>) -> R<Json<Enrollment>> {
    let e = a
        .enroll
        .decide(&id, Decision::Approve)
        .await
        .map_err(|e| ApiErr(StatusCode::NOT_FOUND, e.to_string()))?;
    tracing::info!(id = %e.id, host = %e.hostname, "enrolment approved");
    Ok(Json(e))
}

async fn enroll_deny(State(a): State<App>, Path(id): Path<String>) -> R<Json<Enrollment>> {
    let e = a
        .enroll
        .decide(&id, Decision::Deny)
        .await
        .map_err(|e| ApiErr(StatusCode::NOT_FOUND, e.to_string()))?;
    tracing::info!(id = %e.id, host = %e.hostname, "enrolment denied");
    Ok(Json(e))
}

/// The cluster token, for a box that just got its certificate. Client
/// certificate only: a bearer token holder has it already.
async fn enroll_token(
    State(a): State<App>,
    axum::Extension(authn): axum::Extension<Authn>,
) -> R<Json<TokenSet>> {
    let Authn::Cert(peer) = authn else {
        return Err(ApiErr(
            StatusCode::FORBIDDEN,
            "a client certificate signed by the cluster CA is required".into(),
        ));
    };
    let t = tokens(&a)?;
    tracing::info!(peer = %peer.hostname, "handed the cluster token to an enrolled box");
    Ok(Json(t.get().await))
}

/// The cluster CA certificate (PEM). Public: it is what a joiner pins.
async fn ca_cert(State(a): State<App>) -> R<([(HeaderName, String); 2], String)> {
    let pem = match a.enroll.ca_pem() {
        Some(p) => p.to_string(),
        None => std::fs::read_to_string(&a.cfg.ca_file).map_err(|e| {
            ApiErr(
                StatusCode::NOT_FOUND,
                format!("no CA certificate at {}: {e}", a.cfg.ca_file.display()),
            )
        })?,
    };
    let fp = crate::bootstrap::api_tls::cert_fingerprint(&pem)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-pem-file".to_string()),
            (CA_FINGERPRINT, fp),
        ],
        pem,
    ))
}

/// `GET /v1/cert`: the chain this box serves on its API port, leaf first —
/// its own certificate (`tls_cert`, which may already carry
/// intermediates) followed by the cluster CA that signed it. Box-local,
/// never forwarded: the leaf is per box. Public like `/ca`; it is what
/// any TLS client gets to see anyway. `xdctl ca --chain`.
async fn cert_chain(State(a): State<App>) -> R<Json<Vec<CertInfo>>> {
    let bad = |what: &str, e: std::io::Error| {
        ApiErr(StatusCode::NOT_FOUND, format!("no {what} certificate: {e}"))
    };
    let mut bundle = match &a.cfg.tls_cert {
        Some(p) => std::fs::read_to_string(p).map_err(|e| bad("server", e))?,
        None => {
            return Err(ApiErr(
                StatusCode::NOT_FOUND,
                "this xdebbied serves plain HTTP (no tls_cert configured)".into(),
            ));
        }
    };
    bundle.push('\n');
    match a.enroll.ca_pem() {
        Some(p) => bundle.push_str(p),
        None => {
            bundle.push_str(&std::fs::read_to_string(&a.cfg.ca_file).map_err(|e| bad("CA", e))?)
        }
    }
    let mut chain: Vec<CertInfo> = vec![];
    for c in crate::bootstrap::api_tls::describe_chain(&bundle)? {
        if !chain.iter().any(|k| k.fingerprint == c.fingerprint) {
            chain.push(c);
        }
    }
    Ok(Json(chain))
}

// ---------- hosts ----------

async fn list_hosts(State(a): State<App>) -> Json<Vec<Host>> {
    Json(a.store.read().await.hosts.into_values().collect())
}
async fn get_host(State(a): State<App>, Path(n): Path<String>) -> R<Json<Host>> {
    a.store
        .read()
        .await
        .hosts
        .remove(&n)
        .map(Json)
        .ok_or_else(|| nf("host"))
}
/// Upsert. Runtime fields (capacity, last_seen) are preserved.
async fn put_host(
    State(a): State<App>,
    Path(n): Path<String>,
    Json(mut h): Json<Host>,
) -> R<(StatusCode, Json<Host>)> {
    if h.name != n {
        return Err(bad("name in body must match path"));
    }
    let (code, h) = a
        .store
        .update(|s| {
            let code = if let Some(old) = s.hosts.get(&n) {
                h.capacity = old.capacity;
                h.abe_capacity = old.abe_capacity.clone();
                h.last_seen = old.last_seen;
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            s.hosts.insert(n.clone(), h.clone());
            (code, h)
        })
        .await?;
    Ok((code, Json(h)))
}
async fn delete_host(State(a): State<App>, Path(n): Path<String>) -> R<StatusCode> {
    a.store
        .update(|s| {
            let busy = s
                .vms
                .values()
                .filter(|v| v.host.as_deref() == Some(&n))
                .count();
            if busy > 0 {
                return Err(conflict(format!("{busy} VMs still on host; drain first")));
            }
            s.hosts
                .remove(&n)
                .map(|_| StatusCode::NO_CONTENT)
                .ok_or_else(|| nf("host"))
        })
        .await?
}
async fn set_cordon(a: &App, n: &str, v: bool) -> R<Json<Host>> {
    a.store
        .update(|s| {
            s.hosts
                .get_mut(n)
                .map(|h| {
                    h.cordoned = v;
                    Json(h.clone())
                })
                .ok_or_else(|| nf("host"))
        })
        .await?
}
async fn cordon(State(a): State<App>, Path(n): Path<String>) -> R<Json<Host>> {
    set_cordon(&a, &n, true).await
}
async fn uncordon(State(a): State<App>, Path(n): Path<String>) -> R<Json<Host>> {
    set_cordon(&a, &n, false).await
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DrainPlan {
    moves: BTreeMap<String, String>,
    stuck: BTreeMap<String, String>,
}

/// Cordon, then plan a live migration for every running VM on the host.
async fn drain(State(a): State<App>, Path(n): Path<String>) -> R<Json<DrainPlan>> {
    let stale = a.cfg.host_stale_secs;
    let policy = a.cfg.policy;
    let inflight = ledger_totals(&a.abe_ledger);
    a.store
        .update(|s| {
            let h = s.hosts.get_mut(&n).ok_or_else(|| nf("host"))?;
            h.cordoned = true;
            let mut plan = DrainPlan {
                moves: Default::default(),
                stuck: Default::default(),
            };
            let ids: Vec<Uuid> = s
                .vms
                .values()
                .filter(|v| v.host.as_deref() == Some(&n))
                .map(|v| v.id)
                .collect();
            for id in ids {
                let hosts = hosts_for_placement(&s.hosts, Utc::now(), stale);
                let vms: Vec<Vm> = s.vms.values().cloned().collect();
                let vm = s.vms.get_mut(&id).unwrap();
                if vm.phase != VmPhase::Running {
                    plan.stuck
                        .insert(vm.spec.name.clone(), format!("phase {:?}", vm.phase));
                    continue;
                }
                match place(
                    &vm.spec,
                    &hosts,
                    &vms,
                    policy,
                    |h| h.is_healthy(Utc::now(), stale),
                    Some(id),
                    &inflight,
                ) {
                    Ok(r) => {
                        plan.moves.insert(vm.spec.name.clone(), r[0].host.clone());
                        vm.migrate_to = Some(r[0].host.clone());
                        vm.phase = VmPhase::Migrating;
                        // Count the in-flight move against the target for later decisions.
                        vm.host = Some(r[0].host.clone());
                    }
                    Err(e) => {
                        plan.stuck.insert(vm.spec.name.clone(), e.to_string());
                    }
                }
            }
            // Restore source host on the VM record; the reconciler migrates from it.
            for vm in s.vms.values_mut() {
                if vm.phase == VmPhase::Migrating && plan.moves.contains_key(&vm.spec.name) {
                    vm.host = Some(n.clone());
                }
            }
            Ok(Json(plan))
        })
        .await?
}

async fn failure_domains(State(a): State<App>) -> Json<Vec<FailureDomain>> {
    let mut m: BTreeMap<String, FailureDomain> = BTreeMap::new();
    for h in a.store.read().await.hosts.into_values() {
        let fd = m
            .entry(h.failure_domain.clone())
            .or_insert_with(|| FailureDomain {
                name: h.failure_domain.clone(),
                control_plane: false,
                hosts: vec![],
            });
        fd.control_plane |= h.control_plane_eligible;
        fd.hosts.push(h.name);
    }
    Json(m.into_values().collect())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ScoredOut {
    host: String,
    host_peers: usize,
    fd_peers: usize,
    utilization: f64,
}

async fn dry_run(State(a): State<App>, Json(spec): Json<VmSpec>) -> R<Json<Vec<ScoredOut>>> {
    let s = a.store.read().await;
    let stale = a.cfg.host_stale_secs;
    let hosts = hosts_for_placement(&s.hosts, Utc::now(), stale);
    let vms: Vec<Vm> = s.vms.into_values().collect();
    place(
        &spec,
        &hosts,
        &vms,
        a.cfg.policy,
        |h| h.is_healthy(Utc::now(), stale),
        None,
        &ledger_totals(&a.abe_ledger),
    )
    .map(|r| {
        Json(
            r.into_iter()
                .map(|x| ScoredOut {
                    host: x.host,
                    host_peers: x.host_peers,
                    fd_peers: x.fd_peers,
                    utilization: x.utilization,
                })
                .collect(),
        )
    })
    .map_err(|e| ApiErr(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))
}

// ---------- vms ----------

#[derive(Deserialize)]
struct Sel {
    selector: Option<String>,
}

async fn list_vms(State(a): State<App>, Query(q): Query<Sel>) -> R<Json<Vec<Vm>>> {
    let want: Vec<(String, String)> = match q.selector.as_deref().filter(|s| !s.is_empty()) {
        None => vec![],
        Some(s) => s
            .split(',')
            .map(|kv| {
                kv.split_once('=')
                    .map(|(k, v)| (k.into(), v.into()))
                    .ok_or_else(|| bad("selector must be k=v[,k=v]"))
            })
            .collect::<R<_>>()?,
    };
    Ok(Json(
        a.store
            .read()
            .await
            .vms
            .into_values()
            .filter(|v| {
                want.iter()
                    .all(|(k, val)| v.spec.labels.get(k) == Some(val))
            })
            .collect(),
    ))
}

async fn get_vm(State(a): State<App>, Path(id): Path<Uuid>) -> R<Json<Vm>> {
    a.store
        .read()
        .await
        .vms
        .remove(&id)
        .map(Json)
        .ok_or_else(|| nf("vm"))
}

fn validate(s: &VmSpec) -> R<()> {
    let ok_name = !s.name.is_empty()
        && s.name.len() <= 63
        && s.name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.name.starts_with('-')
        && !s.name.ends_with('-');
    if !ok_name {
        return Err(bad("name must be a DNS-1123 label"));
    }
    if s.vcpus == 0 || s.memory_mib < 256 || s.disk_gib == 0 {
        return Err(bad("vcpus>0, memoryMib>=256, diskGib>0 required"));
    }
    if s.image.contains('/') || s.image.is_empty() {
        return Err(bad("image is a volume name in the host pool"));
    }
    Ok(())
}

/// `GET /v1/version`: daemon version and feature list. Box-local; never forwarded.
async fn version_info(State(a): State<App>) -> Json<VersionInfo> {
    let hostname = a.leadership.status().await.this.hostname;
    Json(VersionInfo {
        version: env!("CARGO_PKG_VERSION"),
        hostname,
        features: vec!["mesh", "bootstrap", "volumes", "dhcp", "enroll"],
    })
}

#[derive(Serialize)]
struct VersionInfo {
    version: &'static str,
    hostname: String,
    features: Vec<&'static str>,
}

/// `GET /v1/network`: overlay addressing config and DHCP responder status.
async fn network(State(a): State<App>) -> R<Json<crate::dhcp::NetworkStatus>> {
    match &a.dhcp {
        Some(d) => Ok(Json(d.status())),
        None => Err(nf("network (no [network] in xdebbied.toml)")),
    }
}

/// Idempotent create. Same id + same spec = 200; same id + different spec = 409.
async fn put_vm(
    State(a): State<App>,
    Path(id): Path<Uuid>,
    Json(spec): Json<VmSpec>,
) -> R<(StatusCode, Json<Vm>)> {
    validate(&spec)?;
    let net = a.cfg.network.clone();
    if spec.ip.is_some() && net.is_none() {
        return Err(bad("spec.ip requires [network] in xdebbied.toml"));
    }
    a.store
        .update(|s| {
            if let Some(v) = s.vms.get(&id) {
                return if v.spec == spec {
                    Ok((StatusCode::OK, Json(v.clone())))
                } else {
                    Err(conflict(
                        "vm exists with a different spec; VMs are immutable, replace them",
                    ))
                };
            }
            if s.vms
                .values()
                .any(|v| v.spec.name == spec.name && v.phase != VmPhase::Deleting)
            {
                return Err(conflict(format!("a vm named {} already exists", spec.name)));
            }
            // Overlay address: bound to the record for the VM's whole life.
            let ip = match &net {
                Some(n) => {
                    let in_use = crate::dhcp::NetworkConfig::in_use(s.vms.values());
                    Some(
                        n.allocate(spec.ip, &in_use)
                            .map_err(|e| conflict(e.to_string()))?,
                    )
                }
                None => None,
            };
            let addresses = ip
                .map(|ip| {
                    vec![Address {
                        kind: "InternalIP".into(),
                        address: ip.to_string(),
                    }]
                })
                .unwrap_or_default();
            let now = Utc::now();
            let vm = Vm {
                id,
                spec,
                phase: VmPhase::Pending,
                power: PowerIntent::On,
                host: None,
                addresses,
                ip,
                disks: Vec::new(),
                message: None,
                migrate_to: None,
                reboot_generation: 0,
                observed_reboot_generation: 0,
                created: now,
                updated: now,
            };
            s.vms.insert(id, vm.clone());
            Ok((StatusCode::CREATED, Json(vm)))
        })
        .await?
}

async fn mutate(a: &App, id: Uuid, f: impl FnOnce(&mut Vm) -> R<()>) -> R<(StatusCode, Json<Vm>)> {
    a.store
        .update(|s| {
            let vm = s.vms.get_mut(&id).ok_or_else(|| nf("vm"))?;
            f(vm)?;
            vm.updated = Utc::now();
            Ok((StatusCode::ACCEPTED, Json(vm.clone())))
        })
        .await?
}

#[derive(Deserialize)]
struct DeleteQ {
    #[serde(default)]
    force: bool,
}

async fn delete_vm(
    State(a): State<App>,
    Path(id): Path<Uuid>,
    Query(q): Query<DeleteQ>,
) -> R<(StatusCode, Json<Vm>)> {
    mutate(&a, id, |v| {
        if v.spec.is_protected() && !q.force {
            return Err(conflict(format!(
                "vm \"{}\" is protected (label {}=true); delete with ?force=true / xdctl vm rm --force",
                v.spec.name, PROTECTED_LABEL
            )));
        }
        v.phase = VmPhase::Deleting;
        v.power = PowerIntent::Off;
        Ok(())
    })
    .await
}
async fn start(State(a): State<App>, Path(id): Path<Uuid>) -> R<(StatusCode, Json<Vm>)> {
    mutate(&a, id, |v| {
        if v.phase == VmPhase::Deleting {
            return Err(conflict("vm is deleting"));
        }
        v.power = PowerIntent::On;
        Ok(())
    })
    .await
}
async fn stop(State(a): State<App>, Path(id): Path<Uuid>) -> R<(StatusCode, Json<Vm>)> {
    mutate(&a, id, |v| {
        v.power = PowerIntent::Off;
        Ok(())
    })
    .await
}
async fn reboot(State(a): State<App>, Path(id): Path<Uuid>) -> R<(StatusCode, Json<Vm>)> {
    mutate(&a, id, |v| {
        if v.phase != VmPhase::Running {
            return Err(conflict("only running vms can reboot"));
        }
        v.reboot_generation += 1;
        Ok(())
    })
    .await
}

async fn migrate(
    State(a): State<App>,
    Path(id): Path<Uuid>,
    Json(req): Json<MigrateRequest>,
) -> R<(StatusCode, Json<Vm>)> {
    let stale = a.cfg.host_stale_secs;
    let policy = a.cfg.policy;
    let inflight = ledger_totals(&a.abe_ledger);
    a.store
        .update(|s| {
            let hosts = hosts_for_placement(&s.hosts, Utc::now(), stale);
            let vms: Vec<Vm> = s.vms.values().cloned().collect();
            let vm = s.vms.get_mut(&id).ok_or_else(|| nf("vm"))?;
            if vm.phase != VmPhase::Running {
                return Err(conflict("only running vms can live-migrate"));
            }
            let mut spec = vm.spec.clone();
            spec.placement.host = req.host.clone();
            let cur = vm.host.clone();
            let target = place(
                &spec,
                &hosts,
                &vms,
                policy,
                |h| h.is_healthy(Utc::now(), stale) && Some(&h.name) != cur.as_ref(),
                Some(id),
                &inflight,
            )
            .map_err(|e| ApiErr(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
            vm.migrate_to = Some(target[0].host.clone());
            vm.phase = VmPhase::Migrating;
            vm.updated = Utc::now();
            Ok((StatusCode::ACCEPTED, Json(vm.clone())))
        })
        .await?
}

// ---------- volumes ----------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateVolume {
    /// Caller's name; a second POST with the same name returns the
    /// existing volume (200) instead of making another.
    name: String,
    bytes: u64,
    /// Which host's ABE should hold it; default: any healthy host with one.
    #[serde(default)]
    host: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachVolume {
    vm: Uuid,
    /// Take the volume away from a VM that still has it (data loss if the
    /// guest is using it).
    #[serde(default)]
    force: bool,
}

async fn list_volumes(State(a): State<App>) -> Json<Vec<Volume>> {
    Json(a.store.read().await.volumes.into_values().collect())
}

async fn get_volume(State(a): State<App>, Path(id): Path<String>) -> R<Json<Volume>> {
    a.store
        .read()
        .await
        .volumes
        .remove(&id)
        .map(Json)
        .ok_or_else(|| nf("volume"))
}

/// Where a standalone volume of `bytes` should go: the ABE host with the
/// most headroom (ties by name), or the pinned one if it fits. `Err` is
/// the 507 message listing every host's reason.
fn choose_abe_host(
    st: &crate::store::State,
    pinned: Option<&str>,
    bytes: u64,
    now: chrono::DateTime<Utc>,
    stale: i64,
    inflight: &BTreeMap<String, u64>,
) -> Result<Host, ApiErr> {
    let hosts = hosts_for_placement(&st.hosts, now, stale);
    let vms: Vec<Vm> = st.vms.values().cloned().collect();
    let mut pending = abe_pending(&hosts, &vms);
    for (h, b) in inflight {
        if let Some(p) = pending.get_mut(h) {
            *p += b;
        }
    }
    let mut reasons = vec![];
    let mut best: Option<(u64, Host)> = None;
    for h in hosts {
        if pinned.is_some_and(|p| p != h.name) {
            continue;
        }
        if !h.is_healthy(now, stale) {
            reasons.push(format!("{}: unhealthy", h.name));
            continue;
        }
        if h.abe.is_none() {
            // Not an ABE host at all; only worth a line when pinned.
            if pinned.is_some() {
                reasons.push(format!("{}: no abe", h.name));
            }
            continue;
        }
        match h.abe_fit(pending.get(&h.name).copied().unwrap_or(0), bytes) {
            Ok(room) => {
                let better = match &best {
                    None => true,
                    Some((r, b)) => room > *r || (room == *r && h.name < b.name),
                };
                if better {
                    best = Some((room, h));
                }
            }
            Err(r) => reasons.push(format!("{}: {r}", h.name)),
        }
    }
    if let Some((_, h)) = best {
        return Ok(h);
    }
    if let Some(p) = pinned
        && !st.hosts.contains_key(p)
    {
        return Err(nf("host"));
    }
    if reasons.is_empty() {
        return Err(ApiErr(
            StatusCode::SERVICE_UNAVAILABLE,
            "no healthy host with an ABE ([[hosts]].abe)".into(),
        ));
    }
    Err(ApiErr(
        StatusCode::INSUFFICIENT_STORAGE,
        format!("no ABE host fits {bytes} bytes: {}", reasons.join(", ")),
    ))
}

/// Create on ABE now (the id is ABE's), then record it. Idempotent on
/// name, and serialized per name so concurrent POSTs make one ABE call:
/// the second sees the record the first inserted. The ABE host is chosen
/// by capacity (3.4) and the bytes are held in the ledger until the record
/// exists, so the scheduler cannot promise them twice.
async fn create_volume(
    State(a): State<App>,
    Json(req): Json<CreateVolume>,
) -> R<(StatusCode, Json<Volume>)> {
    if req.name.is_empty() || req.name.len() > 253 {
        return Err(bad("name is required"));
    }
    if req.bytes == 0 {
        return Err(bad("bytes must be > 0"));
    }
    let _g = a.volume_names.lock(req.name.clone()).await;
    let st = a.store.read().await;
    if let Some(v) = st.volumes.values().find(|v| v.name == req.name) {
        return if v.bytes >= req.bytes {
            Ok((StatusCode::OK, Json(v.clone())))
        } else {
            Err(conflict(format!(
                "volume {} exists with {} bytes (< {} requested)",
                req.name, v.bytes, req.bytes
            )))
        };
    }
    let stale = a.cfg.host_stale_secs;
    let now = Utc::now();
    let host = choose_abe_host(
        &st,
        req.host.as_deref(),
        req.bytes,
        now,
        stale,
        &ledger_totals(&a.abe_ledger),
    )?;
    drop(st);
    let server = host.abe.clone().unwrap_or_default();
    let _res = Reservation::take(&a.abe_ledger, &host.name, req.bytes);
    let named = a
        .abe
        .has(&server, "name")
        .await
        .map_err(|e| ApiErr(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    let e = if named {
        a.abe
            .create_named(&server, req.bytes, &abe::volume_abe_name(&req.name))
            .await
    } else {
        a.abe.create(&server, req.bytes).await
    }
    .map_err(|e| {
        let code = match abe::http_status(&e) {
            Some(503) | Some(507) => StatusCode::INSUFFICIENT_STORAGE,
            Some(409) => StatusCode::CONFLICT,
            _ => StatusCode::BAD_GATEWAY,
        };
        ApiErr(code, format!("{e:#}"))
    })?;
    let bytes = e.bytes.unwrap_or(req.bytes);
    tracing::info!(name = %req.name, id = %e.id, %server, host = %host.name, bytes, existing = e.existing, "created volume");
    let vol = Volume {
        id: e.id,
        name: req.name,
        server,
        traddr: e.traddr,
        port: e.port,
        bytes,
        perf: e.perf,
        attach_to: None,
        attached: None,
        message: None,
        deleting: false,
        created: now,
        updated: now,
    };
    a.store
        .update(|s| {
            s.volumes.insert(vol.id.clone(), vol.clone());
        })
        .await?;
    Ok((StatusCode::CREATED, Json(vol)))
}

/// Delete on ABE and forget. Refused while attached (detach first); a
/// volume that is already gone is a success. The record is marked
/// `deleting` first, in the same store update as the attachment check, so
/// nothing can attach it meanwhile and a retry after an ABE failure (502)
/// carries on from there.
async fn delete_volume(State(a): State<App>, Path(id): Path<String>) -> R<StatusCode> {
    let vol = a
        .store
        .update(|s| {
            let Some(v) = s.volumes.get_mut(&id) else {
                return Ok(None);
            };
            if v.attach_to.is_some() || v.attached.is_some() {
                return Err(conflict(format!(
                    "volume {id} is attached (to {}); detach it first",
                    v.attached
                        .as_ref()
                        .map(|a| a.vm)
                        .or(v.attach_to)
                        .unwrap_or_default()
                )));
            }
            if !v.deleting {
                v.deleting = true;
                v.updated = Utc::now();
            }
            Ok(Some(v.clone()))
        })
        .await??;
    let Some(vol) = vol else {
        return Ok(StatusCode::NO_CONTENT);
    };
    a.abe
        .delete(&vol.server, &vol.id)
        .await
        .map_err(|e| ApiErr(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    a.store
        .update(|s| {
            s.volumes.remove(&id);
        })
        .await?;
    tracing::info!(%id, server = %vol.server, "deleted volume");
    Ok(StatusCode::NO_CONTENT)
}

async fn mutate_volume(
    a: &App,
    id: &str,
    f: impl FnOnce(&crate::store::State, &mut Volume) -> R<()>,
) -> R<(StatusCode, Json<Volume>)> {
    a.store
        .update(|s| {
            let snapshot = s.clone();
            let v = s.volumes.get_mut(id).ok_or_else(|| nf("volume"))?;
            f(&snapshot, v)?;
            v.updated = Utc::now();
            Ok((StatusCode::ACCEPTED, Json(v.clone())))
        })
        .await?
}

/// Ask for the volume to be plugged into a VM. The reconciler does it;
/// poll `GET /volumes/{id}` until `attached.vm` matches.
async fn attach_volume(
    State(a): State<App>,
    Path(id): Path<String>,
    Json(req): Json<AttachVolume>,
) -> R<(StatusCode, Json<Volume>)> {
    mutate_volume(&a, &id, |st, v| {
        if v.deleting {
            return Err(conflict("volume is being deleted"));
        }
        let vm = st.vms.get(&req.vm).ok_or_else(|| nf("vm"))?;
        if vm.phase == VmPhase::Deleting {
            return Err(conflict("vm is deleting"));
        }
        if let Some(att) = &v.attached
            && att.vm != req.vm
            && !req.force
        {
            let live = st.vms.get(&att.vm).is_some_and(|o| o.phase == VmPhase::Running);
            if live {
                return Err(conflict(format!(
                    "volume is attached to running vm {}; detach it there first or attach with force=true",
                    att.vm
                )));
            }
        }
        v.attach_to = Some(req.vm);
        Ok(())
    })
    .await
}

async fn detach_volume(
    State(a): State<App>,
    Path(id): Path<String>,
) -> R<(StatusCode, Json<Volume>)> {
    mutate_volume(&a, &id, |_, v| {
        v.attach_to = None;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, time::Duration};

    use tokio::sync::RwLock;

    use super::*;
    use crate::{
        discovery::peer::{PeerInfo, PeerTable},
        election::state::ElectionStateMachine,
        leader::Leadership,
        mesh::{FakeNetOps, MeshManager},
        token::Tokens,
    };

    /// Serve a router on an ephemeral port; returns its base URL.
    async fn spawn(leadership: Leadership, store: Store) -> String {
        let tokens = Arc::new(Tokens::in_memory(TokenSet {
            current: "secret".into(),
            previous: None,
        }));
        spawn_with(leadership, store, tokens).await
    }

    async fn spawn_with(leadership: Leadership, store: Store, tokens: Arc<Tokens>) -> String {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeNetOps::new();
        f.add_bridge("br-int", 1500, "52:54:00:23:00:fe");
        f.add_underlay("ul0", 1420, Ipv4Addr::new(10, 22, 0, 1));
        let mesh = MeshManager::new_for_test(Arc::new(f), dir.path().into(), None).unwrap();
        let app = App {
            store,
            cfg: Arc::new(toml::from_str("state_file = 'unused'").unwrap()),
            token: Some(tokens),
            mesh,
            dhcp: None,
            abe: abe::Client::new(),
            leadership,
            http: reqwest::Client::new(),
            enroll: Arc::new(
                Enrollments::load(dir.path().join("enroll.json"), None, vec![]).unwrap(),
            ),
            volume_names: Arc::default(),
            abe_ledger: Arc::default(),
            agent: None,
        };
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(l, router(app)).await.unwrap();
            drop(dir);
        });
        url
    }

    /// The real thing: HTTPS with client certificates, this box holding the
    /// CA. A joiner needs no credentials to enrol and poll; once approved,
    /// its certificate alone gets it the token and every other route.
    #[tokio::test]
    async fn enrolment_over_mtls_ends_with_the_cluster_token() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::bootstrap::api_tls::generate_localhost(dir.path());
        let signer = crate::bootstrap::api_tls::CaSigner::load(&paths.ca_cert, &paths.ca_key)
            .unwrap()
            .unwrap();
        let ca_pem = signer.ca_pem().to_string();
        let f = FakeNetOps::new();
        f.add_bridge("br-int", 1500, "52:54:00:23:00:fe");
        f.add_underlay("ul0", 1420, Ipv4Addr::new(10, 22, 0, 1));
        let mesh = MeshManager::new_for_test(Arc::new(f), dir.path().into(), None).unwrap();
        let app = App {
            store: Store::open(dir.path().join("state.json")).unwrap(),
            cfg: Arc::new(toml::from_str("state_file = 'unused'").unwrap()),
            token: Some(Arc::new(Tokens::in_memory(TokenSet {
                current: "secret".into(),
                previous: None,
            }))),
            mesh,
            dhcp: None,
            abe: abe::Client::new(),
            leadership: Leadership::standalone("ca-box", "https://unused"),
            http: reqwest::Client::new(),
            enroll: Arc::new(
                Enrollments::load(dir.path().join("enroll.json"), Some(signer), vec![0]).unwrap(),
            ),
            volume_names: Arc::default(),
            abe_ledger: Arc::default(),
            agent: None,
        };
        let roots = crate::tls::client_roots(&paths.ca_cert, &dir.path().join("nonexistent"));
        let sc = crate::tls::server_config(&paths.server_cert, &paths.server_key, roots).unwrap();
        let handle: axum_server::Handle<std::net::SocketAddr> = axum_server::Handle::new();
        let h = handle.clone();
        tokio::spawn(async move {
            axum_server::bind("127.0.0.1:0".parse().unwrap())
                .acceptor(crate::tls::PeerCertAcceptor::new(sc))
                .handle(h)
                .serve(router(app).into_make_service())
                .await
                .unwrap();
        });
        let addr = handle.listening().await.unwrap();
        let url = format!("https://127.0.0.1:{}", addr.port());
        let ca = reqwest::Certificate::from_pem(ca_pem.as_bytes()).unwrap();
        let anon = reqwest::Client::builder()
            .use_rustls_tls()
            .add_root_certificate(ca.clone())
            .build()
            .unwrap();

        // No credentials: public routes only.
        assert_eq!(
            anon.get(format!("{url}/v1/leader"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            anon.get(format!("{url}/v1/enroll"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            anon.get(format!("{url}/v1/enroll/token"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        let served_ca = anon
            .get(format!("{url}/v1/ca"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(served_ca, ca_pem);

        // Joiner: key + CSR, submit, poll.
        let csr = crate::bootstrap::api_tls::make_csr("joiner.test").unwrap();
        let req = EnrollRequest {
            hostname: "joiner.test".into(),
            box_id: None,
            api_url: "https://joiner.test:7443".into(),
            csr: csr.csr_pem,
            wg_pubkey: None,
        };
        let r = anon
            .post(format!("{url}/v1/enroll"))
            .json(&req)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 202);
        let e: Enrollment = r.json().await.unwrap();
        assert_eq!(
            (e.status, e.fingerprint.as_str()),
            (EnrollStatus::Pending, csr.fingerprint.as_str())
        );
        assert_eq!(e.box_id, 1, "assigned by the CA box (which is 0)");
        let polled: Enrollment = anon
            .get(format!("{url}/v1/enroll/{}", e.id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(polled.status, EnrollStatus::Pending);
        assert_eq!(
            anon.post(format!("{url}/v1/enroll/{}/approve", e.id))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );

        // Operator approves by hostname with the token.
        let pending: Vec<Enrollment> = anon
            .get(format!("{url}/v1/enroll"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        let r = anon
            .post(format!("{url}/v1/enroll/joiner.test/approve"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let approved: Enrollment = anon
            .get(format!("{url}/v1/enroll/{}", e.id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(approved.status, EnrollStatus::Approved);
        let cert = approved.cert.unwrap();
        assert_eq!(approved.ca.as_deref(), Some(ca_pem.as_str()));

        // Token holders do not get the token route; the new member does,
        // and everything else, on its certificate alone.
        assert_eq!(
            anon.get(format!("{url}/v1/enroll/token"))
                .bearer_auth("secret")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        let ident =
            reqwest::Identity::from_pem(format!("{cert}\n{}", csr.key_pem).as_bytes()).unwrap();
        let member = reqwest::Client::builder()
            .use_rustls_tls()
            .add_root_certificate(ca)
            .identity(ident)
            .build()
            .unwrap();
        let set: TokenSet = member
            .get(format!("{url}/v1/enroll/token"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(set.current, "secret");
        let st: LeaderStatus = member
            .get(format!("{url}/v1/leader"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(st.is_leader);
        let status: TokenStatus = member
            .get(format!("{url}/v1/token"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status.id, crate::token::fingerprint("secret"));
    }

    #[tokio::test]
    async fn follower_forwards_cluster_routes_to_the_leader() {
        let dir = tempfile::tempdir().unwrap();
        let leader_store = Store::open(dir.path().join("leader.json")).unwrap();
        let follower_store = Store::open(dir.path().join("follower.json")).unwrap();

        // Leader: standalone is enough, it answers for itself.
        let leader_id = Uuid::new_v4();
        let leader_url = spawn(
            Leadership::standalone("leader", "http://unused"),
            leader_store.clone(),
        )
        .await;

        // Follower: election says `leader_id` leads and the peer table knows its URL.
        let me = Uuid::new_v4();
        let election = Arc::new(RwLock::new(ElectionStateMachine::new(
            me,
            Duration::from_secs(10),
            Duration::from_secs(5),
        )));
        election.write().await.activate();
        election
            .write()
            .await
            .received_leader_heartbeat(leader_id, 1e9);
        let peers = Arc::new(RwLock::new(PeerTable::new()));
        peers.write().await.insert(PeerInfo::new(
            leader_id,
            "leader".into(),
            leader_url.clone(),
            1e9,
        ));
        let me_ref = LeaderRef {
            node_id: Some(me),
            hostname: "follower".into(),
            api_url: "http://unused".into(),
        };
        let follower_url = spawn(
            Leadership::for_test(me_ref, election.clone(), peers),
            follower_store.clone(),
        )
        .await;

        let c = reqwest::Client::new();
        let auth = |r: reqwest::RequestBuilder| r.bearer_auth("secret");

        // /v1/leader is answered locally and points at the leader.
        let st: LeaderStatus = auth(c.get(format!("{follower_url}/v1/leader")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(st.state, "follower");
        assert_eq!(st.leader.unwrap().api_url, leader_url);

        // A write through the follower lands in the leader's store only.
        let host = serde_json::json!({"name": "h1", "uri": "test:///", "failureDomain": "r1"});
        let r = auth(c.put(format!("{follower_url}/v1/hosts/h1")))
            .json(&host)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{}", r.text().await.unwrap());
        assert!(leader_store.read().await.hosts.contains_key("h1"));
        assert!(follower_store.read().await.hosts.is_empty());

        // Reads through the follower see the leader's data.
        let hosts: Vec<Host> = auth(c.get(format!("{follower_url}/v1/hosts")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(hosts.len(), 1);

        // Status codes and bodies come back verbatim.
        let r = auth(c.get(format!("{follower_url}/v1/hosts/nope")))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert_eq!(r.json::<ApiError>().await.unwrap().error, "host not found");

        // Auth is checked before forwarding.
        let r = c
            .get(format!("{follower_url}/v1/hosts"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

        // A request that was already forwarded is not bounced again.
        let r = auth(c.get(format!("{follower_url}/v1/hosts")))
            .header(FORWARDED_BY.as_str(), "other")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);

        // No leader known: 503 with Retry-After.
        election.write().await.start_election();
        let r = auth(c.get(format!("{follower_url}/v1/hosts")))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(r.headers().get(header::RETRY_AFTER).unwrap(), "2");

        // Once this box leads it answers from its own store.
        election.write().await.become_leader();
        let hosts: Vec<Host> = auth(c.get(format!("{follower_url}/v1/hosts")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(hosts.is_empty());
    }

    #[tokio::test]
    async fn rotation_overlaps_until_every_peer_took_the_new_token() {
        use crate::token::{Distributor, Seen};
        let dir = tempfile::tempdir().unwrap();
        let shared = || {
            Arc::new(Tokens::in_memory(TokenSet {
                current: "secret".into(),
                previous: None,
            }))
        };

        // Follower first, so the leader's peer table can name it.
        let f_tokens = shared();
        let follower_url = spawn_with(
            Leadership::standalone("follower", "http://unused"),
            Store::open(dir.path().join("f.json")).unwrap(),
            f_tokens.clone(),
        )
        .await;

        let me = Uuid::new_v4();
        let election = Arc::new(RwLock::new(ElectionStateMachine::new(
            me,
            Duration::from_secs(10),
            Duration::from_secs(5),
        )));
        election.write().await.activate();
        election.write().await.become_leader();
        let peers = Arc::new(RwLock::new(PeerTable::new()));
        peers.write().await.insert(PeerInfo::new(
            Uuid::new_v4(),
            "follower".into(),
            follower_url.clone(),
            1.0,
        ));
        let leadership = Leadership::for_test(
            LeaderRef {
                node_id: Some(me),
                hostname: "leader".into(),
                api_url: "http://unused".into(),
            },
            election,
            peers,
        );
        let l_tokens = shared();
        let leader_url = spawn_with(
            leadership.clone(),
            Store::open(dir.path().join("l.json")).unwrap(),
            l_tokens.clone(),
        )
        .await;

        let c = reqwest::Client::new();
        let get = |url: &str, tok: &str| c.get(format!("{url}/v1/token")).bearer_auth(tok).send();

        // Rotate on the leader: new token returned once, old still accepted there.
        let r = c
            .post(format!("{leader_url}/v1/token/rotate"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
        let rotated: TokenRotated = r.json().await.unwrap();
        assert_eq!(rotated.previous, crate::token::fingerprint("secret"));
        assert!(
            get(&leader_url, "secret")
                .await
                .unwrap()
                .status()
                .is_success()
        );
        assert!(
            get(&leader_url, &rotated.token)
                .await
                .unwrap()
                .status()
                .is_success()
        );
        // ...but the follower has not heard yet.
        assert_eq!(
            get(&follower_url, &rotated.token).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        // A second rotation while this one is out is refused.
        let r = c
            .post(format!("{leader_url}/v1/token/rotate"))
            .bearer_auth(&rotated.token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CONFLICT);

        // Sweep 1: the follower rejects the new token, so the pair is pushed with the old one.
        let d = Distributor {
            tokens: l_tokens.clone(),
            leadership,
            http: reqwest::Client::new(),
        };
        let seen = d.sweep().await;
        assert_eq!(seen[&follower_url], Seen::Installing);
        assert!(
            f_tokens.accepts("secret").await && f_tokens.accepts(&rotated.token).await,
            "overlap on the follower"
        );
        assert!(
            l_tokens.rotating().await,
            "not retired until the follower is confirmed"
        );
        let st: TokenStatus = get(&leader_url, "secret")
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            (st.rotating, st.nodes[0].state.as_str()),
            (true, "installing")
        );

        // Sweep 2: confirmed with the new token on the follower -> previous retired everywhere.
        let seen = d.sweep().await;
        assert_eq!(seen[&follower_url], Seen::Current { overlap: true });
        assert!(!l_tokens.rotating().await);
        assert!(!f_tokens.rotating().await);
        assert_eq!(
            get(&follower_url, "secret").await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&leader_url, "secret").await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert!(
            get(&follower_url, &rotated.token)
                .await
                .unwrap()
                .status()
                .is_success()
        );

        // Steady state: a peer still holding a retired token gets tidied.
        f_tokens
            .install(TokenSet {
                current: rotated.token.clone(),
                previous: Some("stray".into()),
            })
            .await
            .unwrap();
        let seen = d.sweep().await;
        assert_eq!(seen[&follower_url], Seen::Current { overlap: true });
        assert!(!f_tokens.rotating().await);
        assert_eq!(
            d.sweep().await[&follower_url],
            Seen::Current { overlap: false }
        );

        // A peer that missed the rotation entirely is reported, not fixed.
        f_tokens
            .install(TokenSet {
                current: "else".into(),
                previous: None,
            })
            .await
            .unwrap();
        assert_eq!(d.sweep().await[&follower_url], Seen::Stale);
        let st: TokenStatus = get(&leader_url, &rotated.token)
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(st.nodes[0].state, "stale");
    }
}
