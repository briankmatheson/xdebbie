// SPDX-License-Identifier: GPL-3.0-or-later
//! Host-local work on behalf of the leader.
//!
//! The thin pool a root disk is carved from, the NVMe connects an ABE disk
//! needs, the seed image build and the base image an ABE root is filled
//! from all live on the box that runs the VM. Driving its libvirtd over
//! `qemu+tls://` from the leader reaches none of them, so the leader's
//! reconciler goes through [`Dispatch`]: a host whose `agent` is another
//! box gets every [`Hypervisor`] call sent as `POST /v1/agent/<op>` to that
//! box's daemon, which runs it with its own [`Virsh`] (local LVM, NVMe,
//! `qemu:///system`) and answers with the result. Hosts without an `agent`
//! (added by hand with `xdctl host add`) are driven directly, as before.
//!
//! The routes are served by every box for itself (never forwarded) and are
//! authenticated like everything else: the cluster token, or a client
//! certificate signed by the cluster CA.

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result, bail};
use axum::{Json, Router, extract::State, routing::post};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use xdebbie_core::{Address, ApiError, Attachment, Capacity, Disk, Host, Vm, Volume};

use crate::{
    api::ApiErr,
    config::Config,
    libvirt::{DomState, Hypervisor, Virsh},
    token::Tokens,
};

// ---------- wire types ----------

#[derive(Serialize, Deserialize)]
pub struct HostReq {
    pub host: Host,
}
#[derive(Serialize, Deserialize)]
pub struct VmReq {
    pub host: Host,
    pub vm: Vm,
}
#[derive(Serialize, Deserialize)]
pub struct ShutdownReq {
    pub host: Host,
    pub vm: Vm,
    pub force: bool,
}
#[derive(Serialize, Deserialize)]
pub struct DisksReq {
    pub host: Host,
    pub vm: Vm,
    pub disks: Vec<Disk>,
}
/// `prepare_disks` reports partial progress even when it fails, so a retry
/// never allocates twice; the error rides along instead of a status code.
#[derive(Serialize, Deserialize)]
pub struct DisksResp {
    pub disks: Vec<Disk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct AttachReq {
    pub host: Host,
    pub vm: Vm,
    pub vol: Volume,
}
#[derive(Serialize, Deserialize)]
pub struct DetachReq {
    pub host: Host,
    pub vol: Volume,
    pub att: Attachment,
}
#[derive(Serialize, Deserialize)]
pub struct MigrateReq {
    pub src: Host,
    pub dst: Host,
    pub vm: Vm,
}
#[derive(Serialize, Deserialize)]
pub struct StateResp {
    pub state: Option<DomState>,
}
#[derive(Serialize, Deserialize)]
pub struct TargetResp {
    pub target: String,
}
#[derive(Serialize, Deserialize, Default)]
pub struct Empty {}

/// Two API URLs naming the same daemon, give or take a trailing slash.
fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim_end_matches('/'))
}

/// The host as this box drives it: the `[[hosts]]` entry of the same name
/// from the local config when there is one (local URI, local pool), else
/// the record as the leader sent it with `qemu:///system` in place of a
/// TLS URI pointing back at this box.
fn localize(cfg: &Config, host: &Host) -> Host {
    if let Some(mine) = cfg.hosts.iter().find(|h| h.name == host.name) {
        let mut h = host.clone();
        h.uri = mine.uri.clone();
        h.storage_pool = mine.storage_pool.clone();
        h.bridge = mine.bridge.clone();
        return h;
    }
    let mut h = host.clone();
    if h.uri.starts_with("qemu+tls://") || h.uri.starts_with("qemu+tcp://") {
        h.uri = "qemu:///system".into();
    }
    h
}

// ---------- leader side ----------

