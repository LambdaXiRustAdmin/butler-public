#!/usr/bin/env python3
"""
Structural desirability pack (bakeoff T1–T6) against live Butler.

Contract (no barn-smoke waits):
  - Every wait loop prints progress at least every tick (default 3s).
  - Hard deadline per root / per task — never silent multi-minute hang.
  - stdout line-buffered (`python3 -u` recommended).

Usage:
  python3 -u scripts/desirability_pack.py
  python3 -u scripts/desirability_pack.py --base http://127.0.0.1:8002
  python3 -u scripts/desirability_pack.py --only T1,T3

LiteLLM (for future A1/Qwen arm — never guess silently):
  Master key lives in llm-stack docker-compose:
    LITELLM_MASTER_KEY=sk-dummy-key-not-real
  Prefer model alias qwen3-35b (llama-server-35B). Export:
    export LITELLM_MASTER_KEY=sk-dummy-key-not-real
  If key missing when a Qwen arm is requested, **exit non-zero with a path** — no silent skip.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


def log(msg: str) -> None:
    print(msg, flush=True)


def get_json(base: str, path: str, timeout: float = 30.0) -> dict[str, Any]:
    with urllib.request.urlopen(base + path, timeout=timeout) as r:
        return json.load(r)


def post_json(base: str, path: str, body: dict[str, Any], timeout: float = 180.0) -> dict[str, Any]:
    data = json.dumps(body).encode()
    req = urllib.request.Request(
        base + path, data=data, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.load(r)


def warm(base: str, root: str) -> None:
    log(f"  warm {root}")
    try:
        j = post_json(base, "/warm", {"roots": [root]}, timeout=30)
        log(f"    -> {j}")
    except Exception as e:
        log(f"    warm ERR: {e}")


def wait_loaded(
    base: str,
    root: str,
    *,
    deadline_s: float,
    tick_s: float,
) -> tuple[bool, dict[str, Any] | None]:
    """Poll health until root ready. **Always prints** each tick — no silent wait."""
    t0 = time.time()
    tick = 0
    while True:
        elapsed = time.time() - t0
        if elapsed > deadline_s:
            log(f"    DEADLINE {deadline_s:.0f}s exceeded for {root} (still not ready)")
            return False, None
        try:
            h = get_json(base, "/mcp/health", timeout=15)
        except Exception as e:
            log(f"    health ERR t={elapsed:.0f}s: {e}")
            time.sleep(tick_s)
            tick += 1
            continue
        loaded = h.get("loaded") or {}
        hit = None
        for k, v in loaded.items():
            if root.rstrip("/") in k or k.rstrip("/") in root:
                hit = (k, v)
                break
        if hit:
            k, v = hit
            n = v.get("nodes") or 0
            ready = bool(v.get("ready"))
            edges = v.get("edges_complete")
            log(
                f"    tick={tick} t={elapsed:.0f}s key={k} nodes={n} ready={ready} edges={edges}"
            )
            if ready and n > 0:
                return True, v
        else:
            keys = list(loaded.keys())
            log(f"    tick={tick} t={elapsed:.0f}s not in loaded keys={keys}")
        time.sleep(tick_s)
        tick += 1


def is_building(j: dict[str, Any]) -> bool:
    content = j.get("content") or ""
    sc = j.get("structuredContent") or j.get("structured") or {}
    if not isinstance(sc, dict):
        sc = {}
    tel = sc.get("telemetry") or {}
    if not isinstance(tel, dict):
        tel = {}
    err = sc.get("error") or ""
    status = str(tel.get("status") or sc.get("status") or "")
    blob = content + err + status
    return (
        "BUILDING" in blob
        or "Building Graph" in blob
        or status in ("BUILDING", "BUILDING_SOFT_WALL", "building", "building_soft_wall")
    )


def orch(
    base: str,
    project: str,
    goal: str,
    symbol: str | None,
    scope: list[str] | None,
    detail: str,
    *,
    max_tries: int,
    tick_s: float,
) -> tuple[float, dict[str, Any], dict[str, Any]]:
    body: dict[str, Any] = {
        "mcp_tool_name": "butler_orchestrate",
        "goal": goal,
        "project": project,
        "detail": detail,
    }
    if symbol:
        body["target_symbol"] = symbol
    if scope:
        body["scope_paths"] = scope
    t0 = time.time()
    last: dict[str, Any] = {}
    for attempt in range(1, max_tries + 1):
        try:
            j = post_json(base, "/context", body, timeout=180)
        except Exception as e:
            log(f"    attempt {attempt}/{max_tries} HTTP ERR: {e}")
            time.sleep(tick_s)
            continue
        last = j
        sc = j.get("structuredContent") or j.get("structured") or {}
        if not isinstance(sc, dict):
            sc = {}
        content_head = (j.get("content") or "")[:100].replace("\n", " ")
        if is_building(j):
            next_a = sc.get("next_action") or (sc.get("wait_policy") or {}).get("next_action")
            pct = (sc.get("state") or {}).get("percent") or (sc.get("telemetry") or {}).get(
                "progress"
            )
            log(
                f"    attempt {attempt}/{max_tries}: BUILDING pct={pct} next={next_a!r} | {content_head}"
            )
            time.sleep(tick_s)
            continue
        wall = round(time.time() - t0, 2)
        log(f"    attempt {attempt}: DONE wall={wall}s | {content_head}")
        return wall, j, sc
    wall = round(time.time() - t0, 2)
    sc = last.get("structuredContent") or last.get("structured") or {}
    if not isinstance(sc, dict):
        sc = {}
    log(f"    EXHAUSTED tries wall={wall}s still building/error")
    return wall, last, sc


def summarize(task: str, wall: float, sc: dict[str, Any], content: str) -> dict[str, Any]:
    t = sc.get("target") or {}
    return {
        "task": task,
        "wall_s": wall,
        "seed": t.get("name"),
        "file": (t.get("file") or "")[-80:],
        "callers": len(sc.get("callers") or []),
        "callees": len(sc.get("callees") or []),
        "br_in": len(sc.get("bridge_callers") or []),
        "br_out": len(sc.get("bridge_callees") or []),
        "bridges": [
            (b.get("name"), b.get("relation"), b.get("why"))
            for b in list(sc.get("bridge_callers") or [])[:3]
            + list(sc.get("bridge_callees") or [])[:3]
        ],
        "blast": sc.get("blast_domain"),
        "next": sc.get("next_action")
        or (sc.get("wait_policy") or {}).get("next_action")
        if isinstance(sc.get("wait_policy"), dict)
        else sc.get("next_action"),
        "receipt": sc.get("receipt"),
        "error": (sc.get("error") or "")[:160],
        "content": (content or "")[:180].replace("\n", " "),
        "locs": len(sc.get("locations") or []),
        "suggested": (sc.get("suggested_scopes") or [])[:6],
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--base", default="http://127.0.0.1:8002")
    ap.add_argument("--only", default="", help="Comma task ids e.g. T1,T3")
    ap.add_argument("--warm-deadline", type=float, default=90.0)
    ap.add_argument("--tick", type=float, default=3.0)
    ap.add_argument("--build-tries", type=int, default=20)
    ap.add_argument(
        "--out",
        type=Path,
        default=Path("/tmp/desirability_a1_pack.json"),
    )
    args = ap.parse_args()
    only = {s.strip() for s in args.only.split(",") if s.strip()} or None

    tasks = [
        ("T1", "/projects/test_repos/fd", "TraceBlastRadius", "construct_config", ["src/"], "dense"),
        ("T2", "/projects/test_repos/gin", "TraceBlastRadius", "Default", None, "dense"),
        (
            "T3",
            "/projects/test_repos/pyo3/examples/word-count",
            "TraceBlastRadius",
            "search_py",
            None,
            "dense",
        ),
        (
            "T4",
            "/projects/test_repos/pybind11",
            "TraceBlastRadius",
            "test_function_overloading",
            ["tests/"],
            "dense",
        ),
        (
            "T5",
            "/projects/test_repos/tauri/examples/api",
            "TraceBlastRadius",
            "log_operation",
            ["src-tauri/", "src/"],
            "dense",
        ),
    ]
    if only:
        tasks = [t for t in tasks if t[0] in only]

    try:
        fp = get_json(args.base, "/mcp/health").get("fingerprint")
    except Exception as e:
        log(f"FATAL: cannot reach {args.base}: {e}")
        return 2
    log(f"fingerprint={fp} base={args.base}")
    log("RULE: every wait tick prints; no silent multi-minute loops")

    log("\n== PRE-WARM ==")
    for tid, root, *_ in tasks:
        warm(args.base, root)

    log("\n== WAIT LOADED ==")
    for tid, root, *_ in tasks:
        log(f"[{tid}] wait {root} (deadline {args.warm_deadline:.0f}s)")
        ok, v = wait_loaded(
            args.base, root, deadline_s=args.warm_deadline, tick_s=args.tick
        )
        log(f"  -> ready={ok} nodes={(v or {}).get('nodes')}")

    results: list[dict[str, Any]] = []
    log("\n== RUN TASKS ==")
    for tid, root, goal, sym, scope, detail in tasks:
        log(f"\n### {tid} {sym} @ {root}")
        wall, j, sc = orch(
            args.base,
            root,
            goal,
            sym,
            scope,
            detail,
            max_tries=args.build_tries,
            tick_s=args.tick,
        )
        content = (j or {}).get("content") or ""
        row = summarize(tid, wall, sc or {}, content)

        # T2: if disambiguate, re-pin with first *relative* suggested scope
        if tid == "T2" and (
            row.get("blast") == "disambiguate" or (row.get("locs") or 0) >= 3
        ):
            pins = [
                p
                for p in (row.get("suggested") or [])
                if p and not p.startswith("/") and "home/" not in p
            ]
            pin = next(
                (p for p in pins if "gin.go" in p or p.endswith(".go")),
                pins[0] if pins else "gin.go",
            )
            log(f"  T2 re-pin {pin!r} (relative only)")
            wall2, j2, sc2 = orch(
                args.base,
                root,
                goal,
                sym,
                [pin],
                detail,
                max_tries=args.build_tries,
                tick_s=args.tick,
            )
            row = summarize("T2", wall + wall2, sc2 or {}, (j2 or {}).get("content") or "")
            row["pinned"] = pin

        # Score
        if tid == "T1":
            row["correct"] = 1 if row["callers"] > 0 and row["seed"] else 0
            row["bridge_ok"] = "NA"
        elif tid == "T2":
            if row.get("blast") == "disambiguate":
                row["correct"] = 1 if row.get("locs") or row.get("next") else 0
            else:
                row["correct"] = 1 if row.get("seed") else 0
            row["bridge_ok"] = "NA"
        elif tid in ("T3", "T4", "T5"):
            want = "export" if tid != "T5" else "ipc"
            ok = any((b[1] or "").lower() == want for b in row.get("bridges") or [])
            ok = ok or want in (row.get("content") or "").lower()
            row["bridge_ok"] = 1 if ok else 0
            row["correct"] = 1 if ok else 0
        results.append(row)
        log(json.dumps(row, indent=2)[:1000])

    if not only or "T6" in only:
        log("\n### T6 Arch→Trace fd")
        t0 = time.time()
        _, j1, _ = orch(
            args.base,
            "/projects/test_repos/fd",
            "ArchitecturalSummary",
            None,
            ["src/"],
            "compact",
            max_tries=args.build_tries,
            tick_s=args.tick,
        )
        _, j2, sc2 = orch(
            args.base,
            "/projects/test_repos/fd",
            "TraceBlastRadius",
            "construct_config",
            ["src/"],
            "compact",
            max_tries=args.build_tries,
            tick_s=args.tick,
        )
        row = {
            "task": "T6",
            "wall_s": round(time.time() - t0, 2),
            "tool_calls": 2,
            "arch": ((j1 or {}).get("content") or "")[:100],
            "trace_seed": ((sc2 or {}).get("target") or {}).get("name"),
            "correct": 1 if (j1 and sc2 and sc2.get("target")) else 0,
            "bridge_ok": "NA",
        }
        results.append(row)
        log(json.dumps(row, indent=2))

    log("\n======== A1 STRUCTURAL GATE ========")
    for r in results:
        log(
            f"  {r['task']}: correct={r.get('correct')} bridge={r.get('bridge_ok')} "
            f"wall={r.get('wall_s')}s seed={r.get('seed') or r.get('trace_seed')}"
        )
    n = len(results)
    c = sum(int(r.get("correct") or 0) for r in results)
    log(f"reliability={c}/{n} = {c / max(n, 1):.2f}")
    args.out.write_text(json.dumps({"fingerprint": fp, "results": results}, indent=2))
    log(f"wrote {args.out}")
    return 0 if c == n else 1


if __name__ == "__main__":
    sys.exit(main())
