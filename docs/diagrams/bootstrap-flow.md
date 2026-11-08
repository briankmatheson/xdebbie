# xdebbied Bootstrap Flow Visualization

## Complete Bootstrap Process

```
╔════════════════════════════════════════════════════════════════════╗
║                   xdebbied bootstrap                             ║
╚════════════════════════════════════════════════════════════════════╝
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │  Step 1: Check Dependencies                 │
        │  ────────────────────────────────           │
        │  deps::check_dependencies()                 │
        │                                             │
        │  ┌─────────────────────────────────┐        │
        │  │ Check for commands:             │        │
        │  │  • virsh                        │        │
        │  │  • qemu-system-x86_64          │        │
        │  │  • qemu-img                     │        │
        │  │  • cloud-localds                │        │
        │  └─────────────────────────────────┘        │
        │                 │                           │
        │                 ▼                           │
        │  ┌─────────────────────────────────┐        │
        │  │ Check packages:                 │        │
        │  │  • libvirt-daemon-system        │        │
        │  │  • libvirt-clients              │        │
        │  │  • qemu-kvm                     │        │
        │  │  • qemu-utils                   │        │
        │  │  • cloud-image-utils            │        │
        │  └─────────────────────────────────┘        │
        │                 │                           │
        │                 ▼                           │
        │      Missing packages?                      │
        │           │         │                       │
        │         Yes        No                       │
        │           │         │                       │
        │           ▼         ▼                       │
        │    ┌──────────┐  ┌─────────┐               │
        │    │ Running  │  │ Continue│               │
        │    │ as root? │  └─────────┘               │
        │    └──────────┘                             │
        │      │      │                               │
        │     Yes    No                               │
        │      │      │                               │
        │      ▼      ▼                               │
        │   Install  Error                            │
        │    via     with                             │
        │  apt-get   list                             │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │  Step 2: Ensure libvirtd Running            │
        │  ───────────────────────────────            │
        │  deps::ensure_libvirtd_running()            │
        │                                             │
        │  ┌─────────────────────────────────┐        │
        │  │ systemctl is-active libvirtd    │        │
        │  └─────────────────────────────────┘        │
        │                 │                           │
        │                 ▼                           │
        │          Is active?                         │
        │           │         │                       │
        │          Yes       No                       │
        │           │         │                       │
        │           ▼         ▼                       │
        │      Continue   Start it                    │
        │                 systemctl start             │
        │                 systemctl enable            │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │  Step 3: Discover Resources                 │
        │  ──────────────────────────                 │
        │  libvirt_setup::discover_libvirt()          │
        │                                             │
        │  ┌─────────────────────────────────┐        │
        │  │ Discover Networks               │        │
        │  │  virsh net-list --all --name    │        │
        │  │  ┌─────────────────────┐        │        │
        │  │  │ For each network:   │        │        │
        │  │  │  • Get bridge name  │        │        │
        │  │  │  • Get subnet       │        │        │
        │  │  └─────────────────────┘        │        │
        │  └─────────────────────────────────┘        │
        │                 │                           │
        │                 ▼                           │
        │  ┌─────────────────────────────────┐        │
        │  │ Discover Storage Pools          │        │
        │  │  virsh pool-list --all --name   │        │
        │  │  ┌─────────────────────┐        │        │
        │  │  │ For each pool:      │        │        │
        │  │  │  • Get path         │        │        │
        │  │  │  • Get type         │        │        │
        │  │  └─────────────────────┘        │        │
        │  └─────────────────────────────────┘        │
        │                                             │
        │  Result: LibvirtResources {                 │
        │    networks: Vec<NetworkInfo>,              │
        │    pools: Vec<StoragePoolInfo>              │
        │  }                                          │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │  Step 4: Create Resources (if needed)       │
        │  ────────────────────────────────           │
        │                                             │
        │  ┌─────────────────────────────────┐        │
        │  │ Network Creation                │        │
        │  │  Name: xdebbie exists?        │        │
        │  │    │           │                │        │
        │  │   Yes         No                │        │
        │  │    │           │                │        │
        │  │   Skip         ▼                │        │
        │  │          Create network:        │        │
        │  │          • Bridge: virbr-*      │        │
        │  │          • Subnet: CIDR         │        │
        │  │          • DHCP range           │        │
        │  │          • virsh net-define     │        │
        │  │          • virsh net-start      │        │
        │  │          • virsh net-autostart  │        │
        │  └─────────────────────────────────┘        │
        │                 │                           │
        │                 ▼                           │
        │  ┌─────────────────────────────────┐        │
        │  │ Storage Pool Creation           │        │
        │  │  Name: xdebbie exists?        │        │
        │  │    │           │                │        │
        │  │   Yes         No                │        │
        │  │    │           │                │        │
        │  │   Skip         ▼                │        │
        │  │          Create pool:           │        │
        │  │          • mkdir path           │        │
        │  │          • virsh pool-define    │        │
        │  │          • virsh pool-start     │        │
        │  │          • virsh pool-autostart │        │
        │  └─────────────────────────────────┘        │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │  Step 5: Generate Configuration             │
        │  ──────────────────────────────             │
        │  config_gen::generate_config()              │
        │                                             │
        │  Config exists && !force?                   │
        │           │           │                     │
        │          Yes         No                     │
        │           │           │                     │
        │          Skip         ▼                     │
        │                Generate TOML:               │
        │                • listen = "0.0.0.0:7443"    │
        │                • state_file = "..."         │
        │                • token_file = "..."         │
        │                • [[hosts]]                  │
        │                  - name = hostname()        │
        │                  - uri = qemu:///system     │
        │                  - bridge = discovered      │
        │                  - storage_pool = discovered│
        │                  - overcommit settings      │
        │                                             │
        │  Write to /etc/xdebbie/xdebbied.toml    │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │  Step 6: Generate Token                     │
        │  ──────────────────────                     │
        │  token::generate_token()                    │
        │                                             │
        │  Token exists && !force?                    │
        │           │           │                     │
        │          Yes         No                     │
        │           │           │                     │
        │         Reuse         ▼                     │
        │                Generate token:              │
        │                • rand(32 bytes)             │
        │                • hex encode                 │
        │                • 64 characters              │
        │                                             │
        │  Write to /etc/xdebbie/token (0600)       │
        │                                             │
        │  Generate ~/.xdebbie_env:                 │
        │  • export XDEBBIE_URL=http://...          │
        │  • export XDEBBIE_TOKEN='...'             │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ╔════════════════════════════════════════════╗
        ║          Bootstrap Complete!               ║
        ║                                            ║
        ║  Created:                                  ║
        ║   ✓ /etc/xdebbie/xdebbied.toml         ║
        ║   ✓ /etc/xdebbie/token                   ║
        ║   ✓ ~/.xdebbie_env                       ║
        ║   ✓ Network: xdebbie                     ║
        ║   ✓ Pool: xdebbie                        ║
        ║                                            ║
        ║  Next: sudo xdebbied serve               ║
        ╚════════════════════════════════════════════╝
```

