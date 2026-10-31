# Cluster API contract coverage

capq implements Cluster API **contract v1beta2** (CAPI v1.11 and later). Its
CRDs carry `cluster.x-k8s.io/v1beta2: v1alpha1`, mapping the contract to the
version Qfodufk8s serves.

## InfraCluster → `Qfodufk8sCluster`

| Contract requirement | capq |
|---|---|
| CRD contract label | `cluster.x-k8s.io/v1beta2: v1alpha1` |
| `spec.controlPlaneEndpoint {host, port}` | User-supplied VIP (kube-vip) or external LB. Missing → `Ready=False` with a reason |
| `status.initialization.provisioned` | `true` once endpoint is set and a control-plane failure domain exists |
| `status.failureDomains[] {name, controlPlane, attributes}` | Synced from qfodufk8sd hosts; optionally filtered by `spec.failureDomains` |
| `status.conditions` | `Ready`, with stable `lastTransitionTime` |
| Owner reference to Cluster | Waits for it before acting |
| Pause (`spec.paused` / `cluster.x-k8s.io/paused`) | Honored |
| Finalizer and deletion | Deletes any VMs still labelled with the cluster, then releases |
| InfraClusterTemplate (ClusterClass) | `Qfodufk8sClusterTemplate` |

## InfraMachine → `Qfodufk8sMachine`

| Contract requirement | capq |
|---|---|
| `spec.providerID` | `qfodufk8s://<vm-uuid>`; set as soon as the VM is created |
| Node ↔ Machine matching | kubelet `--provider-id={{ ds.meta_data.provider_id }}`; capq substitutes the value into bootstrap data, so jinja is not needed |
| `spec.failureDomain` from Machine | Hard placement constraint |
| `status.failureDomain` | The failure domain of the host actually chosen |
| `status.initialization.provisioned` | `true` when VM is Running with an InternalIP; never flips back |
| `status.addresses` | `InternalIP` from guest agent / ARP / lease, plus `Hostname` |
| Bootstrap data | Waits for `Machine.spec.bootstrap.dataSecretName`, reads `value` as cloud-init user-data |
| Waits for cluster infrastructure | Waits for `Cluster.status.initialization.infrastructureProvisioned` |
| `status.conditions` | `Ready` with reasons `WaitingForBootstrapData`, `Provisioning`, `WaitingForAddress`, `ProvisioningFailed`, `Provisioned` |
| Pause | Honored |
| Finalizer and deletion | Deletes the VM; keeps the finalizer until qfodufk8sd confirms it is gone |
| InfraMachineTemplate | `Qfodufk8sMachineTemplate` |

## Behaviours CAPI relies on

| Feature | How it works with Qfodufk8s |
|---|---|
| KubeadmControlPlane spreading | KCP picks a failure domain per replica; capq places there. Soft anti-affinity spreads further; `strictAntiAffinity` makes it hard |
| MachineDeployment spreading | Soft anti-affinity per MachineDeployment across hosts and failure domains |
| Rolling upgrades | Create-before-delete on immutable machines. Qfodufk8s has no in-place updates |
| MachineHealthCheck remediation | Machine deletion → VM deletion; replacement goes through the scheduler |
| `clusterctl move` | CRDs carry `clusterctl.cluster.x-k8s.io`; VM identity comes from `spec.providerID`, which survives the move; UIDs do not |
| ClusterClass / managed topologies | Template kinds provided |
| Autoscaler (clusterapi provider) | Works on MachineDeployments; no provider-specific code needed |

## Not yet implemented

| Optional feature | Status |
|---|---|
| MachinePool (`Qfodufk8sMachinePool`) | Not implemented; use MachineDeployments |
| IPAM contract (`IPAddressClaim`) | Not implemented; VMs use DHCP on the host bridge |
| Load balancer for the API endpoint | Out of scope by design; use kube-vip as in the example |
| In-place updates / runtime hooks | Not implemented |
| Leader election for capq | Single replica with `Recreate` strategy |
