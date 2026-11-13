// SPDX-License-Identifier: GPL-3.0-or-later
//! XDebbie8sCluster and XDebbie8sMachine reconcilers.

use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, anyhow};
use chrono::Utc;
use k8s_openapi::api::core::v1::Secret;
use kube::{
    Api, Client, ResourceExt,
    api::{ApiResource, DynamicObject, GroupVersionKind, Patch, PatchParams},
    runtime::{
        controller::Action,
        finalizer::{Event, finalizer},
    },
};
use serde_json::{Value, json};
use tracing::{info, warn};
use uuid::Uuid;
use xdebbie_core::{Placement, VmPhase, VmSpec};

use crate::{crd::*, xdebbie::XDebbie8s};

pub const CLUSTER_FINALIZER: &str = "xdebbiecluster.infrastructure.cluster.x-k8s.io";
pub const MACHINE_FINALIZER: &str = "xdebbiemachine.infrastructure.cluster.x-k8s.io";
const CLUSTER_NAME: &str = "cluster.x-k8s.io/cluster-name";
const PAUSED: &str = "cluster.x-k8s.io/paused";
const CP_LABEL: &str = "cluster.x-k8s.io/control-plane";
const MD_LABEL: &str = "cluster.x-k8s.io/deployment-name";
const MS_LABEL: &str = "cluster.x-k8s.io/set-name";
pub const PROVIDER_ID_TOKEN: &str = "{{ ds.meta_data.provider_id }}";

