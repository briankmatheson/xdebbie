#!/usr/bin/env bash
# The repos live under github.com/briankmatheson/; the form without the
# "k" is a typo that has crept into docs before. Fails on any occurrence
# (email addresses at brianmatheson.com are not links and are left alone;
# specs/ may quote the typo on purpose).
set -euo pipefail
cd "$(dirname "$0")/.."
if command -v rg >/dev/null 2>&1; then
  hits=$(rg -n 'github\.com/brianmatheson/' --glob '!target' --glob '!Cargo.lock' --glob '!specs' . || true)
else
  hits=$(grep -rn 'github\.com/brianmatheson/' --exclude-dir=target --exclude-dir=.git --exclude-dir=specs --exclude=Cargo.lock . || true)
fi
if [ -n "$hits" ]; then
  echo "bad repo links (the account is briankmatheson, with a k):" >&2
  echo "$hits" >&2
  exit 1
fi
# Relative links and anchors, code-fence languages, UPPERCASE placeholders,
# site-specific names, and docs/SUMMARY.md coverage (tests/lint-docs.py).
if command -v python3 >/dev/null 2>&1; then
  python3 tests/lint-docs.py
else
  echo "lint-links: python3 missing; skipped tests/lint-docs.py" >&2
fi
echo "lint-links: ok"
