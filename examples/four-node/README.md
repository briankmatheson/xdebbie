# Example: four physical servers, one Kubernetes cluster

Four Debian 13 servers become a Qfodufk8s cloud, then Cluster API builds a
cluster with 3 control-plane and 4 worker VMs on them.

## The layout

```
LAN 10.0.0.0/24   router DHCP .100–.199 (VMs)   VIP 10.0.0.50 (API, kube-vip)

 srv1 10.0.0.11  fd-1  runs qfodufk8sd     ─┐
 srv2 10.0.0.12  fd-2                   ├─ control-plane eligible
 srv3 10.0.0.13  fd-3                  ─┘
 srv4 10.0.0.14  fd-4  workers only

 After `kubectl apply -f cluster.yaml`:

   srv1: demo-cp-1   demo-workers-a
   srv2: demo-cp-2   demo-workers-b
   srv3: demo-cp-3   demo-workers-c
   srv4:             demo-workers-d
```

Each server is its own failure domain, so KCP puts one control plane on each
of srv1–srv3, and `strictAntiAffinity` guarantees it stays that way. Workers
spread across all four by soft anti-affinity.

Sizing below assumes servers with at least 16 threads and 64 GiB RAM.
Control planes are 2 vCPU / 4 GiB / 40 GiB; workers 4 vCPU / 12 GiB / 80 GiB.

## 1. Prepare the hosts

Give every server a stable name in DNS or `/etc/hosts` on all five machines
(the four servers and wherever you run `kubectl`). Then, on srv1, create
qfodufk8sd's identity:

```sh
sudo useradd --system --home-dir /var/lib/qfodufk8s --create-home qfodufk8s
sudo -u qfodufk8s ssh-keygen -t ed25519 -N '' -f /var/lib/qfodufk8s/.ssh/id_ed25519
```

On **each** server (console access recommended; the script rebuilds the
network as a bridge):

```sh
sudo ./prep-host.sh --nic eno1 --pubkey "$(ssh srv1 sudo cat /var/lib/qfodufk8s/.ssh/id_ed25519.pub)"
```

Back on srv1, accept every host key once:

```sh
for h in srv1 srv2 srv3 srv4; do sudo -u qfodufk8s ssh -o StrictHostKeyChecking=accept-new qfodufk8s@$h true; done
```

## 2. Build and distribute the node image

On any Debian box with `libguestfs-tools`:

```sh
./build-image.sh v1.36
./push-image.sh debian-13-k8s-v1.36.qcow2 srv1 srv2 srv3 srv4
```

## 3. Run qfodufk8sd on srv1

```sh
sudo apt install libvirt-clients cloud-image-utils
sudo install -m755 qfodufk8sd qfodufk8sctl /usr/local/bin/
sudo install -d -m750 -o qfodufk8s /etc/qfodufk8s /etc/qfodufk8s/tls
sudo install -m640 -o qfodufk8s qfodufk8sd.toml /etc/qfodufk8s/qfodufk8sd.toml
openssl rand -hex 32 | sudo tee /etc/qfodufk8s/token >/dev/null
# TLS: use your CA, or a self-signed cert for the lab
sudo openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 825 \
  -keyout /etc/qfodufk8s/tls/qfodufk8sd.key -out /etc/qfodufk8s/tls/qfodufk8sd.crt \
  -subj /CN=srv1 -addext "subjectAltName=DNS:srv1,IP:10.0.0.11"
sudo chown qfodufk8s /etc/qfodufk8s/tls/* /etc/qfodufk8s/token

sudo -u qfodufk8s qfodufk8sd -c /etc/qfodufk8s/qfodufk8sd.toml check
sudo install -m644 ../../deploy/systemd/qfodufk8sd.service /etc/systemd/system/
sudo systemctl enable --now qfodufk8sd
```

`check` should print four `ok` lines. Then:

```sh
export QFODUFK8S_URL=https://srv1:7443 QFODUFK8S_TOKEN=$(sudo cat /etc/qfodufk8s/token)
qfodufk8sctl host ls
qfodufk8sctl fd
```