## Serve with Bootstrap Flow

```
╔════════════════════════════════════════════════════════════════════╗
║               xdebbied serve --bootstrap                         ║
╚════════════════════════════════════════════════════════════════════╝
                              │
                              ▼
                    ┌──────────────────┐
                    │  Run Bootstrap   │
                    │  (6 steps above) │
                    └──────────────────┘
                              │
                              ├─── Success ────┐
                              │                │
                    Failure (logged)           │
                              │                │
                          Continue             │
                              │                │
                              └────────┬───────┘
                                       │
                                       ▼
                    ┌────────────────────────────────┐
                    │  Load Configuration            │
                    │  /etc/xdebbie/xdebbied.toml│
                    └────────────────────────────────┘
                                       │
                                       ▼
                    ┌────────────────────────────────┐
                    │  Initialize Hypervisor         │
                    │  (Virsh wrapper)               │
                    └────────────────────────────────┘
                                       │
                                       ▼
                    ┌────────────────────────────────┐
                    │  Open State Store              │
                    │  /var/lib/xdebbie/state.json │
                    └────────────────────────────────┘
                                       │
                                       ▼
                    ┌────────────────────────────────┐
                    │  Load Token                    │
                    │  /etc/xdebbie/token          │
                    └────────────────────────────────┘
                                       │
                                       ▼
                    ┌────────────────────────────────┐
                    │  Start Reconciler              │
                    │  (Background task)             │
                    │   • Poll hosts every 5s        │
                    │   • Update capacity            │
                    │   • Reconcile VM state         │
                    └────────────────────────────────┘
                                       │
                                       ▼
                    ┌────────────────────────────────┐
                    │  Start API Server              │
                    │  Listen: 0.0.0.0:7443          │
                    │  Auth: Bearer token            │
                    │                                │
                    │  Routes:                       │
                    │   POST   /hosts/:name          │
                    │   GET    /hosts                │
                    │   DELETE /hosts/:name          │
                    │   PATCH  /hosts/:name          │
                    │   POST   /vms                  │
                    │   GET    /vms                  │
                    │   GET    /vms/:name            │
                    │   DELETE /vms/:name            │
                    └────────────────────────────────┘
                                       │
                                       ▼
                    ╔════════════════════════════════╗
                    ║   Server Running!              ║
                    ║                                ║
                    ║   Listening on 0.0.0.0:7443    ║
                    ║   Token auth enabled           ║
                    ║   Reconciler active            ║
                    ╚════════════════════════════════╝
```

## Client Connection Flow

