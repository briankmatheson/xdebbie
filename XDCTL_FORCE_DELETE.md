# xdctl: Add --force flag to vm rm command

## Issue

Currently, `xdctl vm rm` doesn't support a `--force` flag, which is a common UX pattern for deletion commands.

```bash
$ xdctl vm rm demo --force
error: unexpected argument '--force' found
```

## Expected Behavior

```bash
# Normal delete (graceful shutdown, wait for cleanup)
xdctl vm rm <id>

# Force delete (immediate, skip cleanup checks)
xdctl vm rm <id> --force
```

## Use Cases

1. **Stuck deletions**: VMs stuck in "Deleting" state
2. **Emergency cleanup**: When graceful shutdown fails
3. **Bulk operations**: Scripts that need reliable cleanup
4. **Development**: Quick iteration without waiting

## Implementation

### CLI Changes

In `crates/xdctl/src/main.rs`:

```rust
#[derive(Args)]
pub struct VmRmArgs {
    /// VM ID to remove
    id: String,
    
    /// Force deletion without graceful shutdown
    #[arg(short, long)]
    force: bool,
}
```

### API Changes

The xdebbied API should support a force parameter:

```
DELETE /v1/vms/{id}?force=true
```

When `force=true`:
- Skip graceful shutdown (virsh destroy instead of shutdown)
- Don't wait for VM to stop
- Clean up libvirt domain immediately
- Delete LVM volumes immediately
- Remove from state.json
- Return success even if some cleanup fails

### Reconciler Changes

Add a `force_delete` field to VM state:

```rust
pub struct Vm {
    // ...
    pub force_delete: bool,
}
```

When reconciling a VM with `force_delete = true`:
- Use `virsh destroy` instead of `virsh shutdown`
- Don't wait for graceful stop
- Remove immediately from inventory

## Related Commands

For consistency, other delete commands should also support --force:

```bash
# These would make sense too:
xdctl host drain <name> --force    # Skip VM migration
xdctl vm stop <id> --force         # Hard power off
```

## Safety

With great power comes great responsibility:

- `--force` should show a warning for running VMs
- Consider requiring confirmation: `--force --yes` or `--force -y`
- Log forced deletions prominently
- Document data loss risks

Example warning:

```
⚠ WARNING: Force deleting running VM 'demo'
This will:
  - Immediately power off the VM
  - Skip graceful shutdown
  - May cause data loss

Continue? (y/N):
```

## Testing

Add tests for:
- Force delete of running VM
- Force delete of stopped VM
- Force delete of VM stuck in Deleting state
- Force delete with missing libvirt domain
- Force delete with missing LVM volumes

## Documentation

Update:
- `xdctl vm rm --help`
- README.md examples
- Operations guide

## Priority

**Medium-High**: Common pain point during development and operations.

## Workaround (Current)

```bash
# Restart xdebbied to clean up stuck VMs
sudo systemctl restart xdebbied

# Or manually:
virsh destroy <vm-id>
virsh undefine <vm-id>
lvremove timesink-vg/<vm-id>-root
# Edit /var/lib/xdebbie/state.json
```

## See Also

- `kubectl delete pod --force --grace-period=0`
- `docker rm -f`
- `virsh destroy` vs `virsh shutdown`
