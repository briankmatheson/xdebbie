# XDebbie OpenSSH Server Dependency

## Change

Added `openssh-server` to the package dependencies in `crates/xdebbied/Cargo.toml`.

## Why

XDebbie supports multi-host clustering using `qemu+ssh://` URIs to connect to remote libvirt instances. This requires SSH to be available on all hosts.

### Example Configuration

```toml
[[hosts]]
name = "gram"
uri = "qemu+ssh://gram/system"  # Requires SSH access
```

Without `openssh-server` installed, remote hosts cannot be reached, and peer hosts show as "Unknown" with 0 capacity in `xdctl host ls`.

## Before This Change

Users had to manually install openssh-server:
```bash
sudo apt-get install openssh-server
```

And then manually accept SSH host keys:
```bash
ssh-keyscan -H <peer-ip> >> ~/.ssh/known_hosts
```

## After This Change

Installing the xdebbied package automatically installs openssh-server:
```bash
sudo dpkg -i xdebbied_*.deb
# openssh-server is now installed and enabled
```

## SSH Key Management

The package does NOT automatically:
- Generate SSH keys (users should do: `ssh-keygen` if needed)
- Accept host keys (users must still SSH to each peer once)
- Configure passwordless SSH (users should use ssh-copy-id or authorized_keys)

### Recommended Setup for Multi-Host

On each host:

1. **Generate SSH key** (if not already present):
   ```bash
   ssh-keygen -t ed25519 -N "" -f ~/.root/.ssh/id_ed25519
   ```

2. **Copy key to other hosts**:
   ```bash
   ssh-copy-id peer-host
   ```

3. **Accept host keys** (one time):
   ```bash
   ssh -o StrictHostKeyChecking=accept-new peer-host hostname
   ```

4. **Test libvirt SSH**:
   ```bash
   virsh -c qemu+ssh://peer-host/system list
   ```

## Alternative: Use IP Addresses

If DNS resolution is problematic, use IP addresses in URIs:
```toml
[[hosts]]
name = "gram"
uri = "qemu+ssh://192.168.1.162/system"
```

## Security Considerations

XDebbie's default configuration assumes:
- Hosts are on a trusted LAN
- SSH access is appropriately secured (keys, not passwords)
- Firewall rules limit SSH to LAN (port 22)

For production deployments (Aviary in a Box):
- Use Flock for enrollment and certificate-based trust
- Consider using SSH certificates instead of key-based auth
- Implement `sshd_config` restrictions (e.g., `AllowUsers`, `Match Address`)

## Related Issues

This dependency resolves issues where:
- `xdctl host ls` shows peers as "Unknown"
- SSH connection errors like "Connection refused" on port 22
- Multi-host features don't work out of the box

## Testing

After rebuilding the package with this change:

```bash
# Build new package
cd ~/src/xdebbie
./scripts/build-deb.sh

# Install on both hosts
sudo dpkg -i target/debian/xdebbied_*.deb

# Verify SSH is running
systemctl status ssh

# Test multi-host
virsh -c qemu+ssh://peer/system list
```

## Documentation Updates

The following docs should be updated to mention SSH:
- ✅ `crates/xdebbied/Cargo.toml` - dependency added
- ⏳ `BOOTSTRAP.md` - mention SSH requirement for multi-host
- ⏳ `DISCOVERY.md` - explain qemu+ssh:// URIs
- ⏳ `README.md` - multi-host quick start

## Git Commit Message

```
feat(pkg): add openssh-server dependency

XDebbie multi-host clustering requires SSH access for qemu+ssh:// URIs.
Adding openssh-server as a package dependency ensures it's available
out of the box.

Closes: #<issue-number-if-applicable>
```
