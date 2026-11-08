# xdebbied Self-Bootstrapping Design

## Goal

Make xdebbied a self-contained binary that can be dropped onto a stock Debian 13 system and automatically:

1. Check and install missing dependencies
2. Auto-discover and configure libvirt resources (networks, storage pools)
3. Generate config file if missing
4. Generate authentication token if missing
5. Start serving

## Current State

Currently requires manual setup:
- Install dependencies (libvirt, cloud-localds, etc.)
- Create libvirt networks manually
- Create storage pools manually
- Write config file manually
- Generate token manually
- Configure systemd service

## Proposed Bootstrap Flow

### On First Run

```
xdebbied serve
  ↓
Check if config exists
  ├─ NO → Bootstrap mode
  │   ├─ Check dependencies
  │   ├─ Auto-discover libvirt resources
  │   ├─ Generate default config
  │   ├─ Generate token
  │   └─ Write config to default location
  └─ YES → Load config and proceed
      ↓
  Validate/remediate dependencies
      ↓
  Ensure required libvirt resources exist
      ↓
  Start serving
```

## Bootstrap Features to Add

### 1. Dependency Checking

Check for required binaries and offer to install:

```rust
struct Dependencies {
    required: Vec<Dependency>,
}

struct Dependency {
    name: &'static str,
    binary: &'static str,
    package: &'static str,  // apt package name
    check_cmd: Option<Vec<&'static str>>,
}

impl Dependencies {
    fn check(&self) -> Vec<Missing> { ... }
    fn remediate(&self) -> Result<()> {
        // Run: apt-get update && apt-get install -y <packages>
    }
}
```

Required dependencies:
- `virsh` (libvirt-clients)
- `cloud-localds` (cloud-image-utils)
- `qemu-img` (qemu-utils)

### 2. Libvirt Resource Discovery

Auto-discover existing libvirt resources:

```rust
struct LibvirtDiscovery {
    networks: Vec<DiscoveredNetwork>,
    pools: Vec<DiscoveredPool>,
    hosts: Vec<DiscoveredHost>,
}

fn discover_libvirt() -> Result<LibvirtDiscovery> {
    // virsh net-list --all
    // virsh pool-list --all
    // virsh list --all
    // Analyze available resources
}
```

### 3. Automatic Network Creation

If no suitable network exists, create one:

```rust
fn ensure_network(name: &str, subnet: &str, dhcp_range: (&str, &str)) -> Result<()> {
    // Check if network exists
    // If not, create with:
    //   virsh net-define <xml>
    //   virsh net-start <name>
    //   virsh net-autostart <name>
}
```

Default network:
```xml
<network>
  <name>xdebbie</name>
  <bridge name='virbr-xdebbie'/>
  <forward mode='nat'/>
  <ip address='192.168.100.1' netmask='255.255.255.0'>
    <dhcp>
      <range start='192.168.100.100' end='192.168.100.200'/>
    </dhcp>
  </ip>
</network>
```

### 4. Automatic Storage Pool Creation

Ensure a storage pool exists:

```rust
fn ensure_storage_pool(name: &str, path: &str) -> Result<()> {
    // Check if pool exists
    // If not, create with:
    //   mkdir -p <path>
    //   virsh pool-define-as <name> dir - - - - <path>
    //   virsh pool-build <name>
    //   virsh pool-start <name>
    //   virsh pool-autostart <name>
}
```

Default pool:
- Name: `xdebbie`
- Path: `/var/lib/libvirt/xdebbie`
- Type: `dir` (directory-based)

### 5. Config Generation

Generate minimal working config:

```rust
fn generate_default_config() -> Config {
    let discovered = discover_libvirt()?;
    
    Config {
        listen: "0.0.0.0:7443".parse().unwrap(),
        state_file: "/var/lib/xdebbie/state.json".into(),
        token_file: Some("/var/lib/xdebbie/token".into()),
        tls_cert: None,  // Start with HTTP
        tls_key: None,
        policy: PlacementPolicy::Spread,
        reconcile_interval_secs: 5,
        host_stale_secs: 60,
        virsh: "virsh".into(),
        seed_command: default_seed_command(),
        hosts: vec![
            Host {
                name: hostname(),
                uri: "qemu:///system".into(),
                failure_domain: "default".into(),
                bridge: discovered.networks.first()
                    .map(|n| n.bridge.clone())
                    .unwrap_or("virbr-xdebbie".into()),
                storage_pool: discovered.pools.first()
                    .map(|p| p.name.clone())
                    .unwrap_or("xdebbie".into()),
                cpu_overcommit: 2.0,
                mem_overcommit: 1.0,
                reserved_cpus: 2,
                reserved_memory_mib: 4096,
                control_plane_eligible: true,
                ..Default::default()
            }
        ],
    }
}
```

### 6. Token Generation

Generate secure token automatically:

```rust
fn generate_token() -> Result<String> {
    use rand::Rng;
    let token: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();
    Ok(token)
}

fn ensure_token(path: &Path) -> Result<()> {
    if !path.exists() {
        let token = generate_token()?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, token)?;
        // Set permissions: 0640, owned by current user
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640))?;
        }
        eprintln!("Generated new token at {}", path.display());
        eprintln!("Save this for API clients:");
        eprintln!("  export XDEBBIE_TOKEN=$(cat {})", path.display());
    }
    Ok(())
}
```

