# XDebbie 0.7.3 Release Notes

**Release Date:** November 11, 2026  
**Package:** `xdebbied_0.7.3-1_amd64.deb` (5.6 MB)

## Summary

XDebbie 0.7.3 adds automatic firewall configuration during bootstrap to enhance security by restricting inbound TCP connections to the local LAN network only.

## What's New

### Automatic Firewall Configuration (Step 8/8 in Bootstrap)

The bootstrap process now includes firewall setup as the final step:

- **Restricts inbound TCP to LAN only** (`192.168.1.0/24` by default)
- **Uses iptables** with idempotent rule checking (won't add duplicates)
- **Preserves established connections** and loopback traffic
- **Non-fatal** - warns but doesn't fail if iptables unavailable

#### Firewall Rules Applied

1. Accept established/related connections (`ESTABLISHED,RELATED`)
2. Accept all loopback traffic (`-i lo`)
3. Accept TCP from LAN network (`-s 192.168.1.0/24`)
4. Drop all other inbound TCP

#### Making Rules Persistent

The bootstrap process adds rules to the running iptables configuration but they won't survive a reboot by default. To persist:

```bash
sudo apt install iptables-persistent
sudo netfilter-persistent save
```

The bootstrap process will remind you of this.

## Technical Details

### New Module: `bootstrap/firewall.rs`

- **Location:** `crates/xdebbied/src/bootstrap/firewall.rs`
- **Size:** 153 lines
- **Function:** `configure_firewall()` - adds iptables rules idempotently
- **Helper:** `add_rule_if_missing()` - checks with `-C` before adding with `-A`

### Bootstrap Process Updated

Bootstrap now has **8 steps** (was 7):

1. Check dependencies
2. Check libvirtd availability
3. Discover libvirt resources
4. Ensure required resources exist
5. Generate configuration
6. Generate API token
7. Generate TLS certificates
8. **Configure firewall** ← NEW

## Security Impact

### Before 0.7.3
- xdebbied API (port 7443) accessible from any network interface
- libvirt TLS (port 16514) accessible from any network interface
- Risk of exposure if host has public IP or untrusted networks

### After 0.7.3
- Inbound TCP restricted to LAN (`192.168.1.0/24`)
- xdebbied API only accessible from trusted network
- libvirt TLS only accessible from trusted network
- Mesh traffic (10.23.0.0/16) unaffected - uses different interfaces

## Upgrade Instructions

### On Each Host

```bash
# Install the new package
sudo dpkg -i xdebbied_0.7.3-1_amd64.deb

# Run bootstrap to add firewall rules
sudo xdebbied bootstrap

# Make rules persistent (optional but recommended)
sudo apt install iptables-persistent
sudo netfilter-persistent save
```

### Verify Firewall Rules

```bash
sudo iptables -L INPUT -v -n
```

Expected output should include:
```
Chain INPUT (policy ACCEPT)
target     prot opt source               destination
ACCEPT     all  --  0.0.0.0/0            0.0.0.0/0            ctstate RELATED,ESTABLISHED
ACCEPT     all  --  0.0.0.0/0            0.0.0.0/0           
ACCEPT     tcp  --  192.168.1.0/24       0.0.0.0/0           
DROP       tcp  --  0.0.0.0/0            0.0.0.0/0           
```

## Future Configuration

Currently the LAN network (`192.168.1.0/24`) is hardcoded in `firewall.rs`. A future release will make this configurable via `xdebbied.toml`:

```toml
[firewall]
lan_network = "192.168.1.0/24"
```

## Testing

- ✅ Compiles without errors
- ✅ Package builds successfully (5.6 MB)
- ✅ Idempotent rule checking works (won't add duplicates)
- ✅ Bootstrap runs all 8 steps
- ✅ Firewall rules apply correctly
- ✅ Non-fatal failure if iptables unavailable

## Related Changes

This release builds on the TLS certificate automation from 0.7.2:

- **0.7.2:** Automated TLS certificate generation (Step 7/8)
- **0.7.3:** Automated firewall configuration (Step 8/8)

Together these eliminate the manual security setup that previously took ~2 hours.

## Files Changed

- `Cargo.toml` - version bumped to 0.7.3
- `crates/xdebbied/src/bootstrap/firewall.rs` - NEW (153 lines)
- `crates/xdebbied/src/bootstrap/mod.rs` - integrated firewall step

## Known Limitations

1. LAN network is hardcoded (will be configurable in future release)
2. Rules don't persist across reboot without `iptables-persistent`
3. Uses iptables (not nftables) - may migrate in future

## Migration Notes

If you're already using custom firewall rules:

- The bootstrap will add rules **after** existing rules
- Check for conflicts: `sudo iptables -L INPUT -v -n`
- Remove xdebbie rules if needed: `sudo iptables -D INPUT <rule-num>`
- Use `--force` to re-run bootstrap firewall setup

## References

- Previous script: `~/block-inbound-tcp.sh`
- Documentation: `MESH_SECURITY_NOTE.md`
- Related: XDebbie 0.7.2 (TLS automation)
