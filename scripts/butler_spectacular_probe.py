#!/usr/bin/env python3
"""Butler spectacular probe — silent-lie mill (repo-agnostic, no LLM).

Mines multi-def name collisions from Trace locations, grades packs with a dual
oracle (Butler pack + path/import truth). Spectacular = high-confidence pack
that teaches a false hard structure (wrong CALL, peer listed as CALL, wrong ★).

Classes (taxonomy):
  Q  qualifier / package collapse   — CALL into wrong same-name def (path)
  P  peer_as_call                   — name_peer listed under callers
  H  wrong_star                     — pin file ≠ ★ preferred file
  X  cross_pin_leak                 — exclusive peer of B appears as CALL into A
  L  loc_fallback invent            — hop-1 CALL parents with warehouse reverse 0
  B  bridge / dual-stack FFI lie    — Export/IPC missing, junk host, false FFI
  S  reverse spine lie              — caller_path not CALL-honest (I5/I6 class)
  W  watcher lie                    — incremental edit/re-edge silent falsehood

Soft (not spectacular): missing dual-stack bridges, empty sample with warehouse
callers, BUILDING, missing keepers, ambiguous short pins (src/lib.rs shells).

Usage:
  python3 scripts/butler_spectacular_probe.py
  python3 scripts/butler_spectacular_probe.py -v --json plans/receipts/spectacular-latest.json
  python3 scripts/butler_spectacular_probe.py --roots gin,prometheus --seeds Default,DefaultOptions
  python3 scripts/butler_spectacular_probe.py --max-pins 4 --max-seeds-per-root 8
  python3 scripts/butler_spectacular_probe.py --ffi --skip-miner   # dual-stack lane only
  python3 scripts/butler_spectacular_probe.py --roots wasmtime --thorough --skip-watcher

Exit:
  0 — spectacular=0 (soft ignored unless --strict-soft)
  1 — spectacular > 0 (or soft>0 with --strict-soft)
  2 — infra (Butler down, no runnable roots)

Reports:
  plans/receipts/spectacular-latest.json
  plans/receipts/spectacular-<utc>.json
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from collections import defaultdict
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

REPO = Path(__file__).resolve().parents[1]
RECEIPTS = Path(os.environ.get("BUTLER_RECEIPTS_DIR", str(Path("/tmp") / "butler-alpha-receipts")))
DEFAULT_BASE = os.environ.get("BUTLER_URL", "http://127.0.0.1:8002").rstrip("/")
HOST_PROJECTS = Path(os.environ.get("BUTLER_HOST_MOUNT", str(Path.home() / "projects")))
CONT_PREFIX = os.environ.get("BUTLER_CONTAINER_MOUNT", "/projects")

# Collision-prone short names (repo-agnostic). Miner only uses those with ≥2 locs.
COLLISION_SEEDS: list[str] = [
    "Default",
    "DefaultOptions",
    "New",
    "Open",
    "open",
    "init",
    "create",
    "parse",
    "run",
    "search",
    "get",
    "bind",
    "main",
    "load",
    "start",
    "build",
    "handle",
    "Close",
    "close",
]

# Expanded set for --thorough (still multi-loc gated; order = priority).
THOROUGH_SEEDS: list[str] = list(
    dict.fromkeys(
        COLLISION_SEEDS
        + [
            "Init",
            "Create",
            "Parse",
            "Run",
            "Get",
            "Bind",
            "Main",
            "Load",
            "Start",
            "Build",
            "Handle",
            "Read",
            "Write",
            "Update",
            "Delete",
            "Connect",
            "connect",
            "Config",
            "config",
            "Error",
            "String",
            "execute",
            "Execute",
            "process",
            "Process",
            "render",
            "Render",
            "setup",
            "Setup",
            "reset",
            "Reset",
            "clone",
            "Clone",
            "copy",
            "Copy",
            "free",
            "Free",
            "alloc",
            "Alloc",
            "from",
            "From",
            "into",
            "Into",
            "new",
            "len",
            "Len",
            "Size",
            "size",
            "Type",
            "type",
            "Value",
            "value",
            "Name",
            "name",
            "Id",
            "ID",
            "Key",
            "key",
            "Set",
            "set",
            "Add",
            "add",
            "Remove",
            "remove",
            "Find",
            "find",
            "Lookup",
            "lookup",
            "Encode",
            "Decode",
            "encode",
            "decode",
            "Marshal",
            "Unmarshal",
            "Format",
            "format",
            "Print",
            "print",
            "Log",
            "log",
            "Debug",
            "Info",
            "Warn",
            "Fatal",
            "Serve",
            "serve",
            "Listen",
            "listen",
            "Dial",
            "dial",
            "Accept",
            "accept",
            "Send",
            "send",
            "Recv",
            "recv",
            "Push",
            "Pop",
            "push",
            "pop",
            "Lock",
            "Unlock",
            "lock",
            "unlock",
            "Wait",
            "wait",
            "Done",
            "done",
            "Stop",
            "stop",
            "Start",
            "Flush",
            "flush",
            "Sync",
            "sync",
            "Seek",
            "seek",
            "Stat",
            "stat",
            "Walk",
            "walk",
            "Visit",
            "visit",
            "Apply",
            "apply",
            "Call",
            "call",
            "Invoke",
            "invoke",
            "Dispatch",
            "dispatch",
            "Route",
            "route",
            "Match",
            "match",
            "Compare",
            "compare",
            "Equal",
            "Equal",
            "Hash",
            "hash",
            "Clear",
            "clear",
            "Reset",
            "Valid",
            "valid",
            "Check",
            "check",
            "Validate",
            "validate",
            "Convert",
            "convert",
            "Transform",
            "transform",
            "Compile",
            "compile",
            "Eval",
            "eval",
            "Exec",
            "exec",
            "Spawn",
            "spawn",
            "Fork",
            "fork",
            "Join",
            "join",
            "Split",
            "split",
            "Merge",
            "merge",
            "Sort",
            "sort",
            "Filter",
            "filter",
            "Map",
            "map",
            "Reduce",
            "Next",
            "next",
            "Prev",
            "prev",
            "First",
            "Last",
            "first",
            "last",
            "Head",
            "Tail",
            "Parent",
            "Child",
            "Root",
            "root",
            "Path",
            "path",
            "File",
            "file",
            "Dir",
            "dir",
            "Buffer",
            "buffer",
            "Bytes",
            "bytes",
            "Reader",
            "Writer",
            "reader",
            "writer",
            "Context",
            "context",
            "Cancel",
            "cancel",
            "Timeout",
            "timeout",
            "Option",
            "option",
            "Result",
            "result",
            "Status",
            "status",
            "State",
            "state",
            "Event",
            "event",
            "Handler",
            "handler",
            "Callback",
            "callback",
            "Listener",
            "listener",
            "Manager",
            "manager",
            "Factory",
            "factory",
            "Builder",
            "builder",
            "Client",
            "client",
            "Server",
            "server",
            "Request",
            "request",
            "Response",
            "response",
            "Header",
            "header",
            "Body",
            "body",
            "Token",
            "token",
            "Auth",
            "auth",
            "User",
            "user",
            "Session",
            "session",
            "Cache",
            "cache",
            "Store",
            "store",
            "Index",
            "index",
            "Query",
            "query",
            "Scan",
            "scan",
            "Iter",
            "iter",
            "Range",
            "range",
            "Slice",
            "slice",
            "Array",
            "array",
            "List",
            "list",
            "Dict",
            "dict",
            "Table",
            "table",
            "Node",
            "node",
            "Edge",
            "edge",
            "Graph",
            "graph",
            "Tree",
            "tree",
            "Heap",
            "heap",
            "Queue",
            "queue",
            "Stack",
            "stack",
            "Pool",
            "pool",
            "Worker",
            "worker",
            "Job",
            "job",
            "Task",
            "task",
            "Thread",
            "thread",
            "Mutex",
            "mutex",
            "Atomic",
            "atomic",
            "Once",
            "once",
            "Lazy",
            "lazy",
            "Ptr",
            "ptr",
            "Ref",
            "ref",
            "Box",
            "box",
            "Arc",
            "Rc",
            "Clone",
            "Drop",
            "drop",
            "Free",
            "Alloc",
            "realloc",
            "memcpy",
            "memset",
            "strcmp",
            "strlen",
            "printf",
            "sprintf",
            "fprintf",
            "scanf",
            "fopen",
            "fclose",
            "fread",
            "fwrite",
            "malloc",
            "calloc",
            "realloc",
            "free",
        ]
    )
)

# Default roots (host paths). Missing dirs are skipped.
DEFAULT_ROOTS: dict[str, Path] = {
    "gin": HOST_PROJECTS / "test_repos/gin",
    "prometheus": HOST_PROJECTS / "test_repos/prometheus",
    "word-count": HOST_PROJECTS / "test_repos/pyo3/examples/word-count",
    "fd": HOST_PROJECTS / "test_repos/fd",
    "click": HOST_PROJECTS / "test_repos/click",
    "fastapi-ts": HOST_PROJECTS / "test_repos/fastapi-ts",
    "self": HOST_PROJECTS / os.environ.get("BUTLER_SELF_REPO", "my-app"),
}

# Optional hand-pinned regressions (must stay green). Repo paths are fixtures only.
# Checks are dual-oracle facts, not framework special cases in the engine.
FIXED_CASES: list[dict[str, Any]] = [
    {
        "id": "R_gin_Default_pin_no_Bind_hard_call",
        "root": "gin",
        "seed": "Default",
        "scope": ["gin.go"],
        "class": "P",
        "checks": ["star_contains:gin.go", "callers_exclude_names:Bind,ShouldBind"],
    },
    {
        "id": "R_gin_Bind_callee_binding_not_gin_Default",
        "root": "gin",
        "seed": "Bind",
        "scope": ["context.go"],
        "class": "Q",
        "checks": [
            "star_contains:context.go",
            "callees_Default_file_contains:binding",
            "callees_Default_file_not_contains:gin.go",
        ],
    },
    {
        "id": "R_prom_tsdb_DefaultOptions_NewWithError_hard",
        "root": "prometheus",
        "seed": "DefaultOptions",
        "scope": ["tsdb/db.go"],
        "class": "Q",
        "checks": [
            "star_contains:tsdb/db.go",
            "callers_include_names:NewWithError",
            "peer_or_absent_names:validateOptions",
        ],
    },
    {
        "id": "R_prom_agent_DefaultOptions_NewWithError_peer",
        "root": "prometheus",
        "seed": "DefaultOptions",
        "scope": ["tsdb/agent/db.go"],
        "class": "P",
        "checks": [
            "star_contains:agent/db.go",
            "callers_exclude_names:NewWithError",
            "peers_include_names:NewWithError",
        ],
    },
    # Method-call honesty: storage.Close / ng.Close must not fan into LazyLoader.Close ★
    {
        "id": "R_prom_LazyLoader_Close_not_called_by_NewTestEngine",
        "root": "prometheus",
        "seed": "Close",
        "scope": ["promql/promqltest/test.go"],
        "class": "Q",
        "checks": [
            "star_contains:promqltest/test.go",
            "callers_exclude_names:NewTestEngineWithOpts,execRangeEval,runInstantQuery",
        ],
    },
]

# Class S: reverse CALL spine gold (caller_path honesty). Repo paths are fixtures only.
SPINE_GOLD: list[dict[str, Any]] = [
    {
        "id": "S_prom_DefaultOptions_spine0_FlushWAL",
        "root": "prometheus",
        "seed": "DefaultOptions",
        "scope": ["tsdb/db.go"],
        "checks": [
            "spine_nonempty",
            "spine0_name:FlushWAL",
            "spine0_in_hop1_callers",
            "spine_no_name_peer",
        ],
    },
    {
        "id": "S_fd_run_spine0_main",
        "root": "fd",
        "seed": "run",
        "scope": ["src/main.rs"],
        "checks": [
            "spine_nonempty",
            "spine0_name:main",
            "spine0_in_hop1_callers",
            "spine_no_name_peer",
        ],
    },
    {
        "id": "S_butler_collisions_spine0_handle",
        "root": "self",
        "seed": "multi_file_name_collisions",
        # P2 peel: method lives in name_index.rs (was model.rs).
        "scope": ["code_graph/src/snooper/name_index.rs"],
        "checks": [
            "spine_nonempty",
            "spine0_name:handle_collisions",
            "spine0_in_hop1_callers",
            "spine_no_name_peer",
        ],
    },
]


# Collision mine junk — keep in sync with code_graph is_collision_mine_junk.
COLLISION_MINE_JUNK: frozenset[str] = frozenset(
    {
        "unknown",
        "Unknown",
        "UNKNOWN",
        "tests",
        "test",
        "Test",
        "Some",
        "None",
        "Ok",
        "Err",
        "true",
        "false",
        "null",
        "nil",
        "undefined",
        "void",
        "int",
        "str",
        "bool",
        "float",
        "string",
        "bytes",
        "object",
        "any",
        "mod",
        "impl",
        "self",
        "Self",
        "crate",
        "super",
    }
)

# Common multi-crate path shells — pin is too coarse for spectacular P.
AMBIGUOUS_PIN_SHELLS: frozenset[str] = frozenset(
    {
        "src/lib.rs",
        "src/main.rs",
        "src/mod.rs",
        "lib.rs",
        "main.rs",
        "mod.rs",
        "main.go",
        "__init__.py",
        "index.ts",
        "index.js",
        "index.tsx",
        "index.jsx",
    }
)


@dataclass
class Finding:
    cls: str  # Q|P|H|X|L|B|S|W|soft
    severity: str  # spectacular | soft
    root: str
    seed: str
    pin: str
    detail: str
    star: str = ""
    receipt: str = ""
    evidence: dict[str, Any] = field(default_factory=dict)


def is_collision_mine_junk(name: str) -> bool:
    n = (name or "").strip()
    if not n or n in COLLISION_MINE_JUNK:
        return True
    alnum = sum(1 for c in n if c.isalnum() or c == "_")
    return alnum < 3


def pin_is_ambiguous_shell(pin: str) -> bool:
    """True when pin is a basename/shell shared by many crates (mill FP class)."""
    p = norm_path(pin).lstrip("./")
    if p in AMBIGUOUS_PIN_SHELLS:
        return True
    parts = [x for x in p.split("/") if x]
    if len(parts) <= 2 and parts and parts[-1] in {
        "lib.rs",
        "main.rs",
        "mod.rs",
        "main.go",
        "__init__.py",
        "index.ts",
        "index.js",
        "index.tsx",
        "index.jsx",
    }:
        return True
    return False


def log(msg: str) -> None:
    print(msg, flush=True)


def host_to_container(path: str | Path) -> str:
    p = str(path).rstrip("/")
    h = str(HOST_PROJECTS).rstrip("/")
    if p == h or p.startswith(h + "/"):
        return CONT_PREFIX + p[len(h) :]
    return p


def container_to_host(path: str) -> str:
    p = path.rstrip("/")
    c = CONT_PREFIX.rstrip("/")
    if p == c or p.startswith(c + "/"):
        return str(HOST_PROJECTS) + p[len(c) :]
    return p


def http_json(
    url: str,
    *,
    body: Optional[dict] = None,
    timeout: float = 120.0,
) -> dict[str, Any]:
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        url,
        data=data,
        headers={"Content-Type": "application/json"},
        method="GET" if body is None else "POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode())


def post_context(base: str, payload: dict, timeout: float = 120.0) -> dict[str, Any]:
    return http_json(f"{base}/context", body=payload, timeout=timeout)


def structured(j: dict) -> dict[str, Any]:
    sc = j.get("structured")
    if isinstance(sc, dict) and sc:
        return sc
    # Some paths put fields at top level
    if isinstance(j.get("target"), dict):
        return j
    return {}


def content_text(j: dict) -> str:
    c = j.get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list) and c:
        return str((c[0] or {}).get("text") or "")
    return ""


def norm_path(p: str) -> str:
    s = (p or "").replace("\\", "/").lstrip("./")
    # Drop host/container mounts so hard/peer keys match (with or without leading /)
    s_stripped = s.lstrip("/")
    for prefix in (
        str(HOST_PROJECTS).replace("\\", "/").rstrip("/").lstrip("/") + "/",
        CONT_PREFIX.rstrip("/").lstrip("/") + "/",
        "home/user/projects/",
        "projects/",
    ):
        if s_stripped.startswith(prefix):
            s_stripped = s_stripped[len(prefix) :]
            break
    s = s_stripped
    # Prefer path under known repo folder names
    for marker in ("test_repos/", "my-app/"):
        if marker in s:
            s = s[s.index(marker) + len(marker) :]
            break
    return s


def basenames(p: str) -> str:
    return norm_path(p).rsplit("/", 1)[-1]


def path_contains(hay: str, needle: str) -> bool:
    return needle.replace("\\", "/") in norm_path(hay)


def wait_building(
    base: str,
    payload: dict,
    *,
    max_tries: int = 40,
    sleep_s: float = 1.5,
) -> tuple[dict[str, Any], dict[str, Any], str]:
    """Retry Trace while status BUILDING; return (raw, structured, content)."""
    last: dict[str, Any] = {}
    sc: dict[str, Any] = {}
    text = ""
    for i in range(max_tries):
        try:
            last = post_context(base, payload, timeout=180.0)
        except Exception as e:
            if i + 1 >= max_tries:
                return {"error": str(e)}, {}, str(e)
            time.sleep(sleep_s)
            continue
        sc = structured(last)
        text = content_text(last)
        status = (last.get("status") or sc.get("status") or "").upper()
        if status in ("BUILDING", "BUILDING_SOFT_WALL") or "Building Graph" in text:
            time.sleep(sleep_s)
            continue
        if sc.get("target") or sc.get("blast_domain") == "disambiguate" or sc.get("error"):
            return last, sc, text
        # empty but not building
        if i > 2:
            return last, sc, text
        time.sleep(sleep_s)
    return last, sc, text


def receipt_bits(sc: dict, text: str) -> dict[str, str]:
    r = sc.get("receipt") if isinstance(sc.get("receipt"), dict) else {}
    conf = str(r.get("confidence") or "").lower()
    edges = str(r.get("edges") or "").lower()
    basis = str(r.get("basis") or "").lower()
    if not conf and "receipt:" in text:
        m = re.search(r"receipt:\s*confidence=(\w+)", text, re.I)
        if m:
            conf = m.group(1).lower()
    return {"confidence": conf, "edges": edges, "basis": basis}


def is_high_complete(rec: dict[str, str]) -> bool:
    if rec.get("basis") in ("error", "scope_not_found"):
        return False
    if rec.get("confidence") in ("low", "error", ""):
        # still treat complete ladder from telemetry
        pass
    high = rec.get("confidence") in ("high", "medium") or rec.get("confidence") == ""
    complete = "complete" in rec.get("edges", "") or rec.get("edges") == ""
    return bool(high and complete)


def star_str(sc: dict) -> str:
    t = sc.get("target") or {}
    if not t:
        return ""
    return f"{t.get('name')} @ {t.get('file')}:{t.get('line')}"


def serious_locations(sc: dict) -> list[dict]:
    locs = sc.get("locations") or []
    out: list[dict] = []
    for loc in locs:
        f = norm_path(loc.get("file") or "")
        fl = f.lower()
        if any(x in fl for x in ("/benchmarks/", "/benches/", "/.git/")):
            continue
        kind = (loc.get("kind") or "").lower()
        if "call_expression" in kind or kind in ("expression_statement", "string"):
            continue
        # Prefer defs
        if any(
            k in kind
            for k in (
                "function",
                "method",
                "class",
                "struct",
                "type",
                "impl",
            )
        ) or not kind:
            out.append(loc)
    return out if out else list(locs)


def pin_from_loc(loc: dict) -> str:
    f = norm_path(loc.get("file") or "")
    # Prefer last 2–3 components for monorepo-relative pins
    parts = [p for p in f.split("/") if p]
    if len(parts) >= 2:
        return "/".join(parts[-2:]) if parts[-1].endswith((".go", ".py", ".rs", ".ts", ".js", ".c", ".cpp", ".h")) else parts[-1]
    return basenames(f)


def neighbor_names(rows: list) -> set[str]:
    return {(r.get("name") or "").strip() for r in rows if (r.get("name") or "").strip()}


def neighbor_keys(rows: list) -> set[tuple[str, str]]:
    """(name, normalized file) — avoid cross-homonym false P/X."""
    out: set[tuple[str, str]] = set()
    for r in rows:
        n = (r.get("name") or "").strip()
        if not n:
            continue
        out.add((n, norm_path(r.get("file") or "")))
    return out


def neighbor_files(rows: list, name: str) -> list[str]:
    out = []
    for r in rows:
        if (r.get("name") or "").strip() == name:
            out.append(norm_path(r.get("file") or ""))
    return out


def is_stdlib_import_path(ipath: str) -> bool:
    """Go stdlib has no dotted first path segment (errors, os, net/http)."""
    first = (ipath or "").split("/")[0]
    return "." not in first


def grade_fixed(sc: dict, checks: list[str], text: str) -> tuple[bool, str, str]:
    """Return (ok, detail, severity). severity spectacular|soft."""
    if not sc or sc.get("error"):
        return False, f"empty/error pack: {sc.get('error') or 'no structured'}", "spectacular"
    t = sc.get("target") or {}
    star_file = norm_path(t.get("file") or "")
    callers = sc.get("callers") or []
    peers = sc.get("peer_callers") or []
    callees = sc.get("callees") or []
    c_names = neighbor_names(callers)
    p_names = neighbor_names(peers)
    # Peer relation leaked into callers?
    for c in callers:
        if (c.get("relation") or "").lower() == "name_peer":
            return False, f"caller {c.get('name')} has relation=name_peer", "spectacular"

    for chk in checks:
        if chk.startswith("star_contains:"):
            needle = chk.split(":", 1)[1]
            if not path_contains(star_file, needle):
                return False, f"★ file {star_file!r} missing {needle!r}", "spectacular"
        elif chk.startswith("callers_include_names:"):
            names = chk.split(":", 1)[1].split(",")
            missing = [n for n in names if n not in c_names]
            if missing:
                # may be sample-capped — soft if warehouse degree high
                tel = sc.get("telemetry") or {}
                in_d = int(tel.get("seed_in_degree") or 0)
                if in_d > 20:
                    return False, f"callers sample missing {missing} (in={in_d})", "soft"
                return False, f"callers missing {missing}; have {sorted(c_names)[:12]}", "spectacular"
        elif chk.startswith("callers_exclude_names:"):
            names = chk.split(":", 1)[1].split(",")
            bad = [n for n in names if n in c_names]
            if bad:
                return False, f"hard callers must not include {bad}", "spectacular"
        elif chk.startswith("peers_include_names:"):
            names = chk.split(":", 1)[1].split(",")
            missing = [n for n in names if n not in p_names]
            if missing:
                return False, f"peer_callers missing {missing}; peers={sorted(p_names)[:12]}", "spectacular"
        elif chk.startswith("peer_or_absent_names:"):
            # Name may be peer or absent; must not be hard caller
            names = chk.split(":", 1)[1].split(",")
            bad = [n for n in names if n in c_names and n not in p_names]
            # if in callers at all, spectacular
            bad2 = [n for n in names if n in c_names]
            if bad2:
                return False, f"{bad2} must not be hard CALL (use peer or omit)", "spectacular"
        elif chk.startswith("callees_Default_file_contains:"):
            needle = chk.split(":", 1)[1]
            files = neighbor_files(callees, "Default")
            if not files:
                # hop-2 only or packed out — soft
                return False, "no Default in callees sample", "soft"
            if not any(path_contains(f, needle) for f in files):
                return False, f"Default callees {files} missing path {needle}", "spectacular"
        elif chk.startswith("callees_Default_file_not_contains:"):
            needle = chk.split(":", 1)[1]
            files = neighbor_files(callees, "Default")
            bad = [f for f in files if path_contains(f, needle) and "binding" not in f]
            # gin.go specifically
            if needle == "gin.go":
                bad = [f for f in files if f.endswith("gin.go") or f.endswith("/gin.go")]
            if bad:
                return False, f"Default callees wrongly hit {bad}", "spectacular"
        else:
            return False, f"unknown check {chk}", "soft"
    return True, "ok", "soft"


def grade_spine_S(sc: dict, *, seed: str, pin: str = "") -> list[Finding]:
    """Class S: reverse spine (caller_path) structural honesty on any Trace pack."""
    findings: list[Finding] = []
    if not sc or not sc.get("target"):
        return findings
    rec = receipt_bits(sc, "")
    domain = (sc.get("blast_domain") or "").lower()
    seed_kind = (sc.get("seed_kind") or (sc.get("target") or {}).get("kind") or "").lower()
    spine = sc.get("caller_path") or []
    hop1 = [c for c in (sc.get("callers") or []) if int(c.get("hop") or 1) <= 1]
    hop1_names = {(c.get("name") or "").strip() for c in hop1 if (c.get("name") or "").strip()}

    # Type seeds must not invent a reverse CALL spine.
    if domain == "type_neighborhood" or any(
        x in seed_kind for x in ("struct", "class_specifier", "class_definition", "interface")
    ):
        if spine and is_high_complete(rec):
            findings.append(
                Finding(
                    "S",
                    "spectacular",
                    "",
                    seed,
                    pin,
                    f"type/domain seed has non-empty caller_path ({len(spine)} steps)",
                    star=star_str(sc),
                    evidence={"spine": [s.get("name") for s in spine[:6]]},
                )
            )
        return findings

    if not spine:
        return findings

    # Spine steps must never be labeled name_peer (CALL-only path).
    for i, s in enumerate(spine):
        rel = (s.get("relation") or "").lower()
        if rel == "name_peer":
            findings.append(
                Finding(
                    "S",
                    "spectacular" if is_high_complete(rec) else "soft",
                    "",
                    seed,
                    pin,
                    f"caller_path[{i}] {s.get('name')} has relation=name_peer (spine is CALL only)",
                    star=star_str(sc),
                    evidence={"step": s},
                )
            )

    # spine[0] must be a hop-1 hard caller when hop-1 sample is non-empty (I6).
    s0 = spine[0]
    s0_name = (s0.get("name") or "").strip()
    if s0_name and hop1_names and s0_name not in hop1_names:
        findings.append(
            Finding(
                "S",
                "spectacular" if is_high_complete(rec) else "soft",
                "",
                seed,
                pin,
                f"spine[0]={s0_name} not in hop-1 callers {sorted(hop1_names)[:12]}",
                star=star_str(sc),
                evidence={
                    "spine0": s0,
                    "hop1": sorted(hop1_names)[:20],
                },
            )
        )

    # Hop field should be 1..n along the path (not all hop=1 invent).
    for i, s in enumerate(spine):
        h = int(s.get("hop") or 0)
        if h and h != i + 1:
            findings.append(
                Finding(
                    "S",
                    "soft",
                    "",
                    seed,
                    pin,
                    f"caller_path[{i}] hop={h} expected {i + 1}",
                    star=star_str(sc),
                )
            )
            break

    return findings


def grade_spine_gold_checks(
    sc: dict, checks: list[str], *, seed: str, pin: str
) -> tuple[bool, str, str]:
    """Return (ok, detail, severity) for SPINE_GOLD check list."""
    if not sc or not sc.get("target"):
        return False, "no target", "spectacular"
    spine = sc.get("caller_path") or []
    hop1 = [c for c in (sc.get("callers") or []) if int(c.get("hop") or 1) <= 1]
    hop1_names = {(c.get("name") or "").strip() for c in hop1}
    rec = receipt_bits(sc, "")
    for chk in checks:
        if chk == "spine_nonempty":
            if not spine:
                sev = "spectacular" if is_high_complete(rec) else "soft"
                return False, "caller_path empty (expected reverse spine)", sev
        elif chk.startswith("spine0_name:"):
            want = chk.split(":", 1)[1]
            got = (spine[0].get("name") if spine else "") or ""
            if got != want:
                return (
                    False,
                    f"spine[0]={got!r} want {want!r}",
                    "spectacular" if is_high_complete(rec) else "soft",
                )
        elif chk == "spine0_in_hop1_callers":
            if not spine:
                continue
            s0 = (spine[0].get("name") or "").strip()
            if hop1_names and s0 not in hop1_names:
                return (
                    False,
                    f"spine[0]={s0} not in hop-1 {sorted(hop1_names)[:10]}",
                    "spectacular" if is_high_complete(rec) else "soft",
                )
            if not hop1_names and s0:
                # Empty hop-1 with non-empty spine is bootstrap-only — soft unless high complete
                if is_high_complete(rec):
                    return False, f"spine[0]={s0} but hop-1 callers empty", "soft"
        elif chk == "spine_no_name_peer":
            for i, s in enumerate(spine):
                if (s.get("relation") or "").lower() == "name_peer":
                    return (
                        False,
                        f"caller_path[{i}] relation=name_peer",
                        "spectacular",
                    )
        else:
            return False, f"unknown spine check {chk}", "soft"
    return True, "ok", "soft"


def grade_loc_fallback_L(sc: dict, *, seed: str, pin: str) -> list[Finding]:
    """Class L: hop-1 CALL parents invented via loc_fallback (not warehouse reverse).

    Product lie: pack teaches hard callers of ★ from enclosing callables of
    *same-name call sites* (often other twins). Engine sets
    ``telemetry.callers_loc_fallback=true`` when that path fires.

    **Not** a lie when warehouse reverse is non-zero (real CALL edges). Do not
    treat missing telemetry as warehouse_in=0 — that FP'd the L mill on fmt/spdlog.
    """
    findings: list[Finding] = []
    if not sc or not sc.get("target"):
        return findings
    tel = sc.get("telemetry") if isinstance(sc.get("telemetry"), dict) else {}
    rec = receipt_bits(sc, "")
    # Require explicit invent flag — secondary "wh missing → 0" path was almost all FPs.
    if not bool(tel.get("callers_loc_fallback")):
        return findings
    direct = [
        c
        for c in (sc.get("callers") or [])
        if int(c.get("hop") or 1) <= 1
    ]
    if not direct:
        return findings
    # Prefer explicit warehouse reverse when present; invent with warehouse>0 is odd.
    wh_raw = tel.get("seed_in_degree_warehouse")
    wh: Optional[int]
    if isinstance(wh_raw, (int, float)):
        wh = int(wh_raw)
    else:
        wh = None
    if wh is not None and wh > 0:
        # Real reverse exists; loc_fallback should not have run. Soft note only.
        return findings
    fb_names = tel.get("callers_loc_fallback_names") or []
    if not isinstance(fb_names, list):
        fb_names = []
    names = sorted(
        {
            (c.get("name") or "").strip()
            for c in direct
            if (c.get("name") or "").strip()
        }
    )
    if not names:
        return findings
    ambig = pin_is_ambiguous_shell(pin)
    if is_high_complete(rec):
        sev = "soft" if ambig else "spectacular"
    else:
        sev = "soft"
    detail = (
        f"hop-1 callers {names[:8]} invented via loc_fallback"
        + (f" (warehouse reverse={wh})" if wh is not None else "")
        + (f" · fb_names={fb_names[:8]}" if fb_names else "")
        + (" · ambiguous pin" if ambig else "")
    )
    findings.append(
        Finding(
            "L",
            sev,
            "",
            seed,
            pin,
            detail,
            star=star_str(sc),
            receipt=f"{rec.get('confidence')}|{rec.get('basis')}|{rec.get('edges')}",
            evidence={
                "warehouse_in": wh,
                "loc_fallback": True,
                "hop1_callers": names[:12],
                "loc_fallback_names": fb_names[:12],
                "ambiguous_pin": ambig,
            },
        )
    )
    return findings


def grade_mined_pin(
    sc: dict,
    *,
    seed: str,
    pin: str,
    other_pins: list[str],
    peer_maps: dict[str, set[str]],
) -> list[Finding]:
    """Grade one pin Trace. peer_maps: pin_key -> set of hard-caller names from other pins."""
    findings: list[Finding] = []
    if not sc or not sc.get("target"):
        domain = (sc.get("blast_domain") or "").lower() if sc else ""
        if domain == "disambiguate":
            return findings  # not a pin success; skip
        findings.append(
            Finding(
                cls="H",
                severity="soft",
                root="",
                seed=seed,
                pin=pin,
                detail="no target on pin Trace",
            )
        )
        return findings

    rec = receipt_bits(sc, "")
    star = star_str(sc)
    t = sc.get("target") or {}
    star_file = norm_path(t.get("file") or "")
    callers = sc.get("callers") or []
    peers = sc.get("peer_callers") or []
    # Hop-1 hard CALL only for mixed peer checks (hop≥2 is transitive neighborhood).
    direct_callers = [c for c in callers if int(c.get("hop") or 1) <= 1]
    c_keys = neighbor_keys(direct_callers)
    p_keys = neighbor_keys(peers)

    # L: loc_fallback invented hop-1 callers (warehouse reverse empty)
    findings.extend(grade_loc_fallback_L(sc, seed=seed, pin=pin))
    # S: reverse spine honesty
    findings.extend(grade_spine_S(sc, seed=seed, pin=pin))

    # H: pin vs ★
    pin_base = basenames(pin)
    if pin_base and pin_base not in star_file and not path_contains(star_file, pin):
        # allow pin "tsdb/db.go" vs file ".../tsdb/db.go"
        if not any(path_contains(star_file, p) for p in (pin, pin_base)):
            sev = "spectacular" if is_high_complete(rec) else "soft"
            findings.append(
                Finding(
                    "H",
                    sev,
                    "",
                    seed,
                    pin,
                    f"★ file {star_file!r} does not match pin {pin!r}",
                    star=star,
                    receipt=f"{rec.get('confidence')}|{rec.get('basis')}|{rec.get('edges')}",
                )
            )

    # Ambiguous short pins (src/lib.rs) create mill FPs on multi-crate monorepos;
    # still report P but demote to soft so tier-2 noise does not block the gate.
    ambig = pin_is_ambiguous_shell(pin)
    p_sev = "soft" if ambig else "spectacular"

    # P: name_peer relation inside callers
    for c in callers:
        rel = (c.get("relation") or "").lower()
        if rel == "name_peer":
            findings.append(
                Finding(
                    "P",
                    p_sev,
                    "",
                    seed,
                    pin,
                    f"caller {c.get('name')} has relation=name_peer in callers[]"
                    + (" (ambiguous pin)" if ambig else ""),
                    star=star,
                    receipt=f"{rec.get('confidence')}|{rec.get('basis')}|{rec.get('edges')}",
                    evidence={"caller": c, "ambiguous_pin": ambig},
                )
            )

    # Same (name,file) must not be both hop-1 hard CALL and peer on one pin
    for name, file_s in sorted(c_keys & p_keys):
        findings.append(
            Finding(
                "P",
                p_sev,
                "",
                seed,
                pin,
                f"{name} @ {file_s} is both hop-1 hard caller and peer_caller on same pin"
                + (" (ambiguous pin)" if ambig else ""),
                star=star,
                evidence={"ambiguous_pin": ambig},
            )
        )

    # Cross-pin: same (name,file) hard on two different pins AND peer on one → mixed
    for other_pin, other_hard in peer_maps.items():
        if other_pin == pin:
            continue
        # other_hard is set of names only for compact maps — upgraded below in miner
        if not other_hard:
            continue
        # when peer_maps carries names only (legacy): skip X name-only to cut noise
        pass

    return findings


def _go_star_matches_import(star_file: str, import_path: str) -> bool:
    """True when ★ path belongs to the imported package (directory match)."""
    ipath = (import_path or "").rstrip("/")
    if not ipath or not star_file:
        return False
    pkg = ipath.rsplit("/", 1)[-1]
    star_dir = star_file.rsplit("/", 1)[0] if "/" in star_file else ""
    star_pkg = star_dir.rsplit("/", 1)[-1] if star_dir else ""
    if star_pkg == pkg or path_contains(star_file, f"/{pkg}/") or star_file.endswith(
        f"/{pkg}.go"
    ):
        return True
    # Nested import: last two segments appear in path (pkg/syserror ↔ …/pkg/syserror/).
    if "/" in ipath:
        suffix = "/".join(ipath.split("/")[-2:])
        if path_contains(star_file, f"/{suffix}/") or path_contains(
            star_file, f"/{suffix}.go"
        ):
            return True
        # Full path suffix after module host (…/private/pkg/syserror)
        segs = [s for s in ipath.split("/") if s]
        if len(segs) >= 3 and segs[0].count(".") >= 1:
            # drop host/org/repo-ish first 3 when present
            for start in (3, 2, 1):
                if start < len(segs):
                    rest = "/".join(segs[start:])
                    if rest and path_contains(star_file, f"/{rest}/"):
                        return True
    return False


def go_import_oracle_false_call(
    host_root: Path,
    sc: dict,
    seed: str,
) -> list[Finding]:
    """Go dual oracle: hard CALL into ★ must be justified by a matching package call.

    A caller may invoke *many* same-name package-qualified symbols (buf: both
    ``syserror.New`` and ``incremental.New``). Flag Q only when the hop-1 hard
    parent has package-qualified ``alias.seed(`` for **other** packages and **no**
    package-qualified call that matches ★'s package. That is the false-collapse
    pattern (edge into ★ without a supporting import call).

    Does not flag when a matching package call exists (true multi-New file).
    """
    findings: list[Finding] = []
    t = sc.get("target") or {}
    star_file = norm_path(t.get("file") or "")
    if not star_file.endswith(".go") and ".go" not in star_file:
        return findings
    callers = sc.get("callers") or []
    for c in callers:
        if (c.get("hop") or 1) > 1:
            continue
        cfile = norm_path(c.get("file") or "")
        if not cfile.endswith(".go"):
            continue
        host_file = _resolve_host_file(host_root, cfile)
        if not host_file:
            continue
        try:
            src = host_file.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        # Collect import alias → path (skip stdlib — errors.New / os.Open noise)
        imports: dict[str, str] = {}
        for m in re.finditer(
            r'^\s*(?:([A-Za-z_]\w*)\s+)?"([^"]+)"\s*$',
            src,
            re.M,
        ):
            path = m.group(2)
            if is_stdlib_import_path(path):
                continue
            alias = m.group(1) or path.rsplit("/", 1)[-1]
            if alias in (".", "_"):
                continue
            imports[alias] = path

        matching: list[tuple[str, str]] = []
        mismatched: list[tuple[str, str]] = []
        for alias, ipath in imports.items():
            if not re.search(rf"\b{re.escape(alias)}\.{re.escape(seed)}\s*\(", src):
                continue
            if _go_star_matches_import(star_file, ipath):
                matching.append((alias, ipath))
            else:
                mismatched.append((alias, ipath))

        # True edge: at least one package-qualified call supports ★.
        if matching:
            continue
        # No supporting call; only other packages' seed() — false collapse into ★.
        if not mismatched:
            continue
        alias, ipath = mismatched[0]
        pkg = ipath.rstrip("/").rsplit("/", 1)[-1]
        star_dir = star_file.rsplit("/", 1)[0] if "/" in star_file else ""
        star_pkg = star_dir.rsplit("/", 1)[-1] if star_dir else ""
        findings.append(
            Finding(
                "Q",
                "spectacular",
                "",
                seed,
                star_file,
                f"caller {c.get('name')} @ {cfile} has {alias}.{seed} "
                f"(import {ipath}) but no package call matching ★ {star_file} "
                f"(pkg~{star_pkg}≠{pkg}; mismatched={len(mismatched)})",
                star=star_str(sc),
                evidence={
                    "caller_file": cfile,
                    "import": ipath,
                    "alias": alias,
                    "star_file": star_file,
                    "mismatched": [
                        {"alias": a, "import": p} for a, p in mismatched[:6]
                    ],
                    "matching": [],
                },
            )
        )
    return findings


def _resolve_host_file(host_root: Path, rel_or_abs: str) -> Optional[Path]:
    cfile = norm_path(rel_or_abs)
    if not cfile:
        return None
    host_file = host_root / cfile
    if host_file.is_file():
        return host_file
    # Absolute host/container paths
    for prefix in (
        str(HOST_PROJECTS) + "/",
        CONT_PREFIX + "/",
        "/home/user/projects/",
        "/projects/",
    ):
        if cfile.startswith(prefix):
            cand = Path(str(HOST_PROJECTS) + "/" + cfile[len(prefix) :])
            if cand.is_file():
                return cand
    abs_p = Path(cfile)
    if abs_p.is_file():
        return abs_p
    matches = list(host_root.rglob(basenames(cfile)))
    if len(matches) == 1 and matches[0].is_file():
        return matches[0]
    return None


def _py_path_affinity(star_file: str, module: str) -> bool:
    """Loose dual-oracle: ★ path should mention module segments (repo-local)."""
    m = (module or "").strip().lstrip(".")
    if not m:
        return True
    # stdlib / external: cannot verify path — do not flag
    first = m.split(".", 1)[0]
    if first in {
        "os",
        "sys",
        "re",
        "json",
        "typing",
        "collections",
        "pathlib",
        "functools",
        "itertools",
        "subprocess",
        "asyncio",
        "logging",
        "http",
        "urllib",
        "abc",
        "enum",
        "dataclasses",
        "contextlib",
        "importlib",
        "unittest",
        "pytest",
        "numpy",
        "pandas",
        "torch",
        "django",
        "flask",
        "click",
        "typer",
        "fastapi",
        "pydantic",
        "requests",
        "httpx",
        "sqlalchemy",
    }:
        return True
    sf = star_file.replace("\\", "/").lower()
    segs = [s for s in m.replace(".", "/").lower().split("/") if s and s != "__init__"]
    if not segs:
        return True
    # Any module segment in path, or last segment as file stem
    if any(f"/{s}/" in f"/{sf}/" or sf.endswith(f"/{s}.py") for s in segs):
        return True
    last = segs[-1]
    return last in sf


def python_import_oracle_false_call(
    host_root: Path,
    sc: dict,
    seed: str,
) -> list[Finding]:
    """Python dual oracle: import-bound call must not collapse to wrong same-name ★.

    Flags when a hop-1 caller file does `import mod` / `from mod import seed` and
    invokes the bound name, but ★ path has zero affinity with that module.
    """
    findings: list[Finding] = []
    t = sc.get("target") or {}
    star_file = norm_path(t.get("file") or "")
    if not star_file.endswith(".py") and ".py" not in star_file:
        return findings
    for c in sc.get("callers") or []:
        if int(c.get("hop") or 1) > 1:
            continue
        cfile = norm_path(c.get("file") or "")
        if not cfile.endswith(".py"):
            continue
        host_file = _resolve_host_file(host_root, cfile)
        if not host_file:
            continue
        try:
            src = host_file.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        # import mod [as alias]  /  import pkg.mod as alias
        imports: dict[str, str] = {}
        for m in re.finditer(
            r"^\s*import\s+([A-Za-z_][\w.]*)(?:\s+as\s+([A-Za-z_]\w*))?\s*$",
            src,
            re.M,
        ):
            mod = m.group(1)
            alias = m.group(2) or mod.split(".", 1)[0]
            imports[alias] = mod
        # from mod import name [as alias]
        for m in re.finditer(
            r"^\s*from\s+([A-Za-z_.][\w.]*)\s+import\s+(.+)$",
            src,
            re.M,
        ):
            mod = m.group(1)
            rest = m.group(2).split("#", 1)[0].strip().rstrip("\\")
            if rest.startswith("("):
                continue  # multi-line — skip (conservative)
            for part in rest.split(","):
                part = part.strip()
                if not part or part == "*":
                    continue
                if " as " in part:
                    exp, alias = part.split(" as ", 1)
                    imports[alias.strip()] = f"{mod}.{exp.strip()}"
                else:
                    imports[part] = f"{mod}.{part}"
        # Attribute call: alias.seed(
        for alias, mod in imports.items():
            if not re.search(rf"\b{re.escape(alias)}\.{re.escape(seed)}\s*\(", src):
                # bare alias() when from mod import seed as alias
                if not (
                    mod.endswith(f".{seed}")
                    and re.search(rf"\b{re.escape(alias)}\s*\(", src)
                ):
                    continue
            if not _py_path_affinity(star_file, mod):
                findings.append(
                    Finding(
                        "Q",
                        "spectacular",
                        "",
                        seed,
                        star_file,
                        f"caller {c.get('name')} @ {cfile} uses import-bound "
                        f"{alias}/{mod} for {seed} but ★ is {star_file}",
                        star=star_str(sc),
                        evidence={
                            "caller_file": cfile,
                            "import": mod,
                            "alias": alias,
                            "star_file": star_file,
                        },
                    )
                )
    return findings


def callee_side_q_oracle(
    host_root: Path,
    sc: dict,
    seed: str,
) -> list[Finding]:
    """Callee-side Q: ★ body package/import-qualified call must land on matching path.

    Mirror of caller-side go/python oracles — catches Bind→wrong Default class when
    the false edge is on the outbound (callee) side.
    """
    findings: list[Finding] = []
    t = sc.get("target") or {}
    star_file = norm_path(t.get("file") or "")
    host_star = _resolve_host_file(host_root, star_file)
    if not host_star:
        return findings
    try:
        src = host_star.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return findings
    callees = [
        c
        for c in (sc.get("callees") or [])
        if int(c.get("hop") or 1) <= 1 and (c.get("name") or "").strip()
    ]
    if not callees:
        return findings

    # Go: alias.Name(
    if star_file.endswith(".go") or host_star.suffix == ".go":
        imports: dict[str, str] = {}
        for m in re.finditer(
            r'^\s*(?:([A-Za-z_]\w*)\s+)?"([^"]+)"\s*$',
            src,
            re.M,
        ):
            path = m.group(2)
            if is_stdlib_import_path(path):
                continue
            alias = m.group(1) or path.rsplit("/", 1)[-1]
            if alias in (".", "_"):
                continue
            imports[alias] = path
        for alias, ipath in imports.items():
            pkg = ipath.rstrip("/").rsplit("/", 1)[-1]
            for cal in callees:
                cname = (cal.get("name") or "").strip()
                if not re.search(
                    rf"\b{re.escape(alias)}\.{re.escape(cname)}\s*\(", src
                ):
                    continue
                cfile = norm_path(cal.get("file") or "")
                cdir = cfile.rsplit("/", 1)[0] if "/" in cfile else ""
                cpkg = cdir.rsplit("/", 1)[-1] if cdir else ""
                ok = (
                    cpkg == pkg
                    or path_contains(cfile, f"/{pkg}/")
                    or cfile.endswith(f"/{pkg}.go")
                )
                if not ok and pkg and cpkg and cpkg != pkg:
                    findings.append(
                        Finding(
                            "Q",
                            "spectacular",
                            "",
                            seed,
                            star_file,
                            f"★ {seed} @ {star_file} calls {alias}.{cname} "
                            f"(import {ipath}) but callee is {cfile} (pkg~{cpkg}≠{pkg})",
                            star=star_str(sc),
                            evidence={
                                "star_file": star_file,
                                "callee_file": cfile,
                                "import": ipath,
                                "alias": alias,
                                "callee": cname,
                            },
                        )
                    )

    # Python: alias.name(
    if star_file.endswith(".py") or host_star.suffix == ".py":
        imports_py: dict[str, str] = {}
        for m in re.finditer(
            r"^\s*import\s+([A-Za-z_][\w.]*)(?:\s+as\s+([A-Za-z_]\w*))?\s*$",
            src,
            re.M,
        ):
            mod = m.group(1)
            alias = m.group(2) or mod.split(".", 1)[0]
            imports_py[alias] = mod
        for m in re.finditer(
            r"^\s*from\s+([A-Za-z_.][\w.]*)\s+import\s+(.+)$",
            src,
            re.M,
        ):
            mod = m.group(1)
            rest = m.group(2).split("#", 1)[0].strip()
            if rest.startswith("("):
                continue
            for part in rest.split(","):
                part = part.strip()
                if not part or part == "*" or " as " in part:
                    if " as " in part:
                        exp, al = part.split(" as ", 1)
                        imports_py[al.strip()] = f"{mod}.{exp.strip()}"
                    continue
                imports_py[part] = f"{mod}.{part}"
        for alias, mod in imports_py.items():
            for cal in callees:
                cname = (cal.get("name") or "").strip()
                if not re.search(
                    rf"\b{re.escape(alias)}\.{re.escape(cname)}\s*\(", src
                ):
                    continue
                cfile = norm_path(cal.get("file") or "")
                if not _py_path_affinity(cfile, mod):
                    findings.append(
                        Finding(
                            "Q",
                            "spectacular",
                            "",
                            seed,
                            star_file,
                            f"★ {seed} @ {star_file} calls {alias}.{cname} "
                            f"(import {mod}) but callee is {cfile}",
                            star=star_str(sc),
                            evidence={
                                "star_file": star_file,
                                "callee_file": cfile,
                                "import": mod,
                                "alias": alias,
                                "callee": cname,
                            },
                        )
                    )
    return findings


def run_ffi_oracle(base: str, verbose: bool) -> list[Finding]:
    """Class B: dual-stack Export/IPC gold holes via butler_ffi_hole_probe."""
    import subprocess

    findings: list[Finding] = []
    script = REPO / "scripts" / "butler_ffi_hole_probe.py"
    if not script.is_file():
        return findings
    report = RECEIPTS / "spectacular-ffi-oracle.json"
    try:
        r = subprocess.run(
            [
                sys.executable,
                str(script),
                "--base",
                base,
                "--json",
                str(report),
            ]
            + (["-v"] if verbose else []),
            capture_output=True,
            text=True,
            timeout=600,
            cwd=str(REPO),
        )
    except (OSError, subprocess.TimeoutExpired) as e:
        findings.append(
            Finding(
                "B",
                "soft",
                "ffi",
                "dual-stack",
                "gold",
                f"ffi oracle failed to run: {e}",
            )
        )
        return findings
    holes: list[dict] = []
    if report.is_file():
        try:
            body = json.loads(report.read_text(encoding="utf-8"))
            holes = body.get("holes") or body.get("fails") or body.get("findings") or []
        except (OSError, json.JSONDecodeError):
            holes = []
    if not holes and r.returncode not in (0, 1):
        findings.append(
            Finding(
                "B",
                "soft",
                "ffi",
                "dual-stack",
                "gold",
                f"ffi exit={r.returncode}: {(r.stderr or r.stdout or '')[:240]}",
            )
        )
        return findings
    for h in holes:
        if not isinstance(h, dict):
            continue
        sev = (h.get("severity") or "soft").lower()
        if sev not in ("spectacular", "soft"):
            sev = "soft"
        inv = h.get("inv") or h.get("id") or h.get("name") or "ffi"
        detail = h.get("detail") or h.get("message") or inv
        # Project path → short root label
        proj = h.get("project") or h.get("root") or ""
        root = Path(str(proj)).name if proj else "ffi"
        seed = h.get("seed") or inv
        findings.append(
            Finding(
                "B",
                sev,
                root,
                str(seed),
                "dual-stack",
                f"{inv}: {detail}",
                star=str(h.get("star") or ""),
                receipt=str(h.get("receipt") or ""),
                evidence={"inv": inv, "project": proj},
            )
        )
    if verbose:
        log(
            f"  ffi oracle: exit={r.returncode} findings={len(findings)} "
            f"report={report}"
        )
    return findings


def run_watcher_oracle(base: str, verbose: bool) -> list[Finding]:
    """Run butler_watcher_probe multi-file as class-W dual oracle (when fixture exists)."""
    import subprocess

    findings: list[Finding] = []
    wc = HOST_PROJECTS / "test_repos/pyo3/examples/word-count"
    if not (wc / "src" / "lib.rs").is_file():
        if verbose:
            log("  watcher oracle: skip (word-count fixture missing)")
        return findings
    script = REPO / "scripts" / "butler_watcher_probe.py"
    if not script.is_file():
        return findings
    report = RECEIPTS / "spectacular-watcher-oracle.json"
    try:
        r = subprocess.run(
            [
                sys.executable,
                str(script),
                "--base",
                base,
                "--mode",
                "multi",
                "--json",
                str(report),
            ],
            capture_output=True,
            text=True,
            timeout=180,
            cwd=str(REPO),
        )
    except (OSError, subprocess.TimeoutExpired) as e:
        findings.append(
            Finding(
                "W",
                "soft",
                "word-count",
                "watcher",
                "multi",
                f"watcher oracle failed to run: {e}",
            )
        )
        return findings
    # Parse report if present
    fails: list[dict] = []
    if report.is_file():
        try:
            body = json.loads(report.read_text(encoding="utf-8"))
            fails = body.get("fails") or body.get("failures") or []
            if not fails and isinstance(body.get("findings"), list):
                fails = body["findings"]
        except (OSError, json.JSONDecodeError):
            fails = []
    if not fails and r.returncode not in (0, 1):
        findings.append(
            Finding(
                "W",
                "soft",
                "word-count",
                "watcher",
                "multi",
                f"watcher exit={r.returncode}: {(r.stderr or r.stdout or '')[:200]}",
            )
        )
        return findings
    for f in fails:
        sev = (f.get("severity") or "soft").lower()
        if sev not in ("spectacular", "soft"):
            sev = "soft"
        inv = f.get("inv") or f.get("id") or f.get("cls") or "watcher"
        detail = f.get("detail") or f.get("message") or inv
        findings.append(
            Finding(
                "W",
                sev,
                "word-count",
                "watcher",
                "multi",
                f"{inv}: {detail}",
                evidence={"inv": inv},
            )
        )
    if verbose:
        log(
            f"  watcher oracle: exit={r.returncode} findings={len(findings)} "
            f"report={report}"
        )
    return findings


def warm_roots(base: str, roots: list[str]) -> None:
    try:
        http_json(f"{base}/warm", body={"roots": roots}, timeout=90)
    except Exception as e:
        log(f"  warm warn: {e}")


def wait_edges_complete(
    base: str,
    cont_root: str,
    *,
    timeout_s: float = 3600.0,
    poll_s: float = 5.0,
) -> bool:
    """Block until warehouse ready + edges_complete (thorough pre-zap)."""
    t0 = time.time()
    while time.time() - t0 < timeout_s:
        try:
            h = http_json(f"{base}/mcp/health", timeout=20)
        except Exception as e:
            log(f"  health wait: {e}")
            time.sleep(poll_s)
            continue
        loaded = h.get("loaded") or {}
        edge = h.get("edge_builds") or {}
        key = None
        for k in loaded:
            if (
                k == cont_root
                or k.rstrip("/") == cont_root.rstrip("/")
                or cont_root in k
                or k in cont_root
            ):
                key = k
                break
        if key:
            info = loaded.get(key) or {}
            e = edge.get(key) or {}
            ready = bool(info.get("ready"))
            complete = bool(info.get("edges_complete"))
            if ready and complete:
                log(
                    f"  edges_complete root={key} nodes={info.get('nodes')} "
                    f"({time.time() - t0:.0f}s)"
                )
                return True
            log(
                f"  … hydrate {key} ready={ready} edges={complete} "
                f"build={e.get('state')}/{e.get('percent')}"
            )
        else:
            log(f"  … not loaded yet (want {cont_root})")
        time.sleep(poll_s)
    log(f"  TIMEOUT waiting edges_complete for {cont_root}")
    return False


def run_spine_gold(
    base: str, available: dict[str, Path], verbose: bool
) -> tuple[list[Finding], list[dict]]:
    """Class S gold: reverse CALL spine must be CALL-honest."""
    findings: list[Finding] = []
    rows: list[dict] = []
    for case in SPINE_GOLD:
        root_key = case["root"]
        if root_key not in available:
            rows.append(
                {
                    "id": case["id"],
                    "ok": None,
                    "skipped": True,
                    "detail": "root missing",
                }
            )
            continue
        host = available[root_key]
        project = host_to_container(host)
        payload = {
            "project": project,
            "goal": "trace",
            "target_symbol": case["seed"],
            "scope_paths": case["scope"],
            "detail": "dense",
        }
        t0 = time.perf_counter()
        _raw, sc, text = wait_building(base, payload)
        ms = (time.perf_counter() - t0) * 1000
        pin = "/".join(case["scope"])
        ok, detail, sev = grade_spine_gold_checks(
            sc, case["checks"], seed=case["seed"], pin=pin
        )
        if not ok:
            findings.append(
                Finding(
                    "S",
                    sev,
                    root_key,
                    case["seed"],
                    pin,
                    f"{case['id']}: {detail}",
                    star=star_str(sc),
                    receipt="|".join(receipt_bits(sc, text).values()),
                )
            )
        # Structural S on the same pack
        for f in grade_spine_S(sc, seed=case["seed"], pin=pin):
            f.root = root_key
            findings.append(f)
        mark = "PASS" if ok else "FAIL"
        if verbose or not ok:
            log(f"  [{mark}] spine {case['id']}  {detail}  ({ms:.0f}ms)")
        rows.append(
            {
                "id": case["id"],
                "ok": ok,
                "detail": detail,
                "ms": round(ms, 1),
                "severity": sev if not ok else "",
                "star": star_str(sc),
                "spine": [
                    s.get("name") for s in (sc.get("caller_path") or [])
                ],
            }
        )
    return findings, rows


def run_fixed(
    base: str, available: dict[str, Path], verbose: bool
) -> tuple[list[Finding], list[dict]]:
    findings: list[Finding] = []
    rows: list[dict] = []
    for case in FIXED_CASES:
        root_key = case["root"]
        if root_key not in available:
            rows.append({"id": case["id"], "ok": None, "skipped": True, "detail": "root missing"})
            continue
        host = available[root_key]
        project = host_to_container(host)
        payload = {
            "project": project,
            "goal": "trace",
            "target_symbol": case["seed"],
            "scope_paths": case["scope"],
            "detail": "dense",
        }
        t0 = time.perf_counter()
        _raw, sc, text = wait_building(base, payload)
        ms = (time.perf_counter() - t0) * 1000
        ok, detail, sev = grade_fixed(sc, case["checks"], text)
        if not ok:
            findings.append(
                Finding(
                    case.get("class", "Q"),
                    sev,
                    root_key,
                    case["seed"],
                    "/".join(case["scope"]),
                    f"{case['id']}: {detail}",
                    star=star_str(sc),
                    receipt="|".join(receipt_bits(sc, text).values()),
                )
            )
        # Dual oracles always run (silent Q / L on otherwise-green packs)
        for f in go_import_oracle_false_call(host, sc, case["seed"]):
            f.root = root_key
            findings.append(f)
        for f in python_import_oracle_false_call(host, sc, case["seed"]):
            f.root = root_key
            findings.append(f)
        for f in callee_side_q_oracle(host, sc, case["seed"]):
            f.root = root_key
            findings.append(f)
        for f in grade_loc_fallback_L(
            sc, seed=case["seed"], pin="/".join(case["scope"])
        ):
            f.root = root_key
            findings.append(f)
        for f in grade_spine_S(sc, seed=case["seed"], pin="/".join(case["scope"])):
            f.root = root_key
            findings.append(f)
        mark = "PASS" if ok else "FAIL"
        if verbose or not ok:
            log(f"  [{mark}] fixed {case['id']}  {detail}  ({ms:.0f}ms)")
        rows.append(
            {
                "id": case["id"],
                "ok": ok,
                "detail": detail,
                "ms": round(ms, 1),
                "severity": sev if not ok else "",
                "star": star_str(sc),
            }
        )
    return findings, rows


def run_miner(
    base: str,
    available: dict[str, Path],
    seeds: list[str],
    *,
    max_seeds_per_root: int,
    max_pins: int,
    verbose: bool,
) -> tuple[list[Finding], list[dict]]:
    findings: list[Finding] = []
    rows: list[dict] = []

    for root_key, host in available.items():
        project = host_to_container(host)
        used = 0
        for seed in seeds:
            if used >= max_seeds_per_root:
                break
            if is_collision_mine_junk(seed):
                if verbose:
                    log(f"  skip junk seed {root_key}/{seed}")
                continue
            # Discover locations (unscoped)
            payload = {
                "project": project,
                "goal": "trace",
                "target_symbol": seed,
                "detail": "compact",
            }
            _raw, sc0, text0 = wait_building(base, payload, max_tries=25)
            domain = (sc0.get("blast_domain") or "").lower()
            locs = serious_locations(sc0)
            if domain == "disambiguate" and sc0.get("locations"):
                locs = serious_locations(sc0)
            # Need multi-def for collision mining
            files = []
            seen_f = set()
            for loc in locs:
                f = norm_path(loc.get("file") or "")
                if not f or f in seen_f:
                    continue
                seen_f.add(f)
                files.append(loc)
            if len(files) < 2:
                if verbose:
                    log(f"  skip {root_key}/{seed}: locs={len(files)}")
                continue
            used += 1
            pins = files[:max_pins]
            # First pass: collect hard callers per pin as (name, file) keys
            pin_hard: dict[str, set[tuple[str, str]]] = {}
            pin_sc: dict[str, dict] = {}
            for loc in pins:
                pin = pin_from_loc(loc)
                # Prefer full relative path suffix that works as scope
                full = norm_path(loc.get("file") or "")
                # Strip absolute host/container prefixes for scope_paths
                for prefix in (
                    str(HOST_PROJECTS) + "/",
                    CONT_PREFIX + "/",
                    "/home/user/projects/",
                    "/projects/",
                ):
                    if full.startswith(prefix):
                        full = full[len(prefix) :]
                        break
                # Prefer path relative to project root
                host_s = str(host).replace("\\", "/").rstrip("/") + "/"
                if full.startswith(host_s):
                    full = full[len(host_s) :]
                cont_s = host_to_container(host).rstrip("/") + "/"
                if full.startswith(cont_s):
                    full = full[len(cont_s) :]
                # Prefer longest relative path first; demote ambiguous shells (src/lib.rs).
                scope_try = []
                for scp in (full, pin, basenames(full)):
                    if scp and scp not in scope_try:
                        scope_try.append(scp)
                scope_try.sort(
                    key=lambda s: (
                        0 if pin_is_ambiguous_shell(s) else 1,
                        s.count("/"),
                        len(s),
                    ),
                    reverse=True,
                )
                sc_pin: dict = {}
                for scp in scope_try:
                    if not scp:
                        continue
                    payload_p = {
                        "project": project,
                        "goal": "trace",
                        "target_symbol": seed,
                        "scope_paths": [scp],
                        "detail": "dense",
                    }
                    _r, sc_pin, _t = wait_building(base, payload_p, max_tries=20)
                    if sc_pin.get("target"):
                        pin = scp
                        break
                if not sc_pin.get("target"):
                    findings.append(
                        Finding(
                            "H",
                            "soft",
                            root_key,
                            seed,
                            pin,
                            "pin Trace failed to resolve target",
                        )
                    )
                    continue
                pin_sc[pin] = sc_pin
                pin_hard[pin] = neighbor_keys(sc_pin.get("callers") or [])
                # grade single pin (name-only peer_maps unused for X now)
                for f in grade_mined_pin(
                    sc_pin,
                    seed=seed,
                    pin=pin,
                    other_pins=list(pin_hard.keys()),
                    peer_maps={},
                ):
                    f.root = root_key
                    findings.append(f)
                for f in go_import_oracle_false_call(host, sc_pin, seed):
                    f.root = root_key
                    findings.append(f)
                for f in python_import_oracle_false_call(host, sc_pin, seed):
                    f.root = root_key
                    findings.append(f)
                for f in callee_side_q_oracle(host, sc_pin, seed):
                    f.root = root_key
                    findings.append(f)

            # Second pass: same (name,file) hard on two pins — note only (often true fan-in)
            pins_list = list(pin_sc.keys())
            for i, pa in enumerate(pins_list):
                for pb in pins_list[i + 1 :]:
                    ha, hb = pin_hard.get(pa, set()), pin_hard.get(pb, set())
                    both = ha & hb
                    if both and basenames(pa) != basenames(pb) and verbose:
                        sample = sorted({n for n, _ in both})[:6]
                        log(
                            f"  note {root_key}/{seed}: shared hard callers {sample} "
                            f"across pins {pa} vs {pb}"
                        )
                    # P already checked per-pin via (name,file) ∩

            rows.append(
                {
                    "root": root_key,
                    "seed": seed,
                    "locs": len(files),
                    "pins": list(pin_sc.keys()),
                    "hard_callers": {
                        k: sorted([n for n, _ in v])[:12] for k, v in pin_hard.items()
                    },
                }
            )
            if verbose:
                log(
                    f"  mined {root_key}/{seed}: locs={len(files)} pins={list(pin_sc.keys())}"
                )

    return findings, rows


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--base", default=DEFAULT_BASE, help="Butler URL")
    ap.add_argument(
        "--roots",
        default="",
        help="Comma roots from default set or host paths (default: all existing defaults)",
    )
    ap.add_argument(
        "--seeds",
        default="",
        help="Comma collision seed names (default: built-in COLLISION_SEEDS)",
    )
    ap.add_argument("--max-pins", type=int, default=4)
    ap.add_argument("--max-seeds-per-root", type=int, default=6)
    ap.add_argument(
        "--thorough",
        action="store_true",
        help=(
            "Wait edges_complete, use large seed lexicon, "
            "higher multi-loc seed/pin caps (zap after full hydrate)"
        ),
    )
    ap.add_argument(
        "--tier2",
        action="store_true",
        help=(
            "Next tier: mine multi-file name collisions from live name_index "
            "via POST /collisions (warehouse must be loaded). Implies hydrate wait."
        ),
    )
    ap.add_argument(
        "--collision-max",
        type=int,
        default=200,
        help="Max collision names from /collisions when --tier2 (default 200)",
    )
    ap.add_argument(
        "--collision-min-files",
        type=int,
        default=2,
        help="Min distinct files for a collision name (default 2)",
    )
    ap.add_argument(
        "--ready-timeout",
        type=float,
        default=3600.0,
        help="Seconds to wait for edges_complete when --thorough (default 1h)",
    )
    ap.add_argument("--skip-fixed", action="store_true")
    ap.add_argument("--skip-miner", action="store_true")
    ap.add_argument(
        "--skip-spine",
        action="store_true",
        help="Skip class-S reverse spine gold cases",
    )
    ap.add_argument(
        "--skip-watcher",
        action="store_true",
        help="Skip class-W watcher multi-file oracle (word-count fixture)",
    )
    ap.add_argument(
        "--ffi",
        action="store_true",
        help="Run class-B dual-stack FFI oracle (butler_ffi_hole_probe gold)",
    )
    ap.add_argument(
        "--skip-ffi",
        action="store_true",
        help="Skip class-B FFI even when --ffi would be implied by hunt",
    )
    ap.add_argument("--strict-soft", action="store_true")
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument(
        "--json",
        default="",
        help="Report path (default plans/receipts/spectacular-latest.json)",
    )
    args = ap.parse_args()
    base = args.base.rstrip("/")
    RECEIPTS.mkdir(parents=True, exist_ok=True)

    if args.tier2:
        # Warehouse-driven collisions; hydrate first; denser pins.
        args.thorough = True
        if args.max_seeds_per_root == 6:
            args.max_seeds_per_root = 120
        if args.max_pins == 4:
            args.max_pins = 12
    if args.thorough and not args.tier2:
        # Full hydrate then dense multi-loc pin grid (fixed lexicon).
        if args.max_seeds_per_root == 6:
            args.max_seeds_per_root = 80
        if args.max_pins == 4:
            args.max_pins = 16
        if not args.seeds.strip():
            args.seeds = ",".join(THOROUGH_SEEDS)

    mode = "TIER2" if args.tier2 else ("THOROUGH" if args.thorough else "STANDARD")
    log("=" * 64)
    log(f"BUTLER SPECTACULAR PROBE [{mode}]")
    log("=" * 64)
    log("Silent-lie mill: multi-def pins + dual oracle (no LLM)")
    log(f"base={base}")
    if args.thorough or args.tier2:
        log(
            f"mode={mode}: max_seeds_per_root={args.max_seeds_per_root} "
            f"max_pins={args.max_pins}"
        )

    # Health
    try:
        h = http_json(f"{base}/mcp/health", timeout=15)
    except Exception as e:
        log(f"FATAL: health {e}")
        return 2
    if h.get("status") != "ok":
        log(f"FATAL: status={h.get('status')}")
        return 2
    log(f"health ok fp={h.get('fingerprint')}")

    # Resolve roots. Oracle-only (--ffi/--skip-miner/--skip-fixed) needs no collision roots.
    available: dict[str, Path] = {}
    oracle_only = args.skip_miner and args.skip_fixed and (args.ffi or not args.skip_watcher)
    if args.roots.strip():
        for tok in args.roots.split(","):
            tok = tok.strip()
            if not tok:
                continue
            if tok in DEFAULT_ROOTS:
                p = DEFAULT_ROOTS[tok]
                if p.is_dir():
                    available[tok] = p
                else:
                    log(f"  skip missing {tok}={p}")
            else:
                p = Path(tok)
                if p.is_dir():
                    available[p.name] = p
                else:
                    log(f"  skip missing path {tok}")
    elif not oracle_only:
        for k, p in DEFAULT_ROOTS.items():
            if p.is_dir():
                available[k] = p
            elif args.verbose:
                log(f"  skip missing {k}")

    # Need collision roots, or at least one oracle lane (ffi / watcher).
    if not available and not args.ffi and args.skip_watcher:
        log("FATAL: no roots available (pass --roots, --ffi, or enable watcher)")
        return 2
    if available:
        log(f"roots: {', '.join(sorted(available))}")
    else:
        log("roots: (none — oracle lanes only)")

    seeds = (
        [s.strip() for s in args.seeds.split(",") if s.strip()]
        if args.seeds.strip()
        else list(COLLISION_SEEDS)
    )

    cont_roots = [host_to_container(p) for p in available.values()]
    if cont_roots:
        warm_roots(base, cont_roots)
        time.sleep(0.5)

    if (args.thorough or args.tier2) and cont_roots:
        log("\n── Wait edges_complete (full hydrate before zap) ──")
        for cr in cont_roots:
            if not wait_edges_complete(
                base, cr, timeout_s=args.ready_timeout
            ):
                log(f"FATAL: {cr} not edges_complete")
                return 2

    # Tier-2: replace / extend seed list from live name_index collisions
    collision_meta: list[dict] = []
    if args.tier2:
        log("\n── Tier-2: mine name_index collisions (POST /collisions) ──")
        tier2_seeds: list[str] = []
        for host in available.values():
            proj = host_to_container(host)
            try:
                body = http_json(
                    f"{base}/collisions",
                    body={
                        "project": proj,
                        "min_files": args.collision_min_files,
                        "max": args.collision_max,
                        "min_name_len": 2,
                    },
                    timeout=120.0,
                )
            except Exception as e:
                log(f"  FATAL: /collisions {proj}: {e}")
                log("  (rebuild butler with handle_collisions; warehouse must be loaded)")
                return 2
            cols = body.get("collisions") or []
            log(
                f"  {proj}: name_index_keys={body.get('name_index_keys')} "
                f"collisions={len(cols)} nodes={body.get('nodes')}"
            )
            junk_n = 0
            for c in cols:
                n = (c.get("name") or "").strip()
                if not n:
                    continue
                if is_collision_mine_junk(n):
                    junk_n += 1
                    continue
                if n not in tier2_seeds:
                    tier2_seeds.append(n)
                collision_meta.append(
                    {
                        "root": Path(host).name,
                        "name": n,
                        "files": c.get("files"),
                        "locations": c.get("locations"),
                    }
                )
            if junk_n:
                log(f"  filtered {junk_n} junk collision seed(s)")
            if args.verbose:
                for c in cols[:20]:
                    log(
                        f"    {c.get('name')}: files={c.get('files')} "
                        f"locs={c.get('locations')}"
                    )
                if len(cols) > 20:
                    log(f"    … +{len(cols) - 20} more")
        if not tier2_seeds:
            log("  FATAL: /collisions returned no multi-file names")
            return 2
        # Prefer warehouse collisions; keep a few fixed-class seeds first for coverage
        seeds = list(dict.fromkeys(tier2_seeds + seeds))
        # Cap miner budget to collision list size (all of them, up to max_seeds)
        args.max_seeds_per_root = max(
            args.max_seeds_per_root, min(len(tier2_seeds), args.collision_max)
        )
        log(
            f"  tier2 seed list: {len(seeds)} (unique collisions + baseline); "
            f"max_seeds_per_root={args.max_seeds_per_root}"
        )

    t0 = time.perf_counter()
    all_findings: list[Finding] = []
    fixed_rows: list[dict] = []
    spine_rows: list[dict] = []
    mine_rows: list[dict] = []

    if not args.skip_fixed and available:
        log("\n── Fixed regressions (must stay green) ──")
        f_find, fixed_rows = run_fixed(base, available, args.verbose)
        all_findings.extend(f_find)

    if not args.skip_spine and available:
        log("\n── Spine gold (class S reverse CALL path) ──")
        s_find, spine_rows = run_spine_gold(base, available, args.verbose)
        all_findings.extend(s_find)

    if not args.skip_miner and available:
        log("\n── Collision miner ──")
        m_find, mine_rows = run_miner(
            base,
            available,
            seeds,
            max_seeds_per_root=args.max_seeds_per_root,
            max_pins=args.max_pins,
            verbose=args.verbose,
        )
        all_findings.extend(m_find)

    if args.ffi and not args.skip_ffi:
        log("\n── FFI dual-stack oracle (class B) ──")
        all_findings.extend(run_ffi_oracle(base, args.verbose))

    if not args.skip_watcher:
        log("\n── Watcher oracle (class W, multi-file) ──")
        all_findings.extend(run_watcher_oracle(base, args.verbose))

    wall = time.perf_counter() - t0
    spectacular = [f for f in all_findings if f.severity == "spectacular"]
    soft = [f for f in all_findings if f.severity == "soft"]

    # Dedupe findings by detail+root+seed
    def dedupe(fs: list[Finding]) -> list[Finding]:
        seen = set()
        out = []
        for f in fs:
            k = (f.cls, f.root, f.seed, f.pin, f.detail)
            if k in seen:
                continue
            seen.add(k)
            out.append(f)
        return out

    spectacular = dedupe(spectacular)
    soft = dedupe(soft)

    ok = len(spectacular) == 0 and (not args.strict_soft or len(soft) == 0)
    exit_code = 0 if ok else 1

    utc = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    report = {
        "gate": "spectacular_probe",
        "utc": utc,
        "butler": base,
        "fingerprint": h.get("fingerprint"),
        "seconds": round(wall, 2),
        "mode": "tier2" if args.tier2 else ("thorough" if args.thorough else "standard"),
        "summary": {
            "ok": ok,
            "spectacular": len(spectacular),
            "soft": len(soft),
            "exit_code": exit_code,
            "roots": sorted(available.keys()),
            "fixed_pass": sum(1 for r in fixed_rows if r.get("ok") is True),
            "fixed_fail": sum(1 for r in fixed_rows if r.get("ok") is False),
            "spine_pass": sum(1 for r in spine_rows if r.get("ok") is True),
            "spine_fail": sum(1 for r in spine_rows if r.get("ok") is False),
            "mined_seeds": len(mine_rows),
            "tier2": bool(args.tier2),
            "collision_candidates": len(collision_meta),
        },
        "taxonomy": {
            "Q": "qualifier/package collapse (wrong same-name CALL target)",
            "P": "peer_as_call (name_peer mixed into callers)",
            "H": "wrong_star (pin ≠ preferred file)",
            "X": "cross_pin_leak (peer/hard mixed across same-name defs)",
            "L": "loc_fallback invent (hop-1 callers with warehouse reverse 0)",
            "B": "bridge/dual-stack FFI lie (Export/IPC missing or junk host)",
            "S": "reverse spine lie (caller_path not CALL-honest)",
            "W": "watcher lie (incremental re-edge falsehood)",
        },
        "fixed": fixed_rows,
        "spine": spine_rows,
        "collisions": collision_meta[:500],
        "mined": mine_rows,
        "findings": [asdict(f) for f in spectacular + soft],
    }

    out = Path(args.json) if args.json else RECEIPTS / "spectacular-latest.json"
    out.write_text(json.dumps(report, indent=2), encoding="utf-8")
    stamped = RECEIPTS / f"spectacular-{utc}.json"
    if not args.json:
        stamped.write_text(json.dumps(report, indent=2), encoding="utf-8")

    log("\n" + "=" * 64)
    log("SPECTACULAR REPORT")
    log("=" * 64)
    log(
        f"fixed pass={report['summary']['fixed_pass']} fail={report['summary']['fixed_fail']}  "
        f"spine pass={report['summary']['spine_pass']} fail={report['summary']['spine_fail']}  "
        f"mined_seeds={report['summary']['mined_seeds']}"
    )
    log(f"spectacular={len(spectacular)} soft={len(soft)} ok={ok} wall={wall:.1f}s")
    for f in spectacular[:20]:
        log(f"  SPEC [{f.cls}] {f.root}/{f.seed} pin={f.pin}: {f.detail}")
    if len(spectacular) > 20:
        log(f"  … +{len(spectacular) - 20} more spectacular")
    for f in soft[:10]:
        log(f"  soft [{f.cls}] {f.root}/{f.seed}: {f.detail}")
    log(f"\nWrote {out}")
    if not args.json:
        log(f"Wrote {stamped}")
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