## Command-Line Interface

### New Bootstrap Command

```bash
xdebbied bootstrap [OPTIONS]
```

Options:
- `--check-only` - Only check dependencies, don't fix
- `--no-install` - Don't auto-install packages
- `--network <name>` - Network name to use/create
- `--pool <name>` - Storage pool name to use/create
- `--config <path>` - Where to write config (default: /etc/xdebbie/xdebbied.toml)

### Modified Serve Command

```bash
xdebbied serve [OPTIONS]
```

New options:
- `--bootstrap` - Run bootstrap before serving
- `--bootstrap-if-needed` - Bootstrap only if config missing (default: on)

### Example Usage

```bash
# Copy binary to new system
scp xdebbied newhost:/usr/local/bin/

# On new system, run as root or with sudo
sudo xdebbied bootstrap
# Checks dependencies, creates resources, generates config

# Start serving (can also be run as systemd service)
sudo xdebbied serve
# Automatically validates and serves

# Or combine
sudo xdebbied serve --bootstrap
```

## File Structure After Bootstrap

```
/etc/xdebbie/
  xdebbied.toml          # Generated config
  
/var/lib/xdebbie/
  state.json               # Runtime state
  token                    # Generated API token
  
/var/lib/libvirt/xdebbie/  # Storage pool
  (VM disk images)
```

## Implementation Plan

### Phase 1: Basic Bootstrap

1. Add dependency checking
2. Add config generation
3. Add token generation
4. Add `bootstrap` subcommand

### Phase 2: Libvirt Discovery

1. Add libvirt resource discovery
2. Auto-select existing resources if suitable
3. Log discovered resources

### Phase 3: Resource Creation

1. Add network creation
2. Add storage pool creation
3. Make resources persistent (autostart)

### Phase 4: Integration

1. Add `--bootstrap` flag to `serve`
2. Auto-bootstrap on first run if config missing
3. Validate resources on every start

### Phase 5: Polish

1. Better error messages
2. Interactive prompts (optional)
3. Dry-run mode
4. Status reporting

## Code Structure

New files to add:

```
crates/xdebbied/src/
  bootstrap/
    mod.rs           # Main bootstrap logic
    deps.rs          # Dependency checking
    libvirt.rs       # Resource discovery/creation
    config.rs        # Config generation
    token.rs         # Token generation
```

Modify:
- `main.rs` - Add bootstrap subcommand
- `config.rs` - Add generation methods

## Example Bootstrap Output

```
$ sudo xdebbied bootstrap

xdebbied Bootstrap
====================

[1/5] Checking dependencies...
  ✓ virsh found at /usr/bin/virsh
  ✓ cloud-localds found at /usr/bin/cloud-localds
  ✓ qemu-img found at /usr/bin/qemu-img

[2/5] Discovering libvirt resources...
  ✓ Found network 'default' (virbr0, 192.168.122.0/24)
  ! No storage pool found in /var/lib/libvirt/images

[3/5] Creating missing resources...
  → Creating storage pool 'xdebbie' at /var/lib/libvirt/xdebbie
  ✓ Pool created and started
  ✓ Pool set to autostart

[4/5] Generating configuration...
  ✓ Config written to /etc/xdebbie/xdebbied.toml
  
[5/5] Generating API token...
  ✓ Token written to /var/lib/xdebbie/token
  
Bootstrap complete!

To start serving:
  xdebbied serve

To use the API:
  export XDEBBIE_URL=http://localhost:7443
  export XDEBBIE_TOKEN=$(cat /var/lib/xdebbie/token)
  xdctl host ls
```

## Security Considerations

1. **Root required** - Network/pool creation needs root
2. **Token permissions** - Set 0640, readable only by xdebbie user/group
3. **Config permissions** - Set 0644, readable by all
4. **Default to HTTP** - TLS requires user-provided certs
5. **Log token path** - But never log token value

## Backwards Compatibility

- Existing configs continue to work
- Bootstrap only runs if config missing or explicitly requested
- No changes to existing runtime behavior
- New flags are opt-in

## Testing

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_config_generation() { ... }
    
    #[test]
    fn test_token_generation() { ... }
    
    #[test]
    fn test_network_xml_generation() { ... }
    
    #[test]
    fn test_pool_xml_generation() { ... }
}
```

## Documentation Updates

- Update README.md with bootstrap usage
- Add QUICKSTART.md for new users
- Update installation docs
- Add systemd service example with bootstrap

## Future Enhancements

- Interactive wizard mode
- Multiple network support
- Remote host auto-discovery (scan subnet for libvirt hosts)
- Image cache management
- Automatic updates
- Health check endpoint
- Metrics export

## Benefits

1. **Easier deployment** - Single binary, no manual setup
2. **Consistent configuration** - Generated configs are known-good
3. **Faster onboarding** - New users up and running in minutes
4. **Fewer support issues** - Auto-remediation of common problems
5. **Better testing** - Easy to spin up test instances
6. **Cloud-init friendly** - Can be automated in VM provisioning

This makes xdebbied truly "drop in and run" - perfect for both development/testing and production use.
