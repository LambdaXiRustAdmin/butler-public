#!/usr/bin/env bash
# Stranger-path smoke: server up → optional warm → one Trace.
# Usage:
#   ./scripts/smoke_stranger.sh /absolute/path/to/repo [symbol]
# Env:
#   BUTLER_URL   default http://127.0.0.1:8002
#   BUTLER_PASSWORD / BUTLER_API_TOKEN  if server auth is on
set -euo pipefail

BASE="${BUTLER_URL:-http://127.0.0.1:8002}"
BASE="${BASE%/}"
ROOT="${1:-}"
SYMBOL="${2:-main}"

auth_args=()
if [[ -n "${BUTLER_PASSWORD:-${BUTLER_API_TOKEN:-}}" ]]; then
  auth_args=(-H "Authorization: Bearer ${BUTLER_PASSWORD:-$BUTLER_API_TOKEN}")
fi

echo "== health =="
if ! curl -sfS -m 5 "${auth_args[@]}" "$BASE/mcp/health" | head -c 200; then
  echo
  echo "FAIL: no health at $BASE — start butler-server first:"
  echo "  cargo build --release -p cli && ./target/release/butler-server"
  exit 2
fi
echo
echo

if [[ -z "$ROOT" ]]; then
  echo "Usage: $0 /absolute/path/to/repo [symbol]"
  echo "Health OK. Pass a project root for warm + Trace."
  exit 0
fi

if [[ "$ROOT" != /* ]]; then
  echo "FAIL: project root must be absolute (got: $ROOT)"
  exit 2
fi

echo "== warm (async register) =="
curl -sfS -m 30 "${auth_args[@]}" -X POST "$BASE/warm" \
  -H 'Content-Type: application/json' \
  -d "{\"roots\":[\"$ROOT\"]}" || true
echo
echo

echo "== Trace $SYMBOL (may BUILDING on first hit — retry same request) =="
for attempt in 1 2 3 4 5 6; do
  body=$(curl -sfS -m 120 "${auth_args[@]}" -X POST "$BASE/context" \
    -H 'Content-Type: application/json' \
    -d "{\"project\":\"$ROOT\",\"goal\":\"TraceBlastRadius\",\"target_symbol\":\"$SYMBOL\",\"detail\":\"compact\"}")
  if echo "$body" | grep -q 'status: BUILDING\|BUILDING_SOFT_WALL\|hydrating'; then
    echo "  attempt $attempt: still building/hydrating — sleep 3s, retry same Trace"
    sleep 3
    continue
  fi
  echo "$body" | python3 -c '
import json, sys
d = json.load(sys.stdin)
content = d.get("content") or ""
st = d.get("structured") or {}
t = st.get("target") or {}
print("--- content (first 900 chars) ---")
print(content[:900])
print("---")
if t:
    print("★", t.get("name"), "@", t.get("file"), ":", t.get("line"))
else:
    print("★ (no target in structured — disambiguate/miss/error; read content)")
print("mode:", d.get("mode"), "selected_count:", d.get("selected_count"))
' 
  echo
  echo "OK: smoke finished (read dossier above; pin scope_paths if disambiguate)."
  exit 0
done

echo "FAIL: still BUILDING after retries — wait and re-run, or: butler warm -r $ROOT --full --server $BASE"
exit 1