pub struct Ctx {
    pub client: Client,
    pub xdebbie: XDebbie8s,
    pub capi_version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Any(#[from] anyhow::Error),
    #[error(transparent)]
    Kube(#[from] kube::Error),
    #[error("finalizer: {0}")]
    Finalizer(String),
}
type Result<T> = std::result::Result<T, Error>;

pub fn error_policy<K>(_: Arc<K>, e: &Error, _: Arc<Ctx>) -> Action {
    warn!(error = %e, "reconcile failed");
    Action::requeue(Duration::from_secs(15))
}

impl Ctx {
    fn capi(&self, kind: &str, ns: &str) -> Api<DynamicObject> {
        let gvk = GroupVersionKind::gvk("cluster.x-k8s.io", &self.capi_version, kind);
        Api::namespaced_with(self.client.clone(), ns, &ApiResource::from_gvk(&gvk))
    }
    async fn owner(
        &self,
        meta: &kube::api::ObjectMeta,
        kind: &str,
    ) -> Result<Option<DynamicObject>> {
        let ns = meta.namespace.as_deref().unwrap_or("default");
        let Some(r) = meta
            .owner_references
            .iter()
            .flatten()
            .find(|r| r.kind == kind && r.api_version.starts_with("cluster.x-k8s.io/"))
        else {
            return Ok(None);
        };
        Ok(self.capi(kind, ns).get_opt(&r.name).await?)
    }
    async fn cluster_for(&self, meta: &kube::api::ObjectMeta) -> Result<Option<DynamicObject>> {
        let ns = meta.namespace.as_deref().unwrap_or("default");
        match meta.labels.as_ref().and_then(|l| l.get(CLUSTER_NAME)) {
            Some(n) => Ok(self.capi("Cluster", ns).get_opt(n).await?),
            None => self.owner(meta, "Cluster").await,
        }
    }
}

fn is_paused(obj_meta: &kube::api::ObjectMeta, cluster: Option<&DynamicObject>) -> bool {
    obj_meta
        .annotations
        .as_ref()
        .is_some_and(|a| a.contains_key(PAUSED))
        || cluster.is_some_and(|c| {
            c.data
                .pointer("/spec/paused")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
}

fn cond(
    t: &str,
    ok: bool,
    reason: &str,
    msg: Option<String>,
    generation: Option<i64>,
    prev: &[Condition],
) -> Condition {
    let status = if ok { "True" } else { "False" }.to_string();
    let ltt = prev
        .iter()
        .find(|c| c.type_ == t && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    Condition {
        type_: t.into(),
        status,
        reason: Some(reason.into()),
        message: msg,
        last_transition_time: ltt,
        observed_generation: generation,
    }
}

fn finalizer_err<E: std::fmt::Display>(e: E) -> Error {
    Error::Finalizer(e.to_string())
}

// ===================== XDebbie8sCluster =====================

pub async fn reconcile_cluster(fc: Arc<XDebbie8sCluster>, ctx: Arc<Ctx>) -> Result<Action> {
    let ns = fc.namespace().unwrap_or_default();
    let api: Api<XDebbie8sCluster> = Api::namespaced(ctx.client.clone(), &ns);
    finalizer(&api, CLUSTER_FINALIZER, fc, |ev| async {
        match ev {
            Event::Apply(fc) => apply_cluster(&api, &fc, &ctx).await,
            Event::Cleanup(fc) => cleanup_cluster(&fc, &ctx).await,
        }
    })
    .await
    .map_err(finalizer_err)
}

async fn apply_cluster(
    api: &Api<XDebbie8sCluster>,
    fc: &XDebbie8sCluster,
    ctx: &Ctx,
) -> Result<Action> {
    let cluster = ctx.owner(&fc.metadata, "Cluster").await?;
    let Some(cluster) = cluster else {
        info!(name = %fc.name_any(), "waiting for Cluster controller to set ownerRef");
        return Ok(Action::requeue(Duration::from_secs(10)));
    };
    if is_paused(&fc.metadata, Some(&cluster)) {
        return Ok(Action::await_change());
    }

    let prev = fc.status.clone().unwrap_or_default();
    let generation = fc.metadata.generation;
    let ep = &fc.spec.control_plane_endpoint;
    let fds = ctx
        .xdebbie
        .failure_domains()
        .await
        .context("listing failure domains")?;
    let fds: Vec<FailureDomain> = fds
        .into_iter()
        .filter(|f| fc.spec.failure_domains.is_empty() || fc.spec.failure_domains.contains(&f.name))
        .map(|f| FailureDomain {
            name: f.name,
            control_plane: f.control_plane,
            attributes: [("hosts".to_string(), f.hosts.join(","))].into(),
        })
        .collect();

    let ep_ok = !ep.host.is_empty() && ep.port > 0;
    let fd_ok = fds.iter().any(|f| f.control_plane);
    let ready = ep_ok && fd_ok;
    let why = if !ep_ok {
        Some(
            "spec.controlPlaneEndpoint host and port are required (use a kube-vip VIP)".to_string(),
        )
    } else if !fd_ok {
        Some("no control-plane-eligible failure domain registered in xdebbied".to_string())
    } else {
        None
    };
    let status = XDebbie8sClusterStatus {
        initialization: Some(Initialization {
            provisioned: Some(ready),
        }),
        failure_domains: fds,
        conditions: vec![cond(
            "Ready",
            ready,
            if ready { "Provisioned" } else { "NotReady" },
            why,
            generation,
            &prev.conditions,
        )],
    };
    if fc.status.as_ref() != Some(&status) {
        api.patch_status(
            &fc.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({ "status": status })),
        )
        .await?;
    }
    Ok(Action::requeue(Duration::from_secs(if ready {
        120
    } else {
        15
    })))
}

/// Belt and braces: remove any VMs still labelled with this cluster.
async fn cleanup_cluster(fc: &XDebbie8sCluster, ctx: &Ctx) -> Result<Action> {
    let sel = format!(
        "xdebbie.dev/cluster={}.{}",
        fc.namespace().unwrap_or_default(),
        fc.name_any()
    );
    let left = ctx.xdebbie.list_vms(&sel).await?;
    if left.is_empty() {
        return Ok(Action::await_change());
    }
    for vm in &left {
        ctx.xdebbie.delete_vm(vm.id).await?;
    }
    Err(anyhow!("{} VMs still deleting", left.len()).into())
}

// ===================== XDebbie8sMachine =====================

pub async fn reconcile_machine(fm: Arc<XDebbie8sMachine>, ctx: Arc<Ctx>) -> Result<Action> {
    let ns = fm.namespace().unwrap_or_default();
    let api: Api<XDebbie8sMachine> = Api::namespaced(ctx.client.clone(), &ns);
    finalizer(&api, MACHINE_FINALIZER, fm, |ev| async {
        match ev {
            Event::Apply(fm) => apply_machine(&api, &fm, &ctx).await,
            Event::Cleanup(fm) => cleanup_machine(&fm, &ctx).await,
        }
    })
    .await
    .map_err(finalizer_err)
}

/// The VM's identity. On first reconcile it is the XDebbie8sMachine's UID; after
/// that it is read back from `spec.providerID`, which survives
/// `clusterctl move` (UIDs do not).
pub fn vm_id(fm: &XDebbie8sMachine) -> Result<Uuid> {
    if let Some(id) = fm
        .spec
        .provider_id
        .as_deref()
        .and_then(|p| p.strip_prefix("xdebbie://"))
    {
        return Ok(Uuid::parse_str(id).map_err(anyhow::Error::from)?);
    }
    Ok(Uuid::parse_str(
        fm.metadata
            .uid
            .as_deref()
            .ok_or_else(|| anyhow!("no uid"))?,
    )
    .map_err(anyhow::Error::from)?)
}

async fn cleanup_machine(fm: &XDebbie8sMachine, ctx: &Ctx) -> Result<Action> {
    let id = vm_id(fm)?;
    if ctx.xdebbie.delete_vm(id).await? {
        info!(machine = %fm.name_any(), "vm gone");
        return Ok(Action::await_change());
    }
    // Keep the finalizer until xdebbied confirms the VM is gone.
    Err(anyhow!("vm {id} still deleting").into())
}

async fn apply_machine(
    api: &Api<XDebbie8sMachine>,
    fm: &XDebbie8sMachine,
    ctx: &Ctx,
) -> Result<Action> {
    let name = fm.name_any();
    let ns = fm.namespace().unwrap_or_default();
    let wait = |s| Ok(Action::requeue(Duration::from_secs(s)));

    let Some(machine) = ctx.owner(&fm.metadata, "Machine").await? else {
        return wait(10);
    };
    let cluster = ctx.cluster_for(&fm.metadata).await?;
    if is_paused(&fm.metadata, cluster.as_ref()) {
        return Ok(Action::await_change());
    }
    let Some(cluster) = cluster else {
        return wait(10);
    };
    let infra_ready = cluster
        .data
        .pointer("/status/initialization/infrastructureProvisioned")
        .or_else(|| cluster.data.pointer("/status/infrastructureReady"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let id = vm_id(fm)?;
    let provider_id = format!("xdebbie://{id}");
    let prev = fm.status.clone().unwrap_or_default();
    let generation = fm.metadata.generation;

    let mut vm = ctx.xdebbie.get_vm(id).await?;
    if vm.is_none() {
        if !infra_ready {
            return wait(10);
        }
        let Some(secret) = machine
            .data
            .pointer("/spec/bootstrap/dataSecretName")
            .and_then(Value::as_str)
        else {
            set_status(
                api,
                fm,
                XDebbie8sMachineStatus {
                    conditions: vec![cond(
                        "Ready",
                        false,
                        "WaitingForBootstrapData",
                        None,
                        generation,
                        &prev.conditions,
                    )],
                    ..prev.clone()
                },
            )
            .await?;
            return wait(10);
        };
        let sec: Secret = Api::namespaced(ctx.client.clone(), &ns).get(secret).await?;
        let data = sec.data.unwrap_or_default();
        let value = data
            .get("value")
            .ok_or_else(|| anyhow!("bootstrap secret {secret} has no 'value'"))?;
        let user_data = String::from_utf8(value.0.clone())
            .map_err(anyhow::Error::from)?
            .replace(PROVIDER_ID_TOKEN, &provider_id);

        let cluster_name = cluster.name_any();
        let mlabels = machine.metadata.labels.clone().unwrap_or_default();
        let is_cp = mlabels.contains_key(CP_LABEL);
        let group = if is_cp {
            "control-plane".to_string()
        } else {
            mlabels
                .get(MD_LABEL)
                .or_else(|| mlabels.get(MS_LABEL))
                .cloned()
                .unwrap_or_else(|| "workers".into())
        };
        let spec = VmSpec {
            name: name.clone(),
            vcpus: fm.spec.vcpus,
            memory_mib: fm.spec.memory_mib,
            disk_gib: fm.spec.disk_gib,
            image: fm.spec.image.clone(),
            data_disk_gib: fm.spec.data_disk_gib,
            ip: None, // allocated by xdebbied from [network]
            storage: fm
                .spec
                .storage
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(|e: String| anyhow::anyhow!("{}: spec.storage: {e}", fm.name_any()))?
                .unwrap_or_default(),
            placement: Placement {
                failure_domain: machine
                    .data
                    .pointer("/spec/failureDomain")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(Into::into),
                anti_affinity_group: Some(format!("{ns}.{cluster_name}.{group}")),
                strict_anti_affinity: fm.spec.strict_anti_affinity,
                required_labels: fm.spec.host_selector.clone(),
                control_plane: is_cp,
                host: None,
            },
            user_data: Some(user_data),
            meta_data: [("cluster".to_string(), cluster_name.clone())].into(),
            labels: [
                (
                    "xdebbie.dev/cluster".to_string(),
                    format!(
                        "{ns}.{}",
                        fm.labels()
                            .get(CLUSTER_NAME)
                            .cloned()
                            .unwrap_or(cluster_name.clone())
                    ),
                ),
                ("xdebbie.dev/machine".to_string(), format!("{ns}.{name}")),
            ]
            .into(),
        };
        info!(machine = %name, %id, "creating vm");
        vm = Some(ctx.xdebbie.put_vm(id, &spec).await?);
    }
    let vm = vm.unwrap();

    if fm.spec.provider_id.as_deref() != Some(&provider_id) {
        api.patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(json!({ "spec": { "providerID": provider_id } })),
        )
        .await?;
    }

    let has_ip = vm.addresses.iter().any(|a| a.kind == "InternalIP");
    let provisioned = vm.phase == VmPhase::Running && has_ip;
    let fd = match &vm.host {
        Some(h) => ctx
            .xdebbie
            .failure_domains()
            .await?
            .into_iter()
            .find(|f| f.hosts.contains(h))
            .map(|f| f.name),
        None => None,
    };
    let (reason, msg) = match vm.phase {
        VmPhase::Running if has_ip => ("Provisioned", None),
        VmPhase::Running => (
            "WaitingForAddress",
            Some("guest agent / DHCP has not reported an address yet".into()),
        ),
        VmPhase::Failed => ("ProvisioningFailed", vm.message.clone()),
        p => (
            "Provisioning",
            vm.message.clone().or(Some(format!("{p:?}"))),
        ),
    };
    let status = XDebbie8sMachineStatus {
        // Once provisioned, stays provisioned (contract: never flip back).
        initialization: Some(Initialization {
            provisioned: Some(
                provisioned
                    || prev
                        .initialization
                        .as_ref()
                        .and_then(|i| i.provisioned)
                        .unwrap_or(false),
            ),
        }),
        addresses: vm
            .addresses
            .iter()
            .map(|a| MachineAddress {
                type_: a.kind.clone(),
                address: a.address.clone(),
            })
            .collect(),
        failure_domain: fd,
        host: vm.host.clone(),
        phase: Some(format!("{:?}", vm.phase)),
        conditions: vec![cond(
            "Ready",
            provisioned,
            reason,
            msg,
            generation,
            &prev.conditions,
        )],
    };
    set_status(api, fm, status).await?;
    wait(if provisioned { 60 } else { 10 })
}

async fn set_status(
    api: &Api<XDebbie8sMachine>,
    fm: &XDebbie8sMachine,
    s: XDebbie8sMachineStatus,
) -> Result<()> {
    if fm.status.as_ref() == Some(&s) {
        return Ok(());
    }
    api.patch_status(
        &fm.name_any(),
        &PatchParams::default(),
        &Patch::Merge(json!({ "status": s })),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crds_carry_contract_label() {
        for c in all_crds() {
            assert_eq!(
                c.metadata.labels.unwrap()["cluster.x-k8s.io/v1beta2"],
                "v1alpha1"
            );
        }
    }
    #[test]
    fn provider_id_wins_over_uid() {
        let mut fm = XDebbie8sMachine::new("m", XDebbie8sMachineSpec::default());
        fm.metadata.uid = Some("11111111-1111-1111-1111-111111111111".into());
        assert_eq!(
            vm_id(&fm).unwrap().to_string(),
            "11111111-1111-1111-1111-111111111111"
        );
        fm.spec.provider_id = Some("xdebbie://22222222-2222-2222-2222-222222222222".into());
        assert_eq!(
            vm_id(&fm).unwrap().to_string(),
            "22222222-2222-2222-2222-222222222222"
        );
    }
    #[test]
    fn condition_keeps_transition_time() {
        let a = cond("Ready", true, "x", None, None, &[]);
        let b = cond("Ready", true, "y", None, None, std::slice::from_ref(&a));
        assert_eq!(a.last_transition_time, b.last_transition_time);
    }
}
