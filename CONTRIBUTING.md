# Contributing to Qfodufk8s

Thanks for helping. Qfodufk8s is GPL-3.0-or-later; by contributing you agree your
work is released under that license.

## Ground rules

- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`
  must pass. CI runs all three, and checks that `deploy/crds.yaml` matches
  `capq crds`.
- New source files start with `// SPDX-License-Identifier: GPL-3.0-or-later`.
- Scheduler changes need a test in `qfodufk8s-core/src/scheduler.rs`.
  Reconciler changes need a test against the fake hypervisor.
- Keep hosts agentless. If a feature needs software on the hypervisor beyond
  libvirt and QEMU, open an issue first.

## Sign-off

Every commit carries a [Developer Certificate of Origin](https://developercertificate.org/)
sign-off, which certifies you have the right to submit it:

```
git commit -s -m "scheduler: prefer hosts with warm base images"
```

## Regenerating CRDs

```
cargo run -q -p capq -- crds > deploy/crds.yaml
```
