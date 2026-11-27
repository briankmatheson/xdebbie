# capx and Cluster API

capx turns Cluster API Machines into xdebbie VMs: one VM per Machine,
placed by xdebbie's scheduler, bootstrapped by kubeadm.

capx implements the Cluster API contract **v1beta2** (CAPI v1.11 and
later). Its resources are in `infrastructure.cluster.x-k8s.io/v1alpha1`,
and its CRDs carry the label `cluster.x-k8s.io/v1beta2: v1alpha1`. The
walk-through is [A Kubernetes cluster with capx](../getting-started/kubernetes.md).

## What you need

- A management cluster with Cluster API core and the kubeadm providers.
- The credentials Secret from the walk-through (`url`, `token`, `ca`).
- A node image on every host ([Base images](../storage/local.md#base-images)).

## The resources

| Kind | Short name | What |
|---|---|---|
| `XDebbie8sCluster` | `fc` | the cluster's infrastructure: its API endpoint and failure domains |
| `XDebbie8sMachine` | `fm` | one VM |
| `XDebbie8sMachineTemplate` | `fmt` | the template KubeadmControlPlane and MachineDeployments stamp machines from |
| `XDebbie8sClusterTemplate` | | for ClusterClass |

`XDebbie8sCluster.spec`:

| Field | Meaning |
|---|---|
| `controlPlaneEndpoint {host, port}` | the API server's address: a kube-vip VIP or an outside load balancer. Required. |
| `failureDomains` | keep the cluster to these failure domains (default: all) |

`XDebbie8sMachine.spec` (in a template, under `spec.template.spec`):

| Field | Meaning |
|---|---|
| `vcpus`, `memoryMib`, `diskGib` | size |
| `image` | base image, on every host |
| `dataDiskGib` | optional second disk |
| `storage` | `local` (default) or `abe` |
| `hostSelector` | host labels the VM's host must carry |
| `strictAntiAffinity` | never two machines of one group on a host (default: soft spreading) |
| `providerID` | set by capx to `xdebbie://VM_UUID`; never set it yourself |

Each VM capx makes is named after its Machine, labelled
`xdebbie.dev/cluster=NAMESPACE.CLUSTER` and
`xdebbie.dev/machine=NAMESPACE.MACHINE`, in the anti-affinity group
`NAMESPACE.CLUSTER.GROUP` (the control plane, or the MachineDeployment),
and gets `cluster: CLUSTER` in its cloud-init meta-data.

## Running capx

```text
capx crds                     print the CRDs (YAML)
capx run [--url URL] [--token TOKEN] [--ca CA] [--capi-version V] [--namespace NS]
```

| Option | Environment | Default | Meaning |
|---|---|---|---|
| `--url` | `XDEBBIE_URL` | | a box's API |
| `--token` | `XDEBBIE_TOKEN` | | the cluster token |
| `--ca` | `XDEBBIE_CA` | | the cluster CA: a file path, or the PEM text itself (as it comes out of a Secret) |
| `--capi-version` | `CAPI_VERSION` | `v1beta2` | the served CAPI core version to read Clusters and Machines from |
| `--namespace` | `WATCH_NAMESPACE` | all | watch one namespace only |

capx needs to read and update its own resources, read Clusters, Machines
and bootstrap Secrets. This manifest runs it; set the image:

```yaml
apiVersion: v1
kind: ServiceAccount
metadata: {name: capx, namespace: capx-system}
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata: {name: capx}
rules:
  - apiGroups: [infrastructure.cluster.x-k8s.io]
    resources: [xdebbie8sclusters, xdebbie8smachines, xdebbie8sclustertemplates, xdebbie8smachinetemplates]
    verbs: [get, list, watch, patch, update]
  - apiGroups: [infrastructure.cluster.x-k8s.io]
    resources: [xdebbie8sclusters/status, xdebbie8smachines/status]
    verbs: [get, patch, update]
  - apiGroups: [cluster.x-k8s.io]
    resources: [clusters, machines]
    verbs: [get, list, watch]
  - apiGroups: [""]
    resources: [secrets]
    verbs: [get]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: capx-aggregate-to-capi
  labels: {cluster.x-k8s.io/aggregate-to-manager: "true"}
rules:
  - apiGroups: [infrastructure.cluster.x-k8s.io]
    resources: [xdebbie8sclusters, xdebbie8smachines, xdebbie8sclustertemplates, xdebbie8smachinetemplates]
    verbs: [get, list, watch, create, update, patch, delete]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata: {name: capx}
roleRef: {apiGroup: rbac.authorization.k8s.io, kind: ClusterRole, name: capx}
subjects: [{kind: ServiceAccount, name: capx, namespace: capx-system}]
---
apiVersion: apps/v1
kind: Deployment
metadata: {name: capx, namespace: capx-system}
spec:
  replicas: 1
  strategy: {type: Recreate}      # one writer: capx has no leader election
  selector: {matchLabels: {app: capx}}
  template:
    metadata: {labels: {app: capx}}
    spec:
      serviceAccountName: capx
      containers:
        - name: manager
          image: REGISTRY/capx:VERSION
          args: [run]
          env:
            - {name: RUST_LOG, value: "info,kube=warn"}
            - name: XDEBBIE_URL
              valueFrom: {secretKeyRef: {name: capx-xdebbie, key: url}}
            - name: XDEBBIE_TOKEN
              valueFrom: {secretKeyRef: {name: capx-xdebbie, key: token}}
            - name: XDEBBIE_CA
              valueFrom: {secretKeyRef: {name: capx-xdebbie, key: ca}}
```

`deploy/capf.yaml` and the `Dockerfile` in the tree still use the
provider's pre-release name; use the manifest above and the image steps in
the walk-through until they are fixed.

capx holds the cluster token and reads bootstrap Secrets: limit it to one
namespace with `WATCH_NAMESPACE` if you can. It looks again at a cluster
every 120 seconds once it is ready (15 before), at a machine every 60
seconds once provisioned (10 before), and 15 seconds after an error. Each
xdebbied call gets 30 seconds.

## What capx does for the contract

**XDebbie8sCluster (InfraCluster)**

| Requirement | capx |
|---|---|
| `spec.controlPlaneEndpoint` | yours to supply; missing means `Ready=False` with a reason |
| `status.initialization.provisioned` | `true` once the endpoint is set and a control-plane failure domain exists |
| `status.failureDomains` | from xdebbie's failure domains, filtered by `spec.failureDomains` |
| `status.conditions` | `Ready` |
| Owner reference, pause (`spec.paused`, `cluster.x-k8s.io/paused`) | waits for the owner; honours pause |
| Deletion | deletes VMs still labelled with the cluster, then lets go |
| ClusterClass | `XDebbie8sClusterTemplate` |

**XDebbie8sMachine (InfraMachine)**

| Requirement | capx |
|---|---|
| `spec.providerID` | `xdebbie://VM_UUID`, set as soon as the VM is created |
| Node to Machine matching | kubelet `--provider-id={{ ds.meta_data.provider_id }}`; capx puts the value into the bootstrap data, so no jinja is needed |
| `spec.failureDomain` from the Machine | a hard placement rule |
| `status.failureDomain` | the domain of the host chosen |
| `status.initialization.provisioned` | `true` when the VM is `Running` with an address; never flips back |
| `status.addresses` | `InternalIP` (guest agent, ARP or DHCP binding) and `Hostname` |
| Bootstrap data | waits for `Machine.spec.bootstrap.dataSecretName`, uses its `value` as user-data |
| Cluster infrastructure | waits for `Cluster.status.initialization.infrastructureProvisioned` |
| `status.conditions` | `Ready`, reasons `WaitingForBootstrapData`, `Provisioning`, `WaitingForAddress`, `ProvisioningFailed`, `Provisioned` |
| Deletion | deletes the VM; keeps the finalizer until xdebbie confirms it is gone |
| Templates | `XDebbie8sMachineTemplate` |

**What Cluster API relies on**

| Feature | With xdebbie |
|---|---|
| KubeadmControlPlane spreading | KCP picks a failure domain per replica and capx places there; soft anti-affinity spreads further, `strictAntiAffinity` makes it hard |
| MachineDeployment spreading | soft anti-affinity per MachineDeployment across hosts and domains |
| Rolling upgrades | create-before-delete; machines are immutable |
| MachineHealthCheck | deleting a Machine deletes its VM; the replacement goes through the scheduler |
| `clusterctl move` | the CRDs carry `clusterctl.cluster.x-k8s.io`; VM identity is in `providerID`, which survives the move |
| Autoscaler (clusterapi provider) | works on MachineDeployments; nothing provider-specific |

**Not done**

| Feature | Status |
|---|---|
| MachinePool | not implemented; use MachineDeployments |
| IPAM contract (`IPAddressClaim`) | not implemented; VMs use xdebbie's DHCP |
| Load balancer for the API endpoint | out of scope; use kube-vip |
| In-place updates, runtime hooks | not implemented |
| Leader election for capx | one replica, `Recreate` |

## If it goes wrong

- If the XDebbie8sCluster is not `Ready`, then `controlPlaneEndpoint` is
  missing, or no host is `control_plane_eligible`.
- If a Machine waits with `WaitingForBootstrapData`, then kubeadm has not
  made its Secret yet; look at the KubeadmConfig.
- If it waits with `WaitingForAddress`, then the VM runs but has no
  address yet: look at its console.

## Reference

[Scheduling](scheduling.md), [`/v1/vms`](../reference/api.md#vms),
[Security](security.md).
