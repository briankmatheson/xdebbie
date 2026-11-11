// SPDX-License-Identifier: GPL-3.0-or-later
//! Cluster API infrastructure types, contract v1beta2.
//!
//! | CAPI contract object   | XDebbie8s kind                |
//! |------------------------|---------------------------|
//! | InfraCluster           | XDebbie8sCluster              |
//! | InfraClusterTemplate   | XDebbie8sClusterTemplate      |
//! | InfraMachine           | XDebbie8sMachine              |
//! | InfraMachineTemplate   | XDebbie8sMachineTemplate      |

use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "v1alpha1";

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiEndpoint {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: i32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    #[serde(rename = "type")]
    pub type_: String,
    /// "True", "False" or "Unknown".
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub last_transition_time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Initialization {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioned: Option<bool>,
}

// ---------------- XDebbie8sCluster ----------------

#[derive(CustomResource, Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.cluster.x-k8s.io",
    version = "v1alpha1",
    kind = "XDebbie8sCluster",
    namespaced,
    status = "XDebbie8sClusterStatus",
    shortname = "fc",
    printcolumn = r#"{"name":"Cluster","type":"string","jsonPath":".metadata.labels['cluster\\.x-k8s\\.io/cluster-name']"}"#,
    printcolumn = r#"{"name":"Endpoint","type":"string","jsonPath":".spec.controlPlaneEndpoint.host"}"#,
    printcolumn = r#"{"name":"Provisioned","type":"boolean","jsonPath":".status.initialization.provisioned"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct XDebbie8sClusterSpec {
    /// The API server endpoint. XDebbie8s does not run load balancers; point this
    /// at a VIP managed by kube-vip (static pod on control-plane VMs) or an
    /// external LB. Required by the CAPI contract.
    #[serde(default)]
    pub control_plane_endpoint: ApiEndpoint,
    /// Restrict the cluster to these failure domains (default: all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_domains: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FailureDomain {
    pub name: String,
    #[serde(default)]
    pub control_plane: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct XDebbie8sClusterStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initialization: Option<Initialization>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_domains: Vec<FailureDomain>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

#[derive(CustomResource, Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.cluster.x-k8s.io",
    version = "v1alpha1",
    kind = "XDebbie8sClusterTemplate",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct XDebbie8sClusterTemplateSpec {
    pub template: XDebbie8sClusterTemplateResource,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct XDebbie8sClusterTemplateResource {
    pub spec: XDebbie8sClusterSpec,
}

// ---------------- XDebbie8sMachine ----------------

#[derive(CustomResource, Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.cluster.x-k8s.io",
    version = "v1alpha1",
    kind = "XDebbie8sMachine",
    namespaced,
    status = "XDebbie8sMachineStatus",
    shortname = "fm",
    printcolumn = r#"{"name":"Cluster","type":"string","jsonPath":".metadata.labels['cluster\\.x-k8s\\.io/cluster-name']"}"#,
    printcolumn = r#"{"name":"Host","type":"string","jsonPath":".status.host"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"ProviderID","type":"string","jsonPath":".spec.providerID"}"#,
    printcolumn = r#"{"name":"Provisioned","type":"boolean","jsonPath":".status.initialization.provisioned"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct XDebbie8sMachineSpec {
    /// Set by capx to `xdebbie://<vm-uuid>`. Never set this yourself.
    #[serde(
        default,
        rename = "providerID",
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_id: Option<String>,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub disk_gib: u64,
    /// Base image volume present in every host's storage pool.
    pub image: String,
    /// Optional extra raw disk attached as /dev/vdb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_disk_gib: Option<u64>,
    /// `local` (default): disks in the host's pool/LVM. `abe`: NVMe/TCP
    /// volumes from the host's ABE exporter, attachable from any host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    /// Host labels the VM's host must carry.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub host_selector: BTreeMap<String, String>,
    /// Never co-locate two machines of the same group (control plane or
    /// MachineDeployment) on one host. Default: soft spreading only.
    #[serde(default)]
    pub strict_anti_affinity: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MachineAddress {
    #[serde(rename = "type")]
    pub type_: String,
    pub address: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct XDebbie8sMachineStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initialization: Option<Initialization>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<MachineAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

#[derive(CustomResource, Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.cluster.x-k8s.io",
    version = "v1alpha1",
    kind = "XDebbie8sMachineTemplate",
    namespaced,
    shortname = "fmt"
)]
#[serde(rename_all = "camelCase")]
pub struct XDebbie8sMachineTemplateSpec {
    pub template: XDebbie8sMachineTemplateResource,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct XDebbie8sMachineTemplateResource {
    pub spec: XDebbie8sMachineSpec,
}

/// All CRDs, labelled so CAPI maps its contract to our served version.
pub fn all_crds()
-> Vec<k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition>
{
    use kube::CustomResourceExt;
    let mut v = vec![
        XDebbie8sCluster::crd(),
        XDebbie8sClusterTemplate::crd(),
        XDebbie8sMachine::crd(),
        XDebbie8sMachineTemplate::crd(),
    ];
    for c in &mut v {
        let l = c.metadata.labels.get_or_insert_with(Default::default);
        l.insert("cluster.x-k8s.io/v1beta2".into(), VERSION.into());
        l.insert(
            "cluster.x-k8s.io/provider".into(),
            "infrastructure-xdebbie".into(),
        );
        // Lets `clusterctl move` discover and relocate these objects.
        l.insert("clusterctl.cluster.x-k8s.io".into(), String::new());
    }
    v
}
