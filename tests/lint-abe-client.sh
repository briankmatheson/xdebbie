#!/usr/bin/env bash
# The daemon talks to ABE through Config::http_client() (cluster CA as
# roots, this box's cert as identity) via abe::Client::with_http(). The
# bare abe::Client::new() is #[cfg(test)] and exists only for unit tests
# against a plain-HTTP fake; nothing outside the test modules of abe.rs
# may call it, or an ABE behind mutual TLS would refuse the daemon.
set -euo pipefail
cd "$(dirname "$0")/.."
# Hits in a file after its `#[cfg(test)]` line are test code; anything
# before it (or in a file without one) is not.
bad=0
while IFS= read -r f; do
  cut=$(grep -n '^#\[cfg(test)\]' "$f" | head -1 | cut -d: -f1 || true)
  cut=${cut:-999999}
  hits=$(grep -n 'abe::Client::new()\|[^a-z_:]Client::new()' "$f" | grep -v 'reqwest::Client::new()' | awk -F: -v c="$cut" '$1 < c' || true)
  if [ -n "$hits" ]; then
    echo "$f: abe::Client::new() outside #[cfg(test)]:" >&2
    echo "$hits" | sed 's/^/  /' >&2
    bad=1
  fi
done < <(grep -rl 'abe::Client\|abe::Nvme' crates/*/src --include='*.rs')
[ $bad = 0 ] || { echo "use abe::Client::with_http(cfg.http_client(abe::TIMEOUT))" >&2; exit 1; }
echo "lint-abe-client: ok"