```
╔════════════════════════════════════════════════════════════════════╗
║                    xdctl usage                              ║
╚════════════════════════════════════════════════════════════════════╝
                              │
                              ▼
                    ┌────────────────────┐
                    │ source             │
                    │ ~/.xdebbie_env   │
                    └────────────────────┘
                              │
                              ▼
                    ┌────────────────────────────────┐
                    │ Environment Variables Set:     │
                    │  XDEBBIE_URL=http://...:7443 │
                    │  XDEBBIE_TOKEN='...'         │
                    └────────────────────────────────┘
                              │
                              ▼
                    ┌────────────────────┐
                    │ xdctl       │
                    │ host ls            │
                    └────────────────────┘
                              │
                              ▼
                    ┌────────────────────────────────┐
                    │ HTTP Request:                  │
                    │  GET /hosts                    │
                    │  Authorization: Bearer <token> │
                    └────────────────────────────────┘
                              │
                              ▼
                    ┌────────────────────┐
                    │ xdebbied         │
                    │ validates token    │
                    └────────────────────┘
                              │
                              ▼
                    ┌────────────────────┐
                    │ Returns host list  │
                    │ with capacity      │
                    └────────────────────┘
```

## CAPX Integration Flow

```
╔════════════════════════════════════════════════════════════════════╗
║            Cluster API Provider XDebbie8s (CAPX)                   ║
╚════════════════════════════════════════════════════════════════════╝
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │ Management Cluster                          │
        │  • Cluster API v1.14.3                      │
        │  • CAPX controller                          │
        │  • cert-manager                             │
        │  • step-ca                                  │
        │  • Cilium CNI                               │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │ User creates XDebbieCluster:                │
        │                                             │
        │  apiVersion: infrastructure...v1alpha1      │
        │  kind: XDebbieCluster                       │
        │  metadata:                                  │
        │    name: workload1                          │
        │  spec:                                      │
        │    controlPlaneEndpoint:                    │
        │      host: 192.168.100.10                   │
        │      port: 6443                             │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │ CAPX Controller Watches                     │
        │  • XDebbieCluster                           │
        │  • XDebbieMachine                           │
        │  • Machine                                  │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │ For each XDebbieMachine:                    │
        │                                             │
        │  1. Generate cloud-init user-data           │
        │  2. Create VM via xdebbied API:           │
        │                                             │
        │     POST /vms                               │
        │     {                                       │
        │       "name": "workload1-cp-abc123",        │
        │       "cpus": 2,                            │
        │       "memory_mib": 4096,                   │
        │       "disk_gib": 40,                       │
        │       "image": "debian-13-k8s-v1.36",       │
        │       "user_data": "...",                   │
        │       "network_data": "..."                 │
        │     }                                       │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │ xdebbied Receives Request                 │
        │                                             │
        │  1. Validate token                          │
        │  2. Select host (spread/binpack)            │
        │  3. Create VM:                              │
        │     • Clone base image                      │
        │     • Generate cloud-init seed              │
        │     • Attach to network                     │
        │     • Start VM                              │
        │  4. Store state                             │
        │  5. Return VM info                          │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │ VM Boots                                    │
        │                                             │
        │  1. cloud-init runs                         │
        │  2. Configures networking                   │
        │  3. Installs Kubernetes                     │
        │  4. Joins cluster (kubeadm)                 │
        └─────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────┐
        │ Workload Cluster Ready                      │
        │                                             │
        │  Control plane: 3 VMs                       │
        │  Workers: N VMs                             │
        │  All managed by xdebbied                  │
        └─────────────────────────────────────────────┘
```

## Complete System Architecture

```
                    ┌──────────────────────────────────┐
                    │  User / Developer                │
                    └────────────┬─────────────────────┘
                                 │
                    ┌────────────┴─────────────┐
                    │                          │
                    ▼                          ▼
        ┌────────────────────┐    ┌────────────────────┐
        │  xdctl      │    │  kubectl           │
        │  (CLI client)      │    │  (CAPI resources)  │
        └─────────┬──────────┘    └─────────┬──────────┘
                  │                         │
                  │ Bearer                  │
                  │ Token                   │
                  │                         │
                  ▼                         ▼
        ┌────────────────────┐    ┌────────────────────┐
        │  xdebbied        │◄───│  CAPX Controller   │
        │  API Server        │    │  (in mgmt cluster) │
        │  :7443             │    └────────────────────┘
        └─────────┬──────────┘
                  │
                  │ libvirt API
                  │
                  ▼
        ┌────────────────────┐
        │  libvirtd          │
        │  qemu:///system    │
        └─────────┬──────────┘
                  │
                  │ Manages
                  │
                  ▼
        ┌─────────────────────────────────────┐
        │  Virtual Machines                   │
        │  ┌──────────┐ ┌──────────┐          │
        │  │ node0    │ │workload1 │  ...     │
        │  │ mgmt     │ │ cluster  │          │
        │  │ cluster  │ │ VMs      │          │
        │  └──────────┘ └──────────┘          │
        │                                     │
        │  Network: virbr-xdebbie           │
        │  Pool: /var/lib/libvirt/xdebbie   │
        └─────────────────────────────────────┘
```

This visualization shows the complete flow from bootstrap through CAPX integration, demonstrating how the self-bootstrapping xdebbied enables seamless Kubernetes cluster provisioning on bare metal.
