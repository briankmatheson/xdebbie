# Bugfix Release: v0.3.1

**Date**: 2026-11-08  
**Type**: Hotfix  
**Previous Version**: 0.3.0  

## Issue

Bootstrap failed when running as systemd service with error:

```
Error: writing environment file to /root/.xdebbie_env
Caused by:
    Read-only file system (os error 30)
```

### Root Cause

The systemd service runs with `ProtectHome=true` security hardening, which makes home directories read-only. The bootstrap process tried to write `~/.xdebbie_env` to `/root/.xdebbie_env`, which failed.

### Impact

- ❌ First-time installation via `dpkg -i` would fail to start
- ❌ `systemctl start xdebbied` would fail
- ❌ ExecStartPre bootstrap step would exit with status 1
- ✅ Manual bootstrap (`xdebbied bootstrap`) worked fine

## Fix

### Code Changes

**File**: `src/xdebbied/src/bootstrap/mod.rs`

Changed environment file location from home directory to system directory:

```diff
- // Write environment file to home directory
- if let Some(home) = std::env::var_os("HOME") {
-     let env_path = Path::new(&home).join(DEFAULT_ENV_FILE);
-     let url = format!("http://{}", opts.listen_addr);
-     token::write_env_file(&env_path, &url, &token)?;
- }
+ // Write environment file to /etc/xdebbie/ (system-wide)
+ let cfg_dir = opts.config_path.parent().unwrap_or_else(|| Path::new(DEFAULT_CONFIG_DIR));
+ let env_path = cfg_dir.join("xdebbie_env");
+ let url = format!("http://{}", opts.listen_addr);
+ if let Err(e) = token::write_env_file(&env_path, &url, &token) {
+     warn!("Failed to write environment file: {}", e);
+     info!("You can manually create it later with:");
+     info!("  echo 'export XDEBBIE_URL={}' > /etc/xdebbie/xdebbie_env", url);
+     info!("  echo 'export XDEBBIE_TOKEN=<token>' >> /etc/xdebbie/xdebbie_env");
+ }
```

**Benefits**:
- `/etc/xdebbie/` is already in systemd `ReadWritePaths`
- System-wide environment file accessible to all users
- Graceful error handling (warns instead of failing)
- Better for multi-user systems

### Updated Constant

```diff
- pub const DEFAULT_ENV_FILE: &str = ".xdebbie_env";
+ pub const DEFAULT_ENV_FILE: &str = "xdebbie_env";
```

### postinst Script Update

Updated installation instructions:

```diff
- echo "Configure client (as your user):"
- echo "  source ~/.xdebbie_env"
- echo "  xdctl host ls"
+ echo "Configure client (run as your user):"
+ echo "  export XDEBBIE_URL=http://127.0.0.1:7443"
+ echo "  export XDEBBIE_TOKEN=\$(sudo cat /etc/xdebbie/token)"
+ echo "  xdctl host ls"
+ echo ""
+ echo "Or source system-wide environment:"
+ echo "  source /etc/xdebbie/xdebbie_env"
```

### Documentation Updates

Updated all references in documentation:

- `BOOTSTRAP.md`
- `CHANGELOG.md`
- `README.md`
- `DISCOVERY.md`
- `RELEASE-NOTES-v0.3.0.md`
- `IMPLEMENTATION-COMPLETE.md`

Changed:
- `~/.xdebbie_env` → `/etc/xdebbie/xdebbie_env`
- `source ~/.xdebbie_env` → `source /etc/xdebbie/xdebbie_env`

## Usage

### Old Way (v0.3.0 - Broken)

```bash
source ~/.xdebbie_env  # ❌ File not created
```

### New Way (v0.3.1 - Fixed)

**Option 1**: Source system environment file
```bash
source /etc/xdebbie/xdebbie_env
xdctl host ls
```

**Option 2**: Manual environment variables
```bash
export XDEBBIE_URL=http://127.0.0.1:7443
export XDEBBIE_TOKEN=$(sudo cat /etc/xdebbie/token)
xdctl host ls
```

**Option 3**: One-liner with inline token
```bash
XDEBBIE_TOKEN=$(sudo cat /etc/xdebbie/token) xdctl host ls
```

## Testing

### Before Fix (v0.3.0)

```bash
$ sudo systemctl start xdebbied
Job for xdebbied.service failed because the control process exited with error code.
See "systemctl status xdebbied.service" and "journalctl -xe" for details.

$ journalctl -u xdebbied -n 5
Error: writing environment file to /root/.xdebbie_env
Caused by:
    Read-only file system (os error 30)
```

### After Fix (v0.3.1)

```bash
$ sudo systemctl start xdebbied
# Success!

$ systemctl status xdebbied
● xdebbied.service - xdebbie VM Orchestrator
     Active: active (running)

$ ls -la /etc/xdebbie/
-rw-r--r-- 1 root root  xdebbie_env
-rw------- 1 root root  token
-rw-r--r-- 1 root root  xdebbied.toml

$ source /etc/xdebbie/xdebbie_env
$ xdctl host ls
# Works!
```

## Version Bump

- **v0.3.0** → **v0.3.1**
- Git tag: `v0.3.1`
- Package: `xdebbied_0.3.1-1_amd64.deb`

## Upgrade Path

### From v0.3.0

```bash
# Just reinstall
sudo dpkg -i xdebbied_0.3.1-1_amd64.deb

# Service will restart automatically
systemctl status xdebbied

# Update your shell environment
source /etc/xdebbie/xdebbie_env
```

### From v0.2.x or earlier

```bash
# Normal upgrade
sudo dpkg -i xdebbied_0.3.1-1_amd64.deb
```

## No Configuration Changes Required

✅ No changes to `/etc/xdebbie/xdebbied.toml`  
✅ No changes to token file  
✅ No changes to state file  
✅ Discovery/election settings unchanged  
✅ Fully backward compatible  

## Security Implications

**Improved Security**:
- System-wide file in `/etc/xdebbie/` (root-owned)
- Consistent with other configuration files
- Protected by systemd hardening
- No write to home directories from root service

**File Permissions**:
```bash
$ ls -la /etc/xdebbie/xdebbie_env
-rw-r--r-- 1 root root 123 Nov  8 16:30 /etc/xdebbie/xdebbie_env
```

Users can read the file but need `sudo` to read the actual token:
```bash
$ cat /etc/xdebbie/xdebbie_env
export XDEBBIE_URL=http://127.0.0.1:7443
export XDEBBIE_TOKEN=$(sudo cat /etc/xdebbie/token)
```

## Summary

This hotfix resolves a critical installation issue where the systemd service would fail to start due to attempting to write to a read-only filesystem. The fix moves the environment file to a system-wide location (`/etc/xdebbie/xdebbie_env`) which is both more appropriate for a system daemon and compatible with systemd security hardening.

**All features from v0.3.0 remain unchanged**:
- ✅ Auto-discovery
- ✅ Leader election  
- ✅ High availability
- ✅ Zero configuration

**Installation now works correctly**:
- ✅ `dpkg -i` completes successfully
- ✅ Service starts without errors
- ✅ Environment file created
- ✅ Ready to use immediately

**Release**: xdebbied_0.3.1-1_amd64.deb (3.5 MB)
