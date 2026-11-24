# Run Kubernetes with Cluster API

Install capx, the Cluster API infrastructure provider, in a management
cluster, and let it build a Kubernetes cluster from XDebbie VMs.

capx implements the Cluster API **v1beta2** contract. What it does for each
part of the contract is in [Cluster API contract](../reference/capi-contract.md).

> **Status.** capx itself is current, but two files in the source tree
> still carry the provider's pre-release name: `Dockerfile` builds a binary
> called `capq`, and `deploy/capf.yaml` grants RBAC on `qfodufk8s*`
> resources and passes `QFODUFK8S_*` variables. Until they are fixed, build
> the image and write the deployment as shown below.

## What you need

- A management cluster with Cluster API core and the kubeadm providers
  (`clusterctl init --bootstrap kubeadm --control-plane kubeadm`).
- It can reach the xdebbied API (port 23443) of a box in your cluster.
- It can reach the new cluster's API endpoint, an address on the overlay.
  Run the management cluster on a box, in a VM on the overlay, or on a
  machine joined to the mesh with xdj (see
  [Connect a laptop with xdj](../how-to/mesh-clients.md)).
- A node image with kubeadm, kubelet and containerd on every host, named for
  example `debian-13-k8s-v1.36` (see [Create your first VM](first-vm.md#2-put-it-where-xdebbied-looks)).

## 1. Build the capx image

From the source tree:

```bash
cargo build --release -p capx
cat > Dockerfile.capx <<'EOF'
FROM gcr.io/distroless/cc-debian12:nonroot
COPY target/release/capx /capx
USER 65532:65532
ENTRYPOINT ["/capx"]
EOF
docker build -f Dockerfile.capx -t REGISTRY/capx:VERSION .
docker push REGISTRY/capx:VERSION
```

## 2. Install the CRDs

```bash
kubectl apply -f deploy/crds.yaml
```

`capx crds` prints the same manifests.

## 3. Give capx the cluster's credentials

capx needs the same three values as `xdctl`, from
`/etc/xdebbie/xdebbie_env` on any box:

```bash
kubectl create namespace capx-system
kubectl -n capx-system create secret generic capx-xdebbie \
  --from-literal=url="$XDEBBIE_URL" \
  --from-literal=token="$XDEBBIE_TOKEN" \
  --from-file=ca=/etc/xdebbie/api-ca.pem
```

`XDEBBIE_CA` may be a file path or the PEM text itself, so the Secret key
works as it is.

## 4. Run capx

capx needs to read and update its own resources, read Clusters, Machines
and bootstrap Secrets, and record events:

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
  - apiGroups: [""]
    resources: [events]
    verbs: [create, patch]
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
  strategy: {type: Recreate}      # one writer; capx has no leader election
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

Save it as `capx.yaml`, then:

```bash
kubectl apply -f capx.yaml
kubectl -n capx-system rollout status deploy/capx
```

`capx run` also takes `--capi-version` (`CAPI_VERSION`, default `v1beta2`)
and `--namespace` (`WATCH_NAMESPACE`, default all namespaces).

## 5. Create a cluster

A cluster is the usual Cluster API objects plus an `XDebbie8sCluster` and
`XDebbie8sMachineTemplate`s:

```yaml
apiVersion: infrastructure.cluster.x-k8s.io/v1alpha1
kind: XDebbie8sCluster
metadata: {name: demo, namespace: default}
spec:
  controlPlaneEndpoint: {host: 10.23.250.10, port: 6443}
---
apiVersion: infrastructure.cluster.x-k8s.io/v1alpha1
kind: XDebbie8sMachineTemplate
metadata: {name: demo-cp, namespace: default}
spec:
  template:
    spec:
      vcpus: 2
      memoryMib: 4096
      diskGib: 40
      image: debian-13-k8s-v1.36
      strictAntiAffinity: true    # never two control planes on one host
```

XDebbie runs no load balancer. Use kube-vip on the control planes for
`controlPlaneEndpoint`, with an overlay address that no box's VM pool uses
(each box allocates from its own `10.23.BOX.2-254`, so an address in an
unused `/24` is safe), or reserve one with `[network] reserved`.

A complete three-control-plane, four-worker example, with kube-vip and the
kubelet `provider-id` setting, is
[examples/four-node/cluster.yaml](https://github.com/briankmatheson/xdebbie/blob/main/examples/four-node/cluster.yaml).
Set the VIP, image name, SSH key and `vip_interface` before applying it.

```bash
kubectl apply -f cluster.yaml
kubectl get cluster,kcp,md,machines,xdebbie8smachines
xdctl vm ls
```

Each Machine's `spec.providerID` is `xdebbie://VM_UUID`, matching
`xdctl vm ls`.