/// The leader's hypervisor: local for hosts this box owns or that have no
/// agent, remote through that agent otherwise.
pub struct Dispatch<H = Virsh> {
    pub local: Arc<H>,
    pub cfg: Arc<Config>,
    pub http: reqwest::Client,
    pub tokens: Option<Arc<Tokens>>,
    /// This daemon's API URL; a host whose `agent` is this is local.
    pub me: String,
}

impl<H: Hypervisor> Dispatch<H> {
    fn agent<'a>(&self, host: &'a Host) -> Option<&'a str> {
        host.agent.as_deref().filter(|a| !same_url(a, &self.me))
    }

    async fn call<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        agent: &str,
        op: &str,
        req: &Req,
    ) -> Result<Resp> {
        let url = format!("{}/v1/agent/{op}", agent.trim_end_matches('/'));
        let mut r = self.http.post(&url).json(req);
        if let Some(t) = &self.tokens {
            r = r.bearer_auth(t.get().await.current);
        }
        let resp = r.send().await.with_context(|| format!("agent {url}"))?;
        let status = resp.status();
        let body = resp
            .bytes()
            .await
            .with_context(|| format!("agent {url}: reading reply"))?;
        if !status.is_success() {
            let msg = serde_json::from_slice::<ApiError>(&body)
                .map(|e| e.error)
                .unwrap_or_else(|_| String::from_utf8_lossy(&body).trim().to_string());
            bail!("agent {agent} {op}: {status}: {msg}");
        }
        serde_json::from_slice(&body).with_context(|| format!("agent {url}: parsing reply"))
    }
}

impl<H: Hypervisor> Hypervisor for Dispatch<H> {
    async fn inventory(&self, host: &Host) -> Result<Capacity> {
        match self.agent(host) {
            Some(a) => {
                self.call(a, "inventory", &HostReq { host: host.clone() })
                    .await
            }
            None => self.local.inventory(&localize(&self.cfg, host)).await,
        }
    }

    async fn state(&self, host: &Host, vm: &Vm) -> Result<Option<DomState>> {
        match self.agent(host) {
            Some(a) => {
                let r: StateResp = self
                    .call(
                        a,
                        "state",
                        &VmReq {
                            host: host.clone(),
                            vm: vm.clone(),
                        },
                    )
                    .await?;
                Ok(r.state)
            }
            None => self.local.state(&localize(&self.cfg, host), vm).await,
        }
    }

    async fn prepare_disks(&self, host: &Host, vm: &Vm, disks: &mut Vec<Disk>) -> Result<()> {
        match self.agent(host) {
            Some(a) => {
                let req = DisksReq {
                    host: host.clone(),
                    vm: vm.clone(),
                    disks: disks.clone(),
                };
                let r: DisksResp = self.call(a, "prepare-disks", &req).await?;
                *disks = r.disks;
                match r.error {
                    Some(e) => bail!("{e}"),
                    None => Ok(()),
                }
            }
            None => {
                self.local
                    .prepare_disks(&localize(&self.cfg, host), vm, disks)
                    .await
            }
        }
    }

    async fn provision(&self, host: &Host, vm: &Vm) -> Result<()> {
        match self.agent(host) {
            Some(a) => self
                .call::<_, Empty>(
                    a,
                    "provision",
                    &VmReq {
                        host: host.clone(),
                        vm: vm.clone(),
                    },
                )
                .await
                .map(drop),
            None => self.local.provision(&localize(&self.cfg, host), vm).await,
        }
    }

    async fn start(&self, host: &Host, vm: &Vm) -> Result<()> {
        match self.agent(host) {
            Some(a) => self
                .call::<_, Empty>(
                    a,
                    "start",
                    &VmReq {
                        host: host.clone(),
                        vm: vm.clone(),
                    },
                )
                .await
                .map(drop),
            None => self.local.start(&localize(&self.cfg, host), vm).await,
        }
    }