> qfodufk8sctl uses the system trust store. For a self-signed lab cert, add
> `qfodufk8sd.crt` to `/usr/local/share/ca-certificates/` and run
> `update-ca-certificates`.

Smoke test without Kubernetes:

```sh
qfodufk8sctl vm create smoke --image debian-13-k8s-v1.36 --dry-run
qfodufk8sctl vm create smoke --image debian-13-k8s-v1.36
qfodufk8sctl vm ls             # Pending → Provisioning → Running, then an IP
qfodufk8sctl vm migrate smoke  # live-migrates to the next best host
qfodufk8sctl vm rm smoke
```

## 4. A management cluster

Bootstrap with kind on your workstation, then optionally pivot so the
cluster manages itself.

```sh
kind create cluster --name qfodufk8s-mgmt
clusterctl init --bootstrap kubeadm --control-plane kubeadm

docker build -t ghcr.io/bmatheson/capq:v0.1.0 ../..
kind load docker-image ghcr.io/bmatheson/capq:v0.1.0 --name qfodufk8s-mgmt

kubectl apply -f ../../deploy/crds.yaml -f ../../deploy/capq.yaml
kubectl -n capq-system create secret generic capq-qfodufk8s-credentials \
  --from-literal=url=https://10.0.0.11:7443 --from-literal=token="$QFODUFK8S_TOKEN"
kubectl -n capq-system rollout status deploy/capq-controller-manager
```

capq uses the container's trust store. With a self-signed qfodufk8sd
certificate, build the image with your CA added, or use a cert from a CA the
distroless base already trusts.

## 5. Create the cluster

Edit `cluster.yaml`: your SSH key, VIP, and `vip_interface` if your image
names the NIC differently. Then:

```sh
kubectl apply -f cluster.yaml
watch 'kubectl get cluster,kcp,md,machines,qfodufk8smachines; qfodufk8sctl vm ls'
```

Within a few minutes the first control plane is up behind 10.0.0.50:

```sh
clusterctl get kubeconfig demo > demo.kubeconfig
# install a CNI that uses pod CIDR 10.244.0.0/16, e.g. Cilium or Calico
kubectl --kubeconfig demo.kubeconfig get nodes -o wide
```

Nodes become `Ready` once the CNI is running. Each node's
`spec.providerID` is `qfodufk8s://<uuid>`, matching `qfodufk8sctl vm ls`.

## 6. Pivot (optional)

```sh
export KUBECONFIG=demo.kubeconfig
clusterctl init --bootstrap kubeadm --control-plane kubeadm
# load/push the capq image to a registry the cluster can pull from, then:
kubectl apply -f ../../deploy/crds.yaml -f ../../deploy/capq.yaml
kubectl -n capq-system create secret generic capq-qfodufk8s-credentials \
  --from-literal=url=https://10.0.0.11:7443 --from-literal=token="$QFODUFK8S_TOKEN"
unset KUBECONFIG
clusterctl move --to-kubeconfig demo.kubeconfig
kind delete cluster --name qfodufk8s-mgmt
```

## 7. Exercise lifecycle

```sh
# scale workers
kubectl --kubeconfig demo.kubeconfig scale md demo-workers --replicas 6

# take srv3 down for maintenance the Kubernetes way
kubectl --kubeconfig demo.kubeconfig drain <node-on-srv3> --ignore-daemonsets --delete-emptydir-data
qfodufk8sctl host cordon srv3
# ...work on srv3...
qfodufk8sctl host uncordon srv3

# kill a worker; the MachineHealthCheck replaces it in ~5 minutes
virsh -c qemu+ssh://qfodufk8s@srv4/system destroy <domain>

# upgrade: bake v1.37, add new templates, bump versions
./build-image.sh v1.37 && ./push-image.sh debian-13-k8s-v1.37.qcow2 srv1 srv2 srv3 srv4
```

## Tear down

```sh
kubectl delete cluster demo     # CAPI deletes machines; capq deletes VMs
qfodufk8sctl vm ls                  # empty
```
