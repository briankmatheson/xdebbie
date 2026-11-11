// SPDX-License-Identifier: GPL-3.0-or-later
//! REST API. Handlers only mutate desired state; all hypervisor I/O happens
//! in the reconciler. Every write is idempotent so controllers can retry.

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use xdebbie_core::*;
use serde::Deserialize;
use uuid::Uuid;

use crate::{config::Config, mesh::MeshManager, store::Store};

pub mod mesh;

#[derive(Clone)]
pub struct App {
    pub store: Store,
    pub cfg: Arc<Config>,
    pub token: Option<Arc<String>>,
    pub mesh: Arc<MeshManager>,
    /// Present when `[network]` is configured (even with `dhcp = false`).
    pub dhcp: Option<Arc<crate::dhcp::DhcpServer>>,
}

pub struct ApiErr(StatusCode, String);
impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(ApiError { error: self.1 })).into_response()
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
        .route("/network", get(network))
        .with_state(app.clone())
        .merge(mesh::router(mesh::MeshApi { mesh: app.mesh.clone() }))
        .layer(middleware::from_fn_with_state(app.token.clone(), bearer_auth));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/v1", v1)
}

/// Bearer-token middleware usable by any router (`None` = unauthenticated).
pub async fn bearer_auth(State(token): State<Option<Arc<String>>>, req: Request, next: Next) -> Response {
    if let Some(tok) = &token {
        let ok = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|v| constant_time_eq(v.as_bytes(), tok.as_bytes()));
        if !ok {
            return ApiErr(
                StatusCode::UNAUTHORIZED,
                "missing or invalid bearer token".into(),
            )
            .into_response();
        }
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
                let hosts: Vec<Host> = s.hosts.values().cloned().collect();
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
    let hosts: Vec<Host> = s.hosts.into_values().collect();
    let vms: Vec<Vm> = s.vms.into_values().collect();
    let stale = a.cfg.host_stale_secs;
    place(
        &spec,
        &hosts,
        &vms,
        a.cfg.policy,
        |h| h.is_healthy(Utc::now(), stale),
        None,
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
                    Some(n.allocate(spec.ip, &in_use).map_err(|e| conflict(e.to_string()))?)
                }
                None => None,
            };
            let addresses = ip
                .map(|ip| vec![Address { kind: "InternalIP".into(), address: ip.to_string() }])
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
    a.store
        .update(|s| {
            let hosts: Vec<Host> = s.hosts.values().cloned().collect();
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
            )
            .map_err(|e| ApiErr(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
            vm.migrate_to = Some(target[0].host.clone());
            vm.phase = VmPhase::Migrating;
            vm.updated = Utc::now();
            Ok((StatusCode::ACCEPTED, Json(vm.clone())))
        })
        .await?
}
