# XDebbie Data Disk Feature

## Overview
Added support for optional data disks to VMs, primarily for ABE (Abe Block Exporter) storage workloads.

## Changes Made

### 1. VmSpec Extension (`xdebbie-core/src/model.rs`)
Added new optional field:
```rust
pub data_disk_gib: Option<u64>
```
When set, creates a raw volume attached as `/dev/vdb` to the VM.

### 2. Libvirt Integration (`xdebbied/src/libvirt.rs`)

#### New Functions:
- `data_vol(vm)` - Returns volume name for data disk
- `ensure_data_disk()` - Creates raw volume if `data_disk_gib` is set

#### Modified Functions:
- `provision()` - Calls `ensure_data_disk()` after root disk and seed
- `migrate()` - Ensures data disk on destination host
- `domain_xml()` - Adds `<disk>` element for `/dev/vdb` when data disk present

## Usage

### Create VM with Data Disk via API:
```json
{
  "name": "abe-server",
  "vcpus": 2,
  "memoryMib": 4096,
  "diskGib": 20,
  "image": "debian-12-base",
  "dataDiskGib": 100
}
```

### Via CAPX (XDebbie8sMachineTemplate):
```yaml
apiVersion: infrastructure.cluster.x-k8s.io/v1alpha1
kind: XDebbie8sMachineTemplate
metadata:
  name: abe-server-mt
spec:
  template:
    spec:
      memoryMib: 4096
      vcpus: 2
      diskGib: 20
      dataDiskGib: 100
      image: debian-12-base
```

## ABE Integration Roadmap

### Completed:
- ✅ Data disk creation
- ✅ Automatic attachment as `/dev/vdb`
- ✅ Support in VM provisioning and migration

### Next Steps:

1. **ABE Cloud-Init Integration**
   - Create ABE-specific cloud-init userdata template
   - Auto-configure LVM on `/dev/vdb`
   - Install and start ABE service

2. **ABE VM Template**
   - Pre-built Debian image with ABE binary
   - Systemd service for auto-start
   - LVM tools pre-installed

3. **xdctl ABE Commands**
   - `xdctl abe create --data-disk 100G`
   - `xdctl abe ls`
   - `xdctl abe volume create <size>`

4. **Discovery Integration**
   - ABE VMs announce themselves via multicast
   - CSI driver discovers ABE servers automatically
   - Load balancing across multiple ABE instances

## Example: Manual ABE Setup on Data Disk

```bash
# Inside VM with data disk
sudo pvcreate /dev/vdb
sudo vgcreate abe /dev/vdb
sudo systemctl enable --now abe

# ABE will create LVs in the 'abe' volume group
# and export them via NVMe-oF
```

## Testing

Build and install:
```bash
cd ~/src/xdebbie
cargo deb -p xdebbied --no-strip
sudo dpkg -i target/debian/xdebbied_0.3.3-1_amd64.deb
```

Create test VM with data disk:
```bash
curl -X POST http://localhost:7443/vms \
  -H "Authorization: Bearer $(sudo cat /etc/xdebbie/token)" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "abe-test",
    "vcpus": 2,
    "memoryMib": 2048,
    "diskGib": 20,
    "dataDiskGib": 50,
    "image": "debian-12-base"
  }'
```

Verify:
```bash
xdctl vm ls
virsh -c qemu:///system domblklist xdebbie-abe-test-<uuid>
```

Should show:
- vda: root disk (qcow2)
- vdb: data disk (raw)
- sda: cloud-init seed (iso)
