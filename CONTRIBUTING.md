# Contributing to XDebbie

Thanks for helping. XDebbie is GPL-3.0-or-later; by contributing you agree
your work is released under that license.

## Ground rules

- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`
  must pass. CI runs all three, and checks that `deploy/crds.yaml` matches
  `capx crds`.
- New source files start with `// SPDX-License-Identifier: GPL-3.0-or-later`.
- Scheduler changes need a test in `crates/xdebbie-core/src/scheduler.rs`.
  Reconciler changes need a test against the fake hypervisor.
- Keep hosts simple. If a feature needs software on the hypervisor beyond
  libvirt, QEMU and xdebbied, open an issue first.

## Documentation

The docs live in `docs/` and build into a site with
[mdBook](https://rust-lang.github.io/mdBook/) (`mdbook build`, output in
`book/`). `docs/SUMMARY.md` is the table of contents; a new page must be
listed there.

- Write plain, short sentences. How-to guides speak to the reader ("Run
  this").
- Start every page with a one-line purpose under its title.
- Tag every code block with a language (`bash`, `toml`, `json`, `text`).
- Write placeholders in capitals (`xdctl approve NEW_BOX`), never in angle
  brackets.
- Use `example.org` names and the documentation address ranges
  (`192.0.2.0/24`), or the product defaults (`10.22.0.0/16`,
  `10.23.0.0/16`). No real hostnames or addresses.
- A new config key goes in `docs/reference/configuration.md`, a new route in
  `docs/reference/api.md`, a new `xdctl` command in
  `docs/reference/xdctl.md`.

Before sending a change, run:

```bash
tests/lint-links.sh        # links, fences, placeholders, site-specific names
tests/lint-config-doc.sh   # every config key is documented
mdbook build               # if you have mdbook
```

If you move a page that the Debian package ships, update `assets` in
`crates/xdebbied/Cargo.toml` and `scripts/verify-package.sh`.

## Sign-off

Every commit carries a [Developer Certificate of Origin](https://developercertificate.org/)
sign-off, which certifies you have the right to submit it:

```bash
git commit -s -m "scheduler: prefer hosts with warm base images"
```

## Regenerating CRDs

```bash
cargo run -q -p capx -- crds > deploy/crds.yaml
```