    async fn shutdown(&self, host: &Host, vm: &Vm, force: bool) -> Result<()> {
        match self.agent(host) {
            Some(a) => self
                .call::<_, Empty>(
                    a,
                    "shutdown",
                    &ShutdownReq {
                        host: host.clone(),
                        vm: vm.clone(),
                        force,
                    },
                )
                .await
                .map(drop),
            None => {
                self.local
                    .shutdown(&localize(&self.cfg, host), vm, force)
                    .await
            }
        }
    }

    async fn reboot(&self, host: &Host, vm: &Vm) -> Result<()> {
        match self.agent(host) {
            Some(a) => self
                .call::<_, Empty>(
                    a,
                    "reboot",
                    &VmReq {
                        host: host.clone(),
                        vm: vm.clone(),
                    },
                )
                .await
                .map(drop),
            None => self.local.reboot(&localize(&self.cfg, host), vm).await,
        }
    }

    async fn destroy(&self, host: &Host, vm: &Vm) -> Result<()> {
        match self.agent(host) {
            Some(a) => self
                .call::<_, Empty>(
                    a,
                    "destroy",
                    &VmReq {
                        host: host.clone(),
                        vm: vm.clone(),
                    },
                )
                .await
                .map(drop),
            None => self.local.destroy(&localize(&self.cfg, host), vm).await,
        }
    }

    async fn addresses(&self, host: &Host, vm: &Vm) -> Result<Vec<Address>> {
        match self.agent(host) {
            Some(a) => {
                self.call(
                    a,
                    "addresses",
                    &VmReq {
                        host: host.clone(),
                        vm: vm.clone(),
                    },
                )
                .await
            }
            None => self.local.addresses(&localize(&self.cfg, host), vm).await,
        }
    }

    async fn tap_owners(&self, host: &Host) -> Result<HashMap<String, String>> {
        match self.agent(host) {
            Some(a) => {
                self.call(a, "tap-owners", &HostReq { host: host.clone() })
                    .await
            }
            None => self.local.tap_owners(&localize(&self.cfg, host)).await,
        }
    }

    /// Runs where the domain is. The destination's disks are still made by
    /// whoever runs this, so LVM-backed migration between boxes is as
    /// unsupported as it was; qcow2 pools with `--copy-storage-inc` work.
    async fn migrate(&self, src: &Host, dst: &Host, vm: &Vm) -> Result<()> {
        match self.agent(src) {
            Some(a) => self
                .call::<_, Empty>(
                    a,
                    "migrate",
                    &MigrateReq {
                        src: src.clone(),
                        dst: dst.clone(),
                        vm: vm.clone(),
                    },
                )
                .await
                .map(drop),
            None => self.local.migrate(&localize(&self.cfg, src), dst, vm).await,
        }
    }

    async fn attach_volume(&self, host: &Host, vm: &Vm, vol: &Volume) -> Result<String> {
        match self.agent(host) {
            Some(a) => {
                let req = AttachReq {
                    host: host.clone(),
                    vm: vm.clone(),
                    vol: vol.clone(),
                };
                let r: TargetResp = self.call(a, "attach-volume", &req).await?;
                Ok(r.target)
            }
            None => {
                self.local
                    .attach_volume(&localize(&self.cfg, host), vm, vol)
                    .await
            }
        }
    }

    async fn detach_volume(&self, host: &Host, vol: &Volume, att: &Attachment) -> Result<()> {
        match self.agent(host) {
            Some(a) => {
                let req = DetachReq {
                    host: host.clone(),
                    vol: vol.clone(),
                    att: att.clone(),
                };
                self.call::<_, Empty>(a, "detach-volume", &req)
                    .await
                    .map(drop)
            }
            None => {
                self.local
                    .detach_volume(&localize(&self.cfg, host), vol, att)
                    .await
            }
        }
    }
}

// ---------- agent side ----------

pub struct AgentApi<H = Virsh> {
    pub hv: Arc<H>,
    pub cfg: Arc<Config>,
}

impl<H> Clone for AgentApi<H> {
    fn clone(&self) -> Self {
        Self {
            hv: self.hv.clone(),
            cfg: self.cfg.clone(),
        }
    }
}

