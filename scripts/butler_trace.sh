#!/usr/bin/env bash
# Thin Trace CLI for agents (Aider, shell) — no IDE required.
#
# Usage:
#   butler_trace /abs/project Symbol [scope_path ...]
#   butler_trace /projects/test_repos/fd construct_config src/
#
# Env:
#   BUTLER_URL              default http://127.0.0.1:8002
#   BUTLER_PASSWORD / BUTLER_API_TOKEN
#   BUTLER_TRACE_DETAIL     compact|dense  default compact
set -euo pipefail

BASE="${BUTLER_URL:-http://127.0.0.1:8002}"
BASE="${BASE%/}"
DETAIL="${BUTLER_TRACE_DETAIL:-compact}"

if [[ $# -lt 2 ]]; then
  echo "Usage: $0 /abs/project Symbol [scope_path ...]" >&2
  exit 2
fi

PROJECT="$1"
SYMBOL="$2"
shift 2

if [[ "$PROJECT" != /* ]]; then
  echo "project must be absolute (got: $PROJECT)" >&2
  exit 2
fi

auth_args=()
if [[ -n "${BUTLER_PASSWORD:-${BUTLER_API_TOKEN:-}}" ]]; then
  auth_args=(-H "Authorization: Bearer ${BUTLER_PASSWORD:-$BUTLER_API_TOKEN}")
fi

# Build JSON with optional scope_paths
if [[ $# -gt 0 ]]; then
  scopes=$(printf '%s\n' "$@" | python3 -c 'import json,sys; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))')
  body=$(python3 -c "
import json
print(json.dumps({
  'project': '''$PROJECT''',
  'goal': 'TraceBlastRadius',
  'target_symbol': '''$SYMBOL''',
  'scope_paths': json.loads('''$scopes'''),
  'detail': '''$DETAIL''',
}))
")
else
  body=$(python3 -c "
import json
print(json.dumps({
  'project': '''$PROJECT''',
  'goal': 'TraceBlastRadius',
  'target_symbol': '''$SYMBOL''',
  'detail': '''$DETAIL''',
}))
")
fi

for attempt in 1 2 3 4 5 6; do
  raw=$(curl -sfS -m 120 "${auth_args[@]}" -X POST "$BASE/context" \
    -H 'Content-Type: application/json' \
    -d "$body") || {
    echo "curl failed (is butler-server up at $BASE?)" >&2
    exit 2
  }
  if echo "$raw" | grep -q 'status: BUILDING\|BUILDING_SOFT_WALL\|hydrating'; then
    echo "# BUILDING/hydrating attempt $attempt — retry" >&2
    sleep 3
    continue
  fi
  echo "$raw" | python3 -c '
import json, sys
d = json.load(sys.stdin)
print(d.get("content") or "")
st = d.get("structured") or {}
t = st.get("target") or {}
if t:
    print("---")
    print("★", t.get("name"), "@", t.get("file"), ":", t.get("line"))
na = st.get("next_action")
if na:
    print("next:", na)
'
  exit 0
done
echo "still BUILDING after retries" >&2
exit 1
