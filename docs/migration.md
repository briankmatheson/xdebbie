# Migration Guide: Adding Bootstrap to Existing xdebbied

This guide helps you migrate from the pre-bootstrap version of xdebbied to the new self-bootstrapping version.

## For Existing Deployments

If you already have xdebbied running with manual configuration, you can:

### Option 1: Keep Your Existing Setup

The new version is **fully backward compatible**. Just upgrade the binary:

```bash
# Build new version
cd src/xdebbie
cargo build --release --package xdebbied

# Stop old version
sudo systemctl stop xdebbied  # or kill existing process

# Replace binary
sudo cp target/release/xdebbied /usr/local/bin/

# Start new version (uses existing config)
sudo xdebbied serve
```

Your existing configuration, token, and state files will work unchanged.

### Option 2: Migrate to Bootstrap-Generated Config

If you want to use the new bootstrap functionality:

```bash
# Backup existing config
sudo cp /etc/xdebbie/xdebbied.toml /etc/xdebbie/xdebbied.toml.backup
sudo cp /etc/xdebbie/token /etc/xdebbie/token.backup

# Stop server
sudo systemctl stop xdebbied

# Run bootstrap (with --force to overwrite)
sudo xdebbied bootstrap --force

# Review new config
diff /etc/xdebbie/xdebbied.toml.backup /etc/xdebbie/xdebbied.toml

# Merge any custom settings from backup
# (e.g., custom listen address, TLS certs, multi-host configs)

# Start server
sudo xdebbied serve
```

### Option 3: Keep Config, Add Bootstrap Features

Use bootstrap to ensure resources exist, but keep your config:

```bash
# Run bootstrap WITHOUT --force (won't overwrite config)
sudo xdebbied bootstrap

# This will:
# - Check/install dependencies
# - Ensure libvirtd is running
# - Create missing networks/pools
# - Generate token if missing
# But preserve your existing config!
```

## Configuration Compatibility

### Field Name Changes

The bootstrap implementation uses the **correct** field names from the original codebase. If you created config manually with incorrect field names, update them:

| Old (Incorrect) | New (Correct) |
|----------------|---------------|
| `bind` | `listen` |
| `state_path` | `state_file` |
| `memory_overcommit` | `mem_overcommit` |
| `qemu_uri` | `uri` |
| `image_pool` | `storage_pool` |

Example migration:

```toml
# OLD (incorrect)
bind = "0.0.0.0:7443"
state_path = "/var/lib/xdebbie/state.json"

[[hosts]]
qemu_uri = "qemu:///system"
image_pool = "default"
memory_overcommit = 1.0
```

```toml
# NEW (correct)
listen = "0.0.0.0:7443"
state_file = "/var/lib/xdebbie/state.json"

[[hosts]]
uri = "qemu:///system"
storage_pool = "default"
mem_overcommit = 1.0
```

### New Fields Available

Bootstrap adds sensible defaults for fields you may not have set:

```toml
# These are now set by default if not present:
policy = "spread"                # or "binpack"
reconcile_interval_secs = 5
host_stale_secs = 60
virsh = "virsh"
seed_command = ["cloud-localds", "--network-config={net}", "{out}", "{user}", "{meta}"]
```

You can customize these in your config or let bootstrap set defaults.

## Multi-Host Configurations

If you have multiple hosts configured, bootstrap will:

1. **Preserve your hosts list** if config exists
2. **Add localhost** as first host if creating new config
3. **Not modify** existing multi-host setups

Example preserved multi-host config:

```toml
[[hosts]]
name = "node1"
uri = "qemu+ssh://node1/system"
bridge = "br0"
storage_pool = "vms"
# ... other settings

[[hosts]]
name = "node2"
uri = "qemu+ssh://node2/system"
bridge = "br0"
storage_pool = "vms"
# ... other settings
```

Bootstrap will **not** overwrite this unless you use `--force`.

## TLS Configuration

If you have TLS configured, bootstrap will:

1. **Preserve** your `tls_cert` and `tls_key` settings
2. **Generate config** with TLS fields commented out by default
3. **Not interfere** with existing TLS setup

Your existing TLS config:

