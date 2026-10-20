#!/usr/bin/env bash
# Release the SDK crates to crates.io, in dependency order.
#
#   scripts/release.sh check     cargo-semver-checks against the last published versions
#   scripts/release.sh dry-run   cargo publish --dry-run for every crate
#   scripts/release.sh publish   publish for real (CARGO_REGISTRY_TOKEN must be set)
set -euo pipefail
cd "$(dirname "$0")/.."

ORDER=(
  partii-rtc-sctp partii-rtc-datachannel partii-rtc partii-webrtc
  partii-protocol partii-netcode partii-client
  partii-net partii-account partii-content partii-server
  bevy_partii partii
)

published() { cargo search "$1" --limit 1 2>/dev/null | grep -q "^$1 = "; }

case "${1:-}" in
  check)
    command -v cargo-semver-checks >/dev/null || { echo "cargo install cargo-semver-checks"; exit 1; }
    for c in "${ORDER[@]}"; do
      if published "$c"; then cargo semver-checks check-release -p "$c"; else echo "$c: not published yet, skipping"; fi
    done
    ;;
  dry-run)
    # Packages and verifies every crate together, in dependency order.
    cargo publish --workspace --dry-run --allow-dirty
    ;;
  publish)
    : "${CARGO_REGISTRY_TOKEN:?set CARGO_REGISTRY_TOKEN}"
    git diff --quiet || { echo "commit first"; exit 1; }
    cargo publish --workspace
    ;;
  *) echo "usage: $0 check|dry-run|publish"; exit 2 ;;
esac
