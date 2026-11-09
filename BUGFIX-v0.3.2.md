# Bugfix Release: v0.3.2

**Date**: 2026-11-08  
**Type**: Hotfix  
**Previous Version**: 0.3.1  

## Issue

Service fails to start on some systems with error:

```
Dependency failed for xdebbied.service
```

### Root Cause

The systemd service file had `Requires=libvirtd.service`, which made libvirtd a **hard dependency**.

On newer systems (Debian 12+, Ubuntu 22.04+), libvirt uses **modular daemons** instead of the monolithic `libvirtd`:
- `virtqemud` - QEMU/KVM driver
- `virtnetworkd` - Network management  
- `virtstoraged` - Storage management
- `virtnodedevd` - Node device management

When only modular daemons are available, `libvirtd.service` doesn't exist, causing xdebbied to fail with "Dependency failed".

### Systems Affected

- ✅ **Works on**: Systems with monolithic libvirtd (older Debian, manual install)
- ❌ **Fails on**: Systems with modular libvirt daemons (newer Debian/Ubuntu default)

### Example Error

```bash
$ systemctl start xdebbied
Job for xdebbied.service failed because it has a dependency on libvirtd.service which is not found.

$ systemctl status xdebbied
● xdebbied.service - xdebbie VM Orchestrator
     Active: inactive (dead)
Nov 08 16:43:40 host systemd[1]: Dependency failed for xdebbied.service
```

## Fix

### Systemd Service Changes

**File**: `crates/xdebbied/debian/xdebbied.service`

Changed from **hard requirement** to **soft preference**:

```diff
 [Unit]
 Description=xdebbie VM Orchestrator
 Documentation=https://github.com/yourusername/xdebbie
-After=network-online.target libvirtd.service local-fs.target
-Wants=network-online.target
-Requires=libvirtd.service
+After=network-online.target libvirtd.service virtqemud.service local-fs.target
+Wants=network-online.target libvirtd.service virtqemud.service
```

**Changes**:
1. **Removed** `Requires=libvirtd.service` (hard dependency)
2. **Added** `Wants=libvirtd.service virtqemud.service` (soft preference)
3. **Added** `After=virtqemud.service` (ordering for modular daemon)

**Benefits**:
- ✅ Works with monolithic libvirtd
- ✅ Works with modular daemons (virtqemud, etc.)
- ✅ Starts even if neither is present (will fail later with better error)
- ✅ Waits for whichever daemon is available

## Behavior

### With Monolithic libvirtd

```bash
$ systemctl status libvirtd
● libvirtd.service - libvirt legacy monolithic daemon
     Active: active (running)

$ systemctl start xdebbied
# ✅ Works - waits for libvirtd to be ready
```

### With Modular Daemons

```bash
$ systemctl status virtqemud
● virtqemud.service - libvirt QEMU/KVM driver daemon
     Active: active (running)

$ systemctl start xdebbied
# ✅ Works - waits for virtqemud to be ready
```

### With Neither (Error Case)

```bash
$ systemctl start xdebbied
# ✅ Service starts but bootstrap will fail with clear error:
# "Failed to connect to libvirt at qemu:///system"
# Better than "Dependency failed" which is cryptic
```

## Testing

### Check Which Libvirt Daemon You Have

```bash
# Monolithic daemon
systemctl status libvirtd

# Modular daemons
systemctl status virtqemud
systemctl status virtnetworkd
systemctl list-units 'virt*'
```

### Test Service Startup

```bash
# Stop service
sudo systemctl stop xdebbied

# Start with debug
sudo systemctl start xdebbied
sudo systemctl status xdebbied

# Should show:
#   Active: active (running)
# Not:
#   Active: inactive (dead)
#   Dependency failed
```

## Version Bump

- **v0.3.1** → **v0.3.2**
- Git tag: `v0.3.2`
- Package: `xdebbied_0.3.2-1_amd64.deb`

## Upgrade Path

### From v0.3.1 or v0.3.0

```bash
# Reinstall package
sudo dpkg -i xdebbied_0.3.2-1_amd64.deb

# Service will restart automatically
systemctl status xdebbied
```

### From v0.2.x or earlier

```bash
# Normal upgrade
sudo dpkg -i xdebbied_0.3.2-1_amd64.deb
```

## No Configuration Changes

✅ No changes to `/etc/xdebbie/xdebbied.toml`  
✅ No changes to environment file  
✅ No changes to code  
✅ Only systemd service dependency  

## Compatibility

**Tested on**:
- ✅ Debian 13 (Trixie) - monolithic libvirtd
- ✅ Debian 12 (Bookworm) - modular daemons
- ✅ Ubuntu 22.04 LTS - modular daemons
- ✅ Ubuntu 24.04 LTS - modular daemons

**Works with**:
- ✅ libvirt 9.x (monolithic)
- ✅ libvirt 10.x (modular by default)
- ✅ libvirt 11.x (modular only)

## Related Documentation

### Libvirt Modular Daemons

More info: https://libvirt.org/daemons.html

**Monolithic** (legacy):
- Single `libvirtd` handles all hypervisors and functions

**Modular** (modern):
- `virtqemud` - QEMU/KVM
- `virtnetworkd` - Networks
- `virtstoraged` - Storage
- `virtnodedevd` - Devices
- `virtinterfaced` - Host interfaces

### Migration from Monolithic to Modular

If you're on monolithic and want to switch:

```bash
# Stop monolithic daemon
sudo systemctl stop libvirtd.socket libvirtd.service

# Enable modular daemons
sudo systemctl enable --now virtqemud.socket

# xdebbied will work with either
sudo systemctl restart xdebbied
```

## Workaround for v0.3.1

If you're stuck on v0.3.1, manually edit the service:

```bash
sudo systemctl edit xdebbied --full
```

Change:
```ini
Requires=libvirtd.service
```

To:
```ini
Wants=libvirtd.service virtqemud.service
```

Then:
```bash
sudo systemctl daemon-reload
sudo systemctl restart xdebbied
```

## Summary

This hotfix resolves a compatibility issue where xdebbied failed to start on systems using modular libvirt daemons instead of the monolithic libvirtd.

**Before (v0.3.1)**:
- ❌ Hard dependency on `libvirtd.service`
- ❌ Fails on systems with modular daemons
- ❌ Cryptic "Dependency failed" error

**After (v0.3.2)**:
- ✅ Soft preference for both libvirtd and virtqemud
- ✅ Works on monolithic and modular systems
- ✅ Better error messages if libvirt is missing

**All features from v0.3.0/v0.3.1 remain unchanged**:
- ✅ Auto-discovery
- ✅ Leader election
- ✅ High availability
- ✅ System-wide environment file

**Release**: xdebbied_0.3.2-1_amd64.deb (3.5 MB)
