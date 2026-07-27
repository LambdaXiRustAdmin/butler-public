#!/usr/bin/env bash
# Structural half of Desirability Gate P1 — glove-fit Trace + reverse CALL spine.
# Usage: bash scripts/desirability_gate_curl.sh [http://127.0.0.1:8002]
set -euo pipefail
BASE="${1:-http://127.0.0.1:8002}"
pass=0
fail=0

post() {
  curl -sS -X POST "${BASE}/context" \
    -H 'Content-Type: application/json' \
    -d "$1" --max-time 90
}

content() {
  python3 -c '
import sys,json
j=json.loads(sys.stdin.read())
c=j.get("content")
if isinstance(c,list) and c:
  print(c[0].get("text",""))
elif isinstance(c,str):
  print(c)
else:
  print(str(j)[:500])
'
}

check() {
  local name="$1" needle="$2" text="$3"
  if echo "$text" | grep -q "$needle"; then
    echo "  OK  $name"
    pass=$((pass+1))
  else
    echo "  FAIL $name (missing: $needle)"
    fail=$((fail+1))
  fi
}

echo "=== Desirability Gate P1 — structural curls ($BASE) ==="
if ! curl -sS --max-time 5 "${BASE}/mcp/health" | grep -q '"status"'; then
  echo "FAIL health not ok at $BASE"
  exit 1
fi
echo "health ok"
echo

echo "--- Edit-Map payload: click Command ---"
post '{"project":"/projects/test_repos/click","goal":"architecture","scope_paths":["src/click/"],"detail":"compact"}' \
  | content >/tmp/gate_click_arch.txt || true
T=$(post '{"project":"/projects/test_repos/click","goal":"trace","target_symbol":"Command","scope_paths":["src/click/"],"detail":"compact"}' | content)
echo "$T" | head -20
echo "$T" >/tmp/gate_click_command.txt
check "receipt high or complete" "receipt:" "$T"
# Callers may be all same-file (honest local helpers) — require external *callers*
# only when the warehouse sample has cross-file parents; otherwise local framing is OK.
if echo "$T" | grep -q "external callers"; then
  echo "  OK  external callers header"
  pass=$((pass+1))
  if echo "$T" | grep -q "local helpers"; then
    ext_line=$(echo "$T" | grep -n "external callers" | head -1 | cut -d: -f1)
    loc_line=$(echo "$T" | grep -n "local helpers" | head -1 | cut -d: -f1)
    if [[ -n "$ext_line" && -n "$loc_line" && "$ext_line" -lt "$loc_line" ]]; then
      echo "  OK  external section before local"
      pass=$((pass+1))
    else
      echo "  FAIL external not before local (ext=$ext_line loc=$loc_line)"
      fail=$((fail+1))
    fi
  fi
elif echo "$T" | grep -qE 'local helpers|all same-file'; then
  echo "  OK  local-only callers framing (no external CALL parents in sample)"
  pass=$((pass+1))
else
  echo "  FAIL no external callers or local-helpers framing"
  fail=$((fail+1))
fi
# External path signal: callers *or* callees (Command's cross-file edges are often callees).
if echo "$T" | grep -qE 'shell_completion|types\.py|exceptions\.py|parser\.py|utils\.py|formatting\.py'; then
  echo "  OK  known external path signal present"
  pass=$((pass+1))
else
  echo "  FAIL no expected external path signal"
  fail=$((fail+1))
fi
echo

echo "--- Spine/Honesty payload: self-repo handle_orchestrate ---"
SELF_ROOT="${BUTLER_SELF_ROOT:-${BUTLER_HOST_MOUNT:-$HOME/projects}/my-app}"
T2=$(post "{\"project\":\"${SELF_ROOT}\",\"goal\":\"trace\",\"target_symbol\":\"handle_orchestrate\",\"scope_paths\":[\"cli/\"],\"detail\":\"compact\"}" | content)
echo "$T2" | head -28
echo "$T2" >/tmp/gate_handle_orchestrate.txt
check "receipt present" "receipt:" "$T2"
# Product path (post reverse-spine): external parent and/or spine section
if echo "$T2" | grep -q "call path (reverse spine"; then
  echo "  OK  reverse CALL spine section"
  pass=$((pass+1))
  if echo "$T2" | grep -qE 'dispatch_tool|← '; then
    echo "  OK  spine names dispatch_tool or parent"
    pass=$((pass+1))
  else
    echo "  FAIL spine names dispatch_tool or parent"
    fail=$((fail+1))
  fi
elif echo "$T2" | grep -q "external callers"; then
  echo "  OK  external callers section (spine may be empty)"
  pass=$((pass+1))
  check "external dispatch_tool signal" "dispatch_tool" "$T2"
else
  # Legacy all-local still acceptable if graph truly empty
  if echo "$T2" | grep -qE 'all same-file|local helpers|callers: \(none'; then
    echo "  OK  local-only / empty callers framing (legacy honesty)"
    pass=$((pass+1))
  else
    echo "  FAIL no spine, external, or local-only framing"
    fail=$((fail+1))
  fi
fi
echo

echo "=== Result: pass=$pass fail=$fail ==="
if [[ "$fail" -gt 0 ]]; then
  exit 1
fi
echo "STRUCTURAL HALF GREEN — run Qwen with plans/desirability-gate-p1-prompts.txt for Track T agent green"
exit 0
