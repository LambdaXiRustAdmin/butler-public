#!/usr/bin/env python3
"""Butler structural integrity gate + optional hydrate reopen probe.

Not GNN. Proves dual-stack Export/IPC + same-lang seeds still tell the truth
on a live tip server (default http://127.0.0.1:8002).

Usage:
  python3 scripts/butler_integrity_gate.py
  python3 scripts/butler_integrity_gate.py --base http://127.0.0.1:8002
  python3 scripts/butler_integrity_gate.py --hydrate-probe /projects/test_repos/pybind11
  BUTLER_URL=http://127.0.0.1:8002 python3 scripts/butler_integrity_gate.py -v

Exit 0 = all integrity cases pass (hydrate probe is advisory unless --hydrate-fail).
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
from dataclasses import dataclass, field
from typing import Any, Callable, Optional


DEFAULT_BASE = os.environ.get("BUTLER_URL", "http://127.0.0.1:8002").rstrip("/")


@dataclass
class Case:
    id: str
    title: str
    payload: dict[str, Any]
    checks: list[Callable[[str, dict[str, Any]], Optional[str]]]
    # If BUILDING/hydrate, retry this many times with sleep
    max_retries: int = 8
    retry_sleep_s: float = 2.0


@dataclass
class CaseResult:
    id: str
    title: str
    ok: bool
    ms: int
    attempts: int
    detail: str
    mode: str = ""


def post_context(base: str, payload: dict[str, Any], timeout: float = 120.0) -> tuple[dict[str, Any], int, Optional[str]]:
    url = f"{base}/context"
    data = json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            body = resp.read().decode()
            ms = int((time.perf_counter() - t0) * 1000)
            try:
                return json.loads(body), ms, None
            except json.JSONDecodeError:
                return {"content": body}, ms, None
    except Exception as e:
        ms = int((time.perf_counter() - t0) * 1000)
        return {}, ms, str(e)


def is_building(content: str, mode: str) -> bool:
    if mode in ("building", "building_soft_wall", "discovery"):
        return mode.startswith("building")
    c = content or ""
    return (
        "status: BUILDING" in c
        or "hydrating cache" in c
        or "=== Building Graph" in c
        or "BUILDING_SOFT_WALL" in c
    )


def orch(
    project: str,
    goal: str,
    symbol: Optional[str] = None,
    scope: Optional[list[str]] = None,
    detail: str = "dense",
) -> dict[str, Any]:
    p: dict[str, Any] = {
        "mcp_tool_name": "butler_orchestrate",
        "project": project,
        "goal": goal,
        "detail": detail,
    }
    if symbol:
        p["target_symbol"] = symbol
    if scope:
        p["scope_paths"] = scope
    return p


def check_re(pattern: str, flags: int = re.I) -> Callable[[str, dict], Optional[str]]:
    rx = re.compile(pattern, flags)

    def _c(content: str, _data: dict) -> Optional[str]:
        if rx.search(content or ""):
            return None
        return f"missing /{pattern}/"

    return _c


def check_not_re(pattern: str, flags: int = re.I) -> Callable[[str, dict], Optional[str]]:
    rx = re.compile(pattern, flags)

    def _c(content: str, _data: dict) -> Optional[str]:
        if rx.search(content or ""):
            return f"forbidden /{pattern}/"
        return None

    return _c


def check_no_discovery(content: str, _data: dict) -> Optional[str]:
    if "No specific project provided" in (content or ""):
        return "discovery mode (bad project path)"
    if "Project Discovery (shallow" in (content or ""):
        return "shallow discovery listing"
    return None


def check_not_orchestrate_error(content: str, _data: dict) -> Optional[str]:
    if "Orchestrate error:" in (content or ""):
        # allow partial banners; hard miss is fail
        if "not found" in (content or "").lower() or "Unrecognized goal" in (content or ""):
            return (content or "")[:160].replace("\n", " ")
    return None


def run_case(base: str, case: Case, verbose: bool) -> CaseResult:
    last_err = ""
    total_ms = 0
    attempts = 0
    content = ""
    mode = ""
    data: dict[str, Any] = {}

    for attempts in range(1, case.max_retries + 1):
        data, ms, err = post_context(base, case.payload)
        total_ms += ms
        if err:
            last_err = err
            if verbose:
                print(f"  [{case.id}] attempt {attempts} network: {err}", file=sys.stderr)
            time.sleep(case.retry_sleep_s)
            continue
        content = data.get("content") or ""
        mode = str(data.get("mode") or "")
        if is_building(content, mode):
            if verbose:
                phase = ""
                for line in content.splitlines()[:6]:
                    if "phase" in line.lower() or "Building" in line or "%" in line:
                        phase = line.strip()
                        break
                print(
                    f"  [{case.id}] attempt {attempts} BUILDING {ms}ms {phase}",
                    file=sys.stderr,
                )
            time.sleep(case.retry_sleep_s)
            continue
        # settled
        fails = []
        for chk in case.checks:
            msg = chk(content, data)
            if msg:
                fails.append(msg)
        if fails:
            return CaseResult(
                case.id,
                case.title,
                False,
                total_ms,
                attempts,
                "; ".join(fails),
                mode,
            )
        return CaseResult(case.id, case.title, True, total_ms, attempts, "ok", mode)

    if last_err:
        return CaseResult(case.id, case.title, False, total_ms, attempts, last_err, mode)
    return CaseResult(
        case.id,
        case.title,
        False,
        total_ms,
        attempts,
        "still BUILDING after retries",
        mode,
    )


def integrity_cases() -> list[Case]:
    WC = "/projects/test_repos/pyo3/examples/word-count"
    PB = "/projects/test_repos/pybind11"
    TA = "/projects/test_repos/tauri/examples/api"
    FD = "/projects/test_repos/fd"
    GN = "/projects/test_repos/gin"
    CL = "/projects/test_repos/click"
    RI = "/projects/test_repos/rich"

    return [
        Case(
            "T3_export_py",
            "word-count search_py → export → search",
            orch(WC, "TraceBlastRadius", "search_py"),
            [
                check_no_discovery,
                check_not_orchestrate_error,
                check_re(r"search_py"),
                check_re(r"\bexport\b"),
                check_re(r"\bsearch\b"),
                check_not_re(r"No specific project provided"),
            ],
        ),
        Case(
            "T3_export_rs",
            "word-count search ← export ← search_py",
            orch(WC, "TraceBlastRadius", "search"),
            [
                check_no_discovery,
                check_re(r"\bexport\b"),
                check_re(r"search_py"),
            ],
        ),
        Case(
            "T4_pybind",
            "pybind test_function_overloading → export → test_function1",
            orch(PB, "TraceBlastRadius", "test_function_overloading", ["tests/"]),
            [
                check_no_discovery,
                check_re(r"test_function_overloading"),
                check_re(r"\bexport\b"),
                check_re(r"test_function1"),
            ],
            max_retries=12,
            retry_sleep_s=3.0,
        ),
        Case(
            "T5_ipc",
            "tauri log_operation ← ipc ← Communication",
            orch(TA, "TraceBlastRadius", "log_operation", ["src-tauri/", "src/"]),
            [
                check_no_discovery,
                check_re(r"log_operation"),
                check_re(r"\bipc\b"),
                check_re(r"Communication"),
            ],
        ),
        Case(
            "T1_fd",
            "fd construct_config has CALL caller",
            orch(FD, "TraceBlastRadius", "construct_config"),
            [
                check_no_discovery,
                check_re(r"construct_config"),
                # direct caller run (or at least some CALL callers section non-empty)
                check_re(r"(CALL callers|direct.*caller|★)"),
                check_not_re(r"Symbol 'construct_config' not found"),
            ],
            max_retries=10,
        ),
        Case(
            "T2_gin",
            "gin Default prefers gin.go production",
            orch(GN, "TraceBlastRadius", "Default"),
            [
                check_no_discovery,
                check_re(r"Default"),
                check_re(r"gin\.go"),
                check_not_re(r"Symbol 'Default' not found"),
            ],
            max_retries=10,
        ),
        Case(
            "NEG_short_name",
            "short project name must not silent-succeed as Trace",
            {
                "mcp_tool_name": "butler_orchestrate",
                "goal": "TraceBlastRadius",
                "project": "word-count",
                "target_symbol": "search_py",
                "detail": "compact",
            },
            [
                # Must be discovery or error — not a real Trace with export
                lambda c, d: (
                    None
                    if (
                        "No specific project" in (c or "")
                        or "Project Discovery" in (c or "")
                        or "Discovery Mode" in (c or "")
                        or d.get("mode") == "discovery"
                    )
                    else "expected discovery for short project name"
                ),
            ],
            max_retries=2,
            retry_sleep_s=0.5,
        ),
        # A′.9 / A′.10 same-lang Python (re-smoke — was island / bench-dominated)
        Case(
            "A9_click_group",
            "click Group has CALL neighborhood (not 0/0 island)",
            orch(CL, "TraceBlastRadius", "Group", ["src/"]),
            [
                check_no_discovery,
                check_re(r"\bGroup\b"),
                check_re(r"core\.py"),
                check_re(r"CALL callers"),
                check_not_re(r"CALL callers \(0\)"),
                check_not_re(r"Symbol 'Group' not found"),
            ],
            max_retries=14,
            retry_sleep_s=3.0,
        ),
        Case(
            "A10_rich_console",
            "rich Console ★ on console.py (not benchmarks-only seed)",
            orch(RI, "TraceBlastRadius", "Console"),
            [
                check_no_discovery,
                check_re(r"\bConsole\b"),
                check_re(r"console\.py"),
                check_not_re(r"★.*benchmarks"),
                check_not_re(r"Symbol 'Console' not found"),
            ],
            max_retries=14,
            retry_sleep_s=3.0,
        ),
    ]


def hydrate_probe(
    base: str,
    project: str,
    symbol: str = "main",
    timeout_s: float = 180.0,
    verbose: bool = False,
) -> tuple[bool, str]:
    """Time until first non-BUILDING Trace after a request (reopen / cold path)."""
    payload = orch(project, "TraceBlastRadius", symbol, detail="compact")
    t0 = time.perf_counter()
    attempts = 0
    phases: list[str] = []
    while time.perf_counter() - t0 < timeout_s:
        attempts += 1
        data, ms, err = post_context(base, payload, timeout=min(60.0, timeout_s))
        if err:
            phases.append(f"err:{err}")
            time.sleep(1.5)
            continue
        content = data.get("content") or ""
        mode = str(data.get("mode") or "")
        if is_building(content, mode):
            line = "BUILDING"
            for ln in content.splitlines()[:8]:
                if "phase" in ln.lower() or "hydrat" in ln.lower() or "Building" in ln:
                    line = ln.strip()[:100]
                    break
            phases.append(f"{ms}ms {line}")
            if verbose:
                print(f"  hydrate attempt {attempts}: {line}", file=sys.stderr)
            time.sleep(1.5)
            continue
        elapsed = time.perf_counter() - t0
        summary = (
            f"READY in {elapsed:.2f}s attempts={attempts} last_ms={ms} mode={mode} "
            f"path={project} symbol={symbol}"
        )
        if phases:
            summary += f" | path=[{'; '.join(phases[:6])}"
            if len(phases) > 6:
                summary += f" … +{len(phases)-6}"
            summary += "]"
        return True, summary
    return False, f"TIMEOUT {timeout_s}s project={project} attempts={attempts} phases={phases[-5:]}"


def health(base: str) -> tuple[bool, str]:
    url = f"{base}/mcp/health"
    try:
        with urllib.request.urlopen(url, timeout=10) as r:
            d = json.loads(r.read().decode())
            fp = d.get("fingerprint") or d.get("version") or "?"
            return True, f"ok fingerprint={fp}"
    except Exception as e:
        return False, str(e)


def main() -> int:
    ap = argparse.ArgumentParser(description="Butler integrity gate")
    ap.add_argument("--base", default=DEFAULT_BASE, help="Butler base URL")
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument(
        "--hydrate-probe",
        metavar="PROJECT",
        help="After integrity, time reopen until non-BUILDING for PROJECT path",
    )
    ap.add_argument(
        "--hydrate-symbol",
        default="main",
        help="Symbol for hydrate probe Trace (default main)",
    )
    ap.add_argument(
        "--hydrate-fail",
        action="store_true",
        help="Exit non-zero if hydrate probe fails/timeouts",
    )
    ap.add_argument(
        "--only",
        metavar="ID",
        help="Run a single case id (e.g. T3_export_py)",
    )
    args = ap.parse_args()
    base = args.base.rstrip("/")

    print(f"Butler integrity gate → {base}")
    ok_h, hmsg = health(base)
    print(f"health: {hmsg}")
    if not ok_h:
        print("FAIL: server not healthy")
        return 2

    cases = integrity_cases()
    if args.only:
        cases = [c for c in cases if c.id == args.only]
        if not cases:
            print(f"unknown case id {args.only!r}")
            return 2

    results: list[CaseResult] = []
    for case in cases:
        if args.verbose:
            print(f"→ {case.id}: {case.title}", file=sys.stderr)
        results.append(run_case(base, case, args.verbose))

    print()
    print(f"{'ID':<16} {'OK':<5} {'ms':>7} {'att':>4}  title / detail")
    print("-" * 88)
    failed = 0
    for r in results:
        mark = "PASS" if r.ok else "FAIL"
        if not r.ok:
            failed += 1
        print(
            f"{r.id:<16} {mark:<5} {r.ms:>7} {r.attempts:>4}  {r.title}"
        )
        if not r.ok or args.verbose:
            print(f"{'':16}       detail: {r.detail}")

    print("-" * 88)
    print(f"integrity: {len(results) - failed}/{len(results)} passed")

    hydrate_note = ""
    if args.hydrate_probe:
        print()
        print(f"hydrate probe: {args.hydrate_probe} symbol={args.hydrate_symbol}")
        hok, hdetail = hydrate_probe(
            base,
            args.hydrate_probe,
            symbol=args.hydrate_symbol,
            verbose=args.verbose,
        )
        print(hdetail)
        hydrate_note = hdetail
        if not hok and args.hydrate_fail:
            failed += 1

    # Machine-readable trailer for logs
    print()
    print(
        "JSON "
        + json.dumps(
            {
                "base": base,
                "failed": failed,
                "results": [
                    {
                        "id": r.id,
                        "ok": r.ok,
                        "ms": r.ms,
                        "attempts": r.attempts,
                        "detail": r.detail,
                        "mode": r.mode,
                    }
                    for r in results
                ],
                "hydrate": hydrate_note or None,
            }
        )
    )

    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