/// `/agent/*`, mounted under `/v1` on every box, inside the auth layer and
/// outside the leader forward.
pub fn router<H: Hypervisor>(api: AgentApi<H>) -> Router {
    Router::new()
        .route("/agent/inventory", post(inventory::<H>))
        .route("/agent/state", post(state::<H>))
        .route("/agent/prepare-disks", post(prepare_disks::<H>))
        .route("/agent/provision", post(provision::<H>))
        .route("/agent/start", post(start::<H>))
        .route("/agent/shutdown", post(shutdown::<H>))
        .route("/agent/reboot", post(reboot::<H>))
        .route("/agent/destroy", post(destroy::<H>))
        .route("/agent/addresses", post(addresses::<H>))
        .route("/agent/tap-owners", post(tap_owners::<H>))
        .route("/agent/migrate", post(migrate::<H>))
        .route("/agent/attach-volume", post(attach_volume::<H>))
        .route("/agent/detach-volume", post(detach_volume::<H>))
        .with_state(api)
}

type R<T> = Result<Json<T>, ApiErr>;

fn err(e: anyhow::Error) -> ApiErr {
    ApiErr::internal(format!("{e:#}"))
}

async fn inventory<H: Hypervisor>(
    State(a): State<AgentApi<H>>,
    Json(q): Json<HostReq>,
) -> R<Capacity> {
    a.hv.inventory(&localize(&a.cfg, &q.host))
        .await
        .map(Json)
        .map_err(err)
}
async fn state<H: Hypervisor>(State(a): State<AgentApi<H>>, Json(q): Json<VmReq>) -> R<StateResp> {
    let state =
        a.hv.state(&localize(&a.cfg, &q.host), &q.vm)
            .await
            .map_err(err)?;
    Ok(Json(StateResp { state }))
}
async fn prepare_disks<H: Hypervisor>(
    State(a): State<AgentApi<H>>,
    Json(mut q): Json<DisksReq>,
) -> R<DisksResp> {
    let r =
        a.hv.prepare_disks(&localize(&a.cfg, &q.host), &q.vm, &mut q.disks)
            .await;
    Ok(Json(DisksResp {
        disks: q.disks,
        error: r.err().map(|e| format!("{e:#}")),
    }))
}
async fn provision<H: Hypervisor>(State(a): State<AgentApi<H>>, Json(q): Json<VmReq>) -> R<Empty> {
    a.hv.provision(&localize(&a.cfg, &q.host), &q.vm)
        .await
        .map_err(err)?;
    Ok(Json(Empty {}))
}
async fn start<H: Hypervisor>(State(a): State<AgentApi<H>>, Json(q): Json<VmReq>) -> R<Empty> {
    a.hv.start(&localize(&a.cfg, &q.host), &q.vm)
        .await
        .map_err(err)?;
    Ok(Json(Empty {}))
}
async fn shutdown<H: Hypervisor>(
    State(a): State<AgentApi<H>>,
    Json(q): Json<ShutdownReq>,
) -> R<Empty> {
    a.hv.shutdown(&localize(&a.cfg, &q.host), &q.vm, q.force)
        .await
        .map_err(err)?;
    Ok(Json(Empty {}))
}
async fn reboot<H: Hypervisor>(State(a): State<AgentApi<H>>, Json(q): Json<VmReq>) -> R<Empty> {
    a.hv.reboot(&localize(&a.cfg, &q.host), &q.vm)
        .await
        .map_err(err)?;
    Ok(Json(Empty {}))
}
async fn destroy<H: Hypervisor>(State(a): State<AgentApi<H>>, Json(q): Json<VmReq>) -> R<Empty> {
    a.hv.destroy(&localize(&a.cfg, &q.host), &q.vm)
        .await
        .map_err(err)?;
    Ok(Json(Empty {}))
}
async fn addresses<H: Hypervisor>(
    State(a): State<AgentApi<H>>,
    Json(q): Json<VmReq>,
) -> R<Vec<Address>> {
    a.hv.addresses(&localize(&a.cfg, &q.host), &q.vm)
        .await
        .map(Json)
        .map_err(err)
}
async fn tap_owners<H: Hypervisor>(
    State(a): State<AgentApi<H>>,
    Json(q): Json<HostReq>,
) -> R<HashMap<String, String>> {
    a.hv.tap_owners(&localize(&a.cfg, &q.host))
        .await
        .map(Json)
        .map_err(err)
}
async fn migrate<H: Hypervisor>(
    State(a): State<AgentApi<H>>,
    Json(q): Json<MigrateReq>,
) -> R<Empty> {
    a.hv.migrate(&localize(&a.cfg, &q.src), &q.dst, &q.vm)
        .await
        .map_err(err)?;
    Ok(Json(Empty {}))
}
async fn attach_volume<H: Hypervisor>(
    State(a): State<AgentApi<H>>,
    Json(q): Json<AttachReq>,
) -> R<TargetResp> {
    let target =
        a.hv.attach_volume(&localize(&a.cfg, &q.host), &q.vm, &q.vol)
            .await
            .map_err(err)?;
    Ok(Json(TargetResp { target }))
}
async fn detach_volume<H: Hypervisor>(
    State(a): State<AgentApi<H>>,
    Json(q): Json<DetachReq>,
) -> R<Empty> {
    a.hv.detach_volume(&localize(&a.cfg, &q.host), &q.vol, &q.att)
        .await
        .map_err(err)?;
    Ok(Json(Empty {}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, uri: &str, agent: Option<&str>) -> Host {
        let mut h: Host =
            serde_json::from_value(serde_json::json!({"name": name, "uri": uri})).unwrap();
        h.agent = agent.map(String::from);
        h
    }

    fn cfg_with(hosts: Vec<Host>) -> Config {
        let mut c: Config = toml::from_str("").unwrap();
        c.hosts = hosts;
        c
    }

    #[test]
    fn localize_prefers_the_local_config_entry() {
        let cfg = cfg_with(vec![host("box0", "qemu:///system", None)]);
        let sent = host(
            "box0",
            "qemu+tls://10.23.0.1/system",
            Some("https://box0:7443"),
        );
        let h = localize(&cfg, &sent);
        assert_eq!(h.uri, "qemu:///system");
        assert_eq!(h.agent.as_deref(), Some("https://box0:7443"));
    }

    #[test]
    fn localize_falls_back_to_the_system_uri() {
        let cfg = cfg_with(vec![]);
        assert_eq!(
            localize(&cfg, &host("x", "qemu+tls://10.23.0.1/system", None)).uri,
            "qemu:///system"
        );
        assert_eq!(
            localize(&cfg, &host("x", "qemu+ssh://xdebbie@srv5/system", None)).uri,
            "qemu+ssh://xdebbie@srv5/system"
        );
    }

    #[test]
    fn same_url_ignores_trailing_slash_and_case() {
        assert!(same_url("https://Box0:7443/", "https://box0:7443"));
        assert!(!same_url("https://box0:7443", "https://box1:7443"));
    }

    /// Records every call; `state` answers Running for domains it provisioned.
    #[derive(Default)]
    struct Fake {
        calls: std::sync::Mutex<Vec<String>>,
        doms: std::sync::Mutex<std::collections::BTreeSet<String>>,
    }
    impl Fake {
        fn log(&self, host: &Host, what: &str) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{what}@{}:{}", host.name, host.uri));
        }
    }
    impl Hypervisor for Fake {
        async fn inventory(&self, h: &Host) -> Result<Capacity> {
            self.log(h, "inventory");
            Ok(Capacity {
                cpus: 8,
                memory_mib: 4096,
                disk_free_gib: 77,
            })
        }
        async fn state(&self, h: &Host, v: &Vm) -> Result<Option<DomState>> {
            self.log(h, "state");
            Ok(self
                .doms
                .lock()
                .unwrap()
                .contains(&v.domain_name())
                .then_some(DomState::Running))
        }
        async fn prepare_disks(&self, h: &Host, _: &Vm, disks: &mut Vec<Disk>) -> Result<()> {
            self.log(h, "prepare_disks");
            let d: Disk = serde_json::from_value(serde_json::json!({
                "role": "root", "server": "https://10.22.1.1:443", "traddr": "10.22.1.1", "port": "4420",
                "id": "0123456789abcdef0123456789abcdef", "bytes": 1u64 << 30, "populated": true
            }))
            .unwrap();
            disks.push(d);
            bail!("second disk failed")
        }
        async fn provision(&self, h: &Host, v: &Vm) -> Result<()> {
            self.log(h, "provision");
            self.doms.lock().unwrap().insert(v.domain_name());
            Ok(())
        }
        async fn start(&self, h: &Host, _: &Vm) -> Result<()> {
            self.log(h, "start");
            Ok(())
        }
        async fn shutdown(&self, h: &Host, _: &Vm, force: bool) -> Result<()> {
            self.log(h, if force { "shutdown!" } else { "shutdown" });
            Ok(())
        }
        async fn reboot(&self, h: &Host, _: &Vm) -> Result<()> {
            self.log(h, "reboot");
            bail!("no such domain")
        }
        async fn destroy(&self, h: &Host, v: &Vm) -> Result<()> {
            self.log(h, "destroy");
            self.doms.lock().unwrap().remove(&v.domain_name());
            Ok(())
        }
        async fn addresses(&self, h: &Host, _: &Vm) -> Result<Vec<Address>> {
            self.log(h, "addresses");
            Ok(vec![Address {
                kind: "InternalIP".into(),
                address: "10.23.1.5".into(),
            }])
        }
        async fn tap_owners(&self, h: &Host) -> Result<HashMap<String, String>> {
            self.log(h, "tap_owners");
            Ok(HashMap::from([("vnet3".to_string(), "xd-abc".to_string())]))
        }
        async fn migrate(&self, s: &Host, d: &Host, _: &Vm) -> Result<()> {
            self.log(s, &format!("migrate->{}", d.name));
            Ok(())
        }
        async fn attach_volume(&self, h: &Host, _: &Vm, _: &Volume) -> Result<String> {
            self.log(h, "attach_volume");
            Ok("vdd".into())
        }
        async fn detach_volume(&self, h: &Host, _: &Volume, _: &Attachment) -> Result<()> {
            self.log(h, "detach_volume");
            Ok(())
        }
    }

    fn vm(name: &str) -> Vm {
        serde_json::from_value(serde_json::json!({
            "id": "11111111-2222-3333-4444-555555555555",
            "spec": {"name": name, "image": "debian-13", "vcpus": 1, "memoryMib": 512, "diskGib": 10},
            "phase": "Pending", "power": "On",
            "created": "2026-01-01T00:00:00Z", "updated": "2026-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    /// The leader (an empty config: it owns no host) drives a host whose
    /// agent is a fake box behind the real router; the fake sees every call
    /// with its own local URI, and results and errors come back intact.
    #[tokio::test]
    async fn every_call_round_trips_through_the_agent() {
        let fake = Arc::new(Fake::default());
        let box1_cfg: Config = toml::from_str(
            "state_file = 'unused'\n[[hosts]]\nname = 'box1'\nuri = 'qemu:///system'\nstorage_pool = 'abe'\nbridge = 'virbr-xdebbie'",
        )
        .unwrap();
        let app = Router::new().nest(
            "/v1",
            router(AgentApi {
                hv: fake.clone(),
                cfg: Arc::new(box1_cfg),
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let d = Dispatch {
            local: Arc::new(Fake::default()),
            cfg: Arc::new(toml::from_str("state_file = 'unused'").unwrap()),
            http: reqwest::Client::new(),
            tokens: None,
            me: "http://leader:7443".into(),
        };
        // What the leader's store holds for box1: the registrar's remote view.
        let h = host("box1", "qemu+tls://10.23.1.1/system", Some(&agent_url));
        let v = vm("w1-cp-abc");

        let cap = d.inventory(&h).await.unwrap();
        assert_eq!(cap.disk_free_gib, 77);
        assert_eq!(d.state(&h, &v).await.unwrap(), None);
        d.provision(&h, &v).await.unwrap();
        assert_eq!(d.state(&h, &v).await.unwrap(), Some(DomState::Running));
        d.start(&h, &v).await.unwrap();
        d.shutdown(&h, &v, true).await.unwrap();
        let e = d.reboot(&h, &v).await.unwrap_err().to_string();
        assert!(e.contains("no such domain") && e.contains("reboot"), "{e}");
        assert_eq!(d.addresses(&h, &v).await.unwrap()[0].address, "10.23.1.5");
        assert_eq!(d.tap_owners(&h).await.unwrap()["vnet3"], "xd-abc");
        let dst = host(
            "box2",
            "qemu+tls://10.23.2.1/system",
            Some("http://box2:7443"),
        );
        d.migrate(&h, &dst, &v).await.unwrap();
        let vol: Volume = serde_json::from_value(serde_json::json!({
            "id": "fedcba9876543210fedcba9876543210", "name": "data", "server": "https://10.22.1.1:443",
            "traddr": "10.22.1.1", "port": "4420", "bytes": 1u64 << 30,
            "created": "2026-01-01T00:00:00Z", "updated": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(d.attach_volume(&h, &v, &vol).await.unwrap(), "vdd");
        let att: Attachment = serde_json::from_value(
            serde_json::json!({"vm": v.id, "host": "box1", "target": "vdd"}),
        )
        .unwrap();
        d.detach_volume(&h, &vol, &att).await.unwrap();
        // partial progress survives a failed prepare_disks
        let mut disks = vec![];
        let e = d
            .prepare_disks(&h, &v, &mut disks)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(e, "second disk failed");
        assert_eq!(disks.len(), 1);
        d.destroy(&h, &v).await.unwrap();
        assert_eq!(d.state(&h, &v).await.unwrap(), None);

        let calls = fake.calls.lock().unwrap().clone();
        assert!(
            calls.iter().all(|c| c.ends_with("@box1:qemu:///system")),
            "{calls:?}"
        );
        assert_eq!(calls[0], "inventory@box1:qemu:///system");
        assert!(calls.contains(&"shutdown!@box1:qemu:///system".to_string()));
        assert!(calls.contains(&"migrate->box2@box1:qemu:///system".to_string()));
        // the leader's own driver saw nothing
        assert!(d.local.calls.lock().unwrap().is_empty());
    }

    /// A host whose agent is this box, or that has none, is driven locally.
    #[tokio::test]
    async fn own_and_agentless_hosts_stay_local() {
        let local = Arc::new(Fake::default());
        let d = Dispatch {
            local: local.clone(),
            cfg: Arc::new(
                toml::from_str(
                    "state_file = 'unused'\n[[hosts]]\nname = 'box0'\nuri = 'qemu:///system'",
                )
                .unwrap(),
            ),
            http: reqwest::Client::new(),
            tokens: None,
            me: "https://box0:7443".into(),
        };
        // mirrored from a previous leader: remote view of ourselves
        d.inventory(&host(
            "box0",
            "qemu+tls://10.23.0.1/system",
            Some("https://box0:7443/"),
        ))
        .await
        .unwrap();
        d.inventory(&host("srv5", "qemu+ssh://xdebbie@srv5/system", None))
            .await
            .unwrap();
        let calls = local.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                "inventory@box0:qemu:///system",
                "inventory@srv5:qemu+ssh://xdebbie@srv5/system"
            ]
        );
    }
}
