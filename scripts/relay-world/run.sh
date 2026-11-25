#!/usr/bin/env bash
# relay-world: prove partii-net's relays worldwide on STAGING.
#
#   scripts/relay-world/run.sh [--build] [--out DIR] [orchestrate.py args...]
#
# - Fly token from the macOS Keychain (partii / partii-fly-token), the room key
#   from ~/.config/partii/staging.env (STAGING_STORE_KEY, a publishable key).
#   Neither is printed; the key reaches machines as an app secret.
# - --build: build and push the net-probe image on Fly's remote builder first
#   (otherwise the image of the current commit must already exist).
# - Hard cap 30 min; a watchdog kills the run if the heartbeat file has not
#   moved for 5 min; every machine in the app is destroyed on any exit.
# - Results: DIR/report.md, DIR/report.json, DIR/results.json, DIR/progress.jsonl.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
app=partii-relay-test
cap_s=${RELAY_WORLD_CAP_S:-1800}
stall_s=${RELAY_WORLD_STALL_S:-300}
build=0
out="$root/target/relay-world/$(date -u +%Y%m%d-%H%M%S)"
pass=()
while [ $# -gt 0 ]; do
  case "$1" in
    --build) build=1; shift ;;
    --out) out="$2"; shift 2 ;;
    *) pass+=("$1"); shift ;;
  esac
done
mkdir -p "$out"

FLY_API_TOKEN="$(security find-generic-password -a partii -s partii-fly-token -w)"
export FLY_API_TOKEN
# Only the two values this needs leave the env file (nothing else is exported).
envf="$HOME/.config/partii/staging.env"
STAGING_STORE_KEY="$(set -a; . "$envf"; set +a; printf %s "${STAGING_STORE_KEY:-}")"
PARTII_EDGE="$(set -a; . "$envf"; set +a; printf %s "${STAGING_EDGE:-}")"
[ -n "$STAGING_STORE_KEY" ] && [ -n "$PARTII_EDGE" ] || { echo "staging.env lacks STAGING_STORE_KEY or STAGING_EDGE" >&2; exit 2; }
export PARTII_EDGE
case "$PARTII_EDGE" in *staging*) ;; *) echo "refusing non-staging edge" >&2; exit 2 ;; esac

destroy_all() {
  local ids
  ids="$(flyctl machine list -a "$app" --json 2>/dev/null | python3 -c 'import json,sys; print(" ".join(m["id"] for m in json.load(sys.stdin)))' 2>/dev/null || true)"
  for id in $ids; do flyctl machine destroy "$id" -a "$app" --force >/dev/null 2>&1 || true; done
  echo "cleanup: destroyed [${ids}]"
}
pids=()
cleanup() {
  local rc=$?
  for p in "${pids[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  destroy_all
  exit $rc
}
trap cleanup EXIT INT TERM HUP

flyctl apps list 2>/dev/null | grep -q "^ *$app " || flyctl apps create "$app" --org personal >/dev/null
# The publishable key as an app secret (stdin, never argv).
printf 'PARTII_KEY=%s\n' "${STAGING_STORE_KEY:?no STAGING_STORE_KEY}" | flyctl secrets import -a "$app" --stage >/dev/null

sha="$(git -C "$root" rev-parse --short HEAD)"
image="registry.fly.io/$app:probe-$sha"
if [ "$build" = 1 ]; then
  (cd "$root" && flyctl deploy --build-only --remote-only --push -a "$app" \
    -c scripts/relay-world/fly.toml --dockerfile scripts/relay-world/Dockerfile \
    --ignorefile scripts/relay-world/dockerignore --image-label "probe-$sha") > "$out/build.log" 2>&1
fi

: > "$out/heartbeat"
python3 "$here/orchestrate.py" --image "$image" --edge "$PARTII_EDGE" --out "$out" "${pass[@]}" &
orch=$!
pids+=("$orch")
start=$(date +%s)
# Hard cap and no-progress watchdog.
while kill -0 "$orch" 2>/dev/null; do
  sleep 10
  now=$(date +%s)
  hb=$(stat -f %m "$out/heartbeat" 2>/dev/null || stat -c %Y "$out/heartbeat")
  if [ $((now - start)) -gt "$cap_s" ]; then
    echo "relay-world: hard cap ${cap_s}s reached; stopping" | tee -a "$out/progress.jsonl"
    kill "$orch" 2>/dev/null || true; break
  fi
  if [ $((now - hb)) -gt "$stall_s" ]; then
    echo "relay-world: no progress for ${stall_s}s; stopping" | tee -a "$out/progress.jsonl"
    kill "$orch" 2>/dev/null || true; break
  fi
done
wait "$orch" 2>/dev/null && rc=0 || rc=$?
echo "relay-world: report in $out/report.md (rc=$rc)"
exit $rc