```toml
tls_cert = "/etc/xdebbie/cert.pem"
tls_key = "/etc/xdebbie/key.pem"
```

Will be preserved and continue working.

## Token Migration

### Existing Token

If you already have a token file:

```bash
# Bootstrap will reuse it
sudo xdebbied bootstrap
# Output: "Token already exists at /etc/xdebbie/token, reusing"

# To generate new token
sudo xdebbied bootstrap --force
```

### Environment File

Bootstrap creates `~/.xdebbie_env` for convenience. If you use a different method to set environment variables:

**Option 1**: Source the env file
```bash
source ~/.xdebbie_env
```

**Option 2**: Continue using your method
```bash
export XDEBBIE_URL=http://myhost:7443
export XDEBBIE_TOKEN=$(sudo cat /etc/xdebbie/token)
```

Both work equally well.

## State File Migration

Bootstrap **never** modifies `/var/lib/xdebbie/state.json`. Your VM state, host capacity, and runtime data are preserved across upgrades.

## Systemd Service Updates

If you have a systemd service, you can optionally add bootstrap to the start sequence:

### Old Service (Still Works)

```ini
[Service]
ExecStart=/usr/local/bin/xdebbied serve
```

### New Service (With Bootstrap)

```ini
[Service]
ExecStart=/usr/local/bin/xdebbied serve --bootstrap
```

The `--bootstrap` flag will:
- Run bootstrap checks/setup
- Log warnings if anything fails
- Continue to serve even if bootstrap fails

This is useful for:
- Systems that might reboot and lose libvirt networks
- Automatic recovery from service failures
- Ensuring dependencies are always met

## Rollback Plan

If you need to rollback to the old version:

```bash
# Stop new version
sudo systemctl stop xdebbied

# Restore old binary
sudo cp /usr/local/bin/xdebbied.backup /usr/local/bin/xdebbied

# Restore config if changed
sudo cp /etc/xdebbie/xdebbied.toml.backup /etc/xdebbie/xdebbied.toml

# Start old version
sudo systemctl start xdebbied
```

State file is compatible in both directions.

## Testing Checklist

After upgrading:

- [ ] Server starts without errors
- [ ] Existing VMs are visible (`xdctl vm ls`)
- [ ] Can create new VMs
- [ ] Host inventory updates correctly
- [ ] Token authentication works
- [ ] Client tools connect successfully
- [ ] Reconciliation loop running
- [ ] VMs can be deleted
- [ ] State persists across restarts

## Troubleshooting

### "unknown field" errors

You're using old (incorrect) field names. Update your config:

```bash
# See what's wrong
sudo xdebbied serve

# Generate correct config
sudo xdebbied bootstrap --force

# Or manually fix field names (see table above)
```

### "missing or invalid bearer token"

Token file might have wrong permissions or path:

```bash
# Check token file
ls -la /etc/xdebbie/token

# Should be mode 0600, readable by server user

# Regenerate if needed
sudo xdebbied bootstrap --force
```

### Bootstrap creates unwanted resources

If you don't want the default `xdebbie` network/pool:

```bash
# Use existing resources
sudo xdebbied bootstrap --network default --pool default

# Or keep your manual config
sudo xdebbied serve  # without --bootstrap
```

## Best Practices

1. **Backup first**: Copy config, token, and state before upgrading
2. **Test in staging**: Try the upgrade on a non-production system
3. **Review config**: Check generated config matches your needs
4. **Monitor startup**: Watch logs during first start with new version
5. **Keep backups**: Save old binary and config for quick rollback

## Support

If you encounter issues:

1. Check logs: `journalctl -u xdebbied -f`
2. Validate config: `sudo xdebbied check`
3. Review documentation: [BOOTSTRAP.md](src/xdebbie/BOOTSTRAP.md)
4. File an issue with logs and config (redact tokens!)

## Summary

The bootstrap functionality is:
- **Fully backward compatible** - old configs work unchanged
- **Optional** - you can ignore it and use manual setup
- **Non-destructive** - preserves existing resources unless `--force`
- **Additive** - adds convenience, doesn't remove functionality

Upgrade with confidence! The new version works exactly like the old version when you use `xdebbied serve` without `--bootstrap`.
