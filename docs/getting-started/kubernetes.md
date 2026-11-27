# A Kubernetes cluster with capx

Install capx in a management cluster and let Cluster API build a
Kubernetes cluster from xdebbie VMs.

capx is xdebbie's Cluster API infrastructure provider (contract
**v1beta2**). Each Machine becomes one VM; xdebbie places it, and kubeadm
does the rest.

## What you need

- A management cluster with Cluster API core and the kubeadm providers:
  `clusterctl init --bootstrap kubeadm --control-plane kubeadm`.
- From it, a route to a box's API (23443/tcp) and to the overlay, where the
  new cluster's API endpoint lives. Run it on a box, in a VM, or on a
  machine on the mesh ([A laptop on the mesh](laptop.md)).
- A node image with kubeadm, kubelet and containerd on every host, named
  for example `debian-13-k8s-v1.36` ([Base images](../storage/local.md#base-images)).
- Docker (or another image builder) and a registry the management cluster
  pulls from.

## Steps

1. Build the capx binary and image. The `Dockerfile` in the tree still
   builds the provider under its pre-release name, so use this one:

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

2. Install the CRDs (`capx crds` prints the same):

   ```bash
   kubectl apply -f deploy/crds.yaml
   ```

3. Give capx the cluster's credentials, the same three values `xdctl`
   uses, from `/etc/xdebbie/xdebbie_env` on a box:

   ```bash
   source /etc/xdebbie/xdebbie_env
   kubectl create namespace capx-system
   kubectl -n capx-system create secret generic capx-xdebbie \
     --from-literal=url="$XDEBBIE_URL" \
     --from-literal=token="$XDEBBIE_TOKEN" \
     --from-file=ca=/etc/xdebbie/api-ca.pem
   ```

4. Save the manifest in [Running capx](../compute/kubernetes.md#running-capx)
   as `capx.yaml`, set its image, and apply it:

   ```bash
   kubectl apply -f capx.yaml
   kubectl -n capx-system rollout status deploy/capx
   ```

5. Write the cluster. Start from
   [examples/four-node/cluster.yaml](https://github.com/briankmatheson/xdebbie/blob/main/examples/four-node/cluster.yaml)
   (three control planes, four workers, kube-vip). Set the VIP, the image
   name, your SSH key and `vip_interface`, then:

   ```bash
   kubectl apply -f cluster.yaml
   ```

6. Watch it come up:

   ```bash
   kubectl get cluster,kcp,md,machines,xdebbie8smachines
   xdctl vm ls
   ```

## What you should see

One VM per Machine in `xdctl vm ls`. Each Machine's `spec.providerID` is
`xdebbie://VM_UUID`, the same id `xdctl vm ls` shows. The XDebbie8sMachine
turns `Provisioned` once its VM is `Running` with an address.

xdebbie runs no load balancer. The example puts kube-vip on the control
planes, on an overlay address no box hands out. Each box allocates from its
own `10.23.BOX.2-254`, so an address in an unused `/24` is safe, or name it
in DNS and [reserve it](../network/addresses-and-dhcp.md#reserved-addresses).

## If it goes wrong

- If capx logs `401`, then the token in the Secret is old: it was rotated
  ([Tokens](../compute/security.md#rotate-the-token)).
- If Machines stay `Provisioning` and the VMs stay `Pending`, then
  `xdctl vm describe VM` says which host failed which filter.
- If the VMs run but nodes never join, then the image is missing kubeadm,
  or the kubelet's `--provider-id` is not set from
  `{{ ds.meta_data.provider_id }}` ([capx and Cluster API](../compute/kubernetes.md)).

## Reference

[capx and Cluster API](../compute/kubernetes.md) (resources, fields,
contract coverage), [Scheduling](../compute/scheduling.md) (how control
planes are spread).
