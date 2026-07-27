#!/usr/bin/env python3
"""Butler Alpha gate suite (A1) — one fixed tip check.

Orchestrates existing probes into a single pass/fail for map Alpha.

  python3 scripts/butler_alpha_gate.py
  python3 scripts/butler_alpha_gate.py --base http://127.0.0.1:8002 -v
  python3 scripts/butler_alpha_gate.py --strict-soft   # soft holes fail too
  python3 scripts/butler_alpha_gate.py --lanes health,integrity,watcher

Exit:
  0 — all lanes pass (spectacular=0; soft ignored unless --strict-soft)
  1 — one or more lanes failed
  2 — infra (health down, missing scripts)

Reports:
  plans/receipts/alpha-gate-latest.json
  plans/receipts/alpha-gate-<utc>.json
  per-lane JSON under plans/receipts/alpha-gate-lanes/

Not A1: full Qwen agent loop, Aider, gold harvest (A2/A3/A6).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

REPO = Path(__file__).resolve().parents[1]
SCRIPTS = REPO / "scripts"
RECEIPTS = Path(os.environ.get("BUTLER_RECEIPTS_DIR", str(Path("/tmp") / "butler-alpha-receipts")))
LANE_DIR = RECEIPTS / "alpha-gate-lanes"

DEFAULT_BASE = os.environ.get("BUTLER_URL", "http://127.0.0.1:8002").rstrip("/")

# Host-side roots used by watcher / hole probes (container maps via /projects).
HOST_PROJECTS = Path(os.environ.get("BUTLER_HOST_MOUNT", str(Path.home() / "projects")))
CONT_PREFIX = os.environ.get("BUTLER_CONTAINER_MOUNT", "/projects")


def host_to_container(host: Path | str) -> str:
    p = str(host)
    h = str(HOST_PROJECTS)
    if p.startswith(h):
        return CONT_PREFIX + p[len(h) :]
    return p


# Alpha keepers (host paths). Missing dirs are skipped with a note, not hard-fail
# except when a lane requires them (watcher → word-count).
KEEPERS_HOST = {
    "word-count": HOST_PROJECTS / "test_repos/pyo3/examples/word-count",
    "click": HOST_PROJECTS / "test_repos/click",
    "gin": HOST_PROJECTS / "test_repos/gin",
    "wisperer": HOST_PROJECTS / "lambda-wisperer",
    "pybind11": HOST_PROJECTS / "test_repos/pybind11",
}


@dataclass
class LaneResult:
    id: str
    title: str
    ok: bool
    exit_code: int
    seconds: float
    spectacular: int = 0
    soft: int = 0
    failed: int = 0  # integrity-style fail count
    detail: str = ""
    report: str = ""  # path to lane JSON if any
    skipped: bool = False


def http_get_json(url: str, timeout: float = 15.0) -> tuple[Optional[dict], Optional[str]]:
    try:
        with urllib.request.urlopen(url, timeout=timeout) as resp:
            return json.loads(resp.read().decode()), None
    except Exception as e:
        return None, str(e)


def http_post_json(url: str, payload: dict, timeout: float = 60.0) -> tuple[Optional[dict], Optional[str]]:
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        url, data=data, headers={"Content-Type": "application/json"}, method="POST"
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode()), None
    except Exception as e:
        return None, str(e)


def load_summary(path: Path) -> dict[str, Any]:
    if not path.is_file():
        return {}
    try:
        d = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return {}
    if not isinstance(d, dict):
        return {}
    s = d.get("summary")
    if isinstance(s, dict):
        return {"summary": s, "root": d}
    return {"summary": {}, "root": d}


def spectacular_soft_from_report(path: Path) -> tuple[int, int]:
    info = load_summary(path)
    s = info.get("summary") or {}
    if s:
        return int(s.get("spectacular") or 0), int(s.get("soft") or 0)
    root = info.get("root") or {}
    # hole / ffi style: fails list
    fails = root.get("fails") or root.get("holes") or []
    if isinstance(fails, list) and fails:
        spec = sum(
            1
            for f in fails
            if isinstance(f, dict) and f.get("severity") == "spectacular"
        )
        soft = sum(
            1 for f in fails if isinstance(f, dict) and f.get("severity") == "soft"
        )
        return spec, soft
    return 0, 0


def parse_integrity_stdout(stdout: str) -> tuple[int, int]:
    """Return (failed, total_hint). Parse trailing JSON line."""
    failed = 0
    for line in stdout.splitlines():
        if line.startswith("JSON "):
            try:
                d = json.loads(line[5:])
                failed = int(d.get("failed") or 0)
                return failed, len(d.get("results") or [])
            except Exception:
                pass
    m = re.search(r"integrity:\s*(\d+)/(\d+)\s+passed", stdout)
    if m:
        ok, total = int(m.group(1)), int(m.group(2))
        return total - ok, total
    return -1, 0


def run_cmd(
    argv: list[str],
    *,
    timeout: float,
    env: Optional[dict[str, str]] = None,
) -> tuple[int, str, str, float]:
    t0 = time.perf_counter()
    try:
        p = subprocess.run(
            argv,
            cwd=str(REPO),
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
        )
        sec = time.perf_counter() - t0
        return p.returncode, p.stdout or "", p.stderr or "", sec
    except subprocess.TimeoutExpired as e:
        sec = time.perf_counter() - t0
        out = (e.stdout or "") if isinstance(e.stdout, str) else ""
        err = (e.stderr or "") if isinstance(e.stderr, str) else f"timeout after {timeout}s"
        return 124, out, err, sec


def ensure_dirs() -> None:
    RECEIPTS.mkdir(parents=True, exist_ok=True)
    LANE_DIR.mkdir(parents=True, exist_ok=True)


def lane_health(base: str) -> LaneResult:
    t0 = time.perf_counter()
    body, err = http_get_json(f"{base}/mcp/health")
    sec = time.perf_counter() - t0
    if err or not body:
        return LaneResult(
            id="health",
            title="MCP health",
            ok=False,
            exit_code=2,
            seconds=sec,
            detail=err or "empty body",
        )
    st = body.get("status")
    fp = body.get("fingerprint") or ""
    loaded = body.get("loaded") or {}
    ok = st == "ok"
    return LaneResult(
        id="health",
        title="MCP health",
        ok=ok,
        exit_code=0 if ok else 1,
        seconds=sec,
        detail=f"status={st} fingerprint={fp} loaded={len(loaded)}",
    )


def warm_keepers(base: str, verbose: bool) -> list[str]:
    """POST /warm with host roots (server maps to warehouse keys)."""
    notes: list[str] = []
    existing: list[str] = []
    for name, host in KEEPERS_HOST.items():
        if not host.is_dir():
            notes.append(f"skip warm {name}: missing {host}")
            continue
        existing.append(str(host))
        notes.append(f"queue warm {name}: {host}")
    if not existing:
        notes.append("no keepers on disk to warm")
        return notes
    body, err = http_post_json(f"{base}/warm", {"roots": existing}, timeout=60.0)
    if err:
        notes.append(f"warm request failed: {err}")
    else:
        msg = ""
        if isinstance(body, dict):
            msg = str(body.get("message") or body.get("ok") or "")
        notes.append(f"warm ack {len(existing)} root(s){(': ' + msg) if msg else ''}")
    return notes


def wait_ready(base: str, cont_key: str, timeout_s: float, verbose: bool) -> bool:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        body, err = http_get_json(f"{base}/mcp/health")
        if body:
            loaded = body.get("loaded") or {}
            info = loaded.get(cont_key) or {}
            if info.get("ready") or info.get("edges_complete"):
                return True
            # path-form tolerant
            for k, v in loaded.items():
                if cont_key.rstrip("/") in k or k in cont_key:
                    if v.get("ready") or v.get("edges_complete"):
                        return True
        if verbose:
            print(f"  … wait ready {cont_key}", file=sys.stderr)
        time.sleep(2.0)
    return False


def py(script: str) -> list[str]:
    return [sys.executable, str(SCRIPTS / script)]


def run_lane_integrity(base: str, verbose: bool) -> LaneResult:
    report = LANE_DIR / "integrity.json"
    argv = py("butler_integrity_gate.py") + ["--base", base]
    if verbose:
        argv.append("-v")
    code, out, err, sec = run_cmd(argv, timeout=600)
    failed, total = parse_integrity_stdout(out)
    if failed < 0:
        failed = 0 if code == 0 else 1
    # Write a small report for the aggregate
    report.write_text(
        json.dumps(
            {
                "exit_code": code,
                "failed": failed,
                "total_hint": total,
                "stdout_tail": out[-4000:],
                "stderr_tail": err[-2000:],
            },
            indent=2,
        ),
        encoding="utf-8",
    )
    ok = code == 0 and failed == 0
    return LaneResult(
        id="integrity",
        title="Structural integrity (export/IPC/same-lang)",
        ok=ok,
        exit_code=code,
        seconds=sec,
        spectacular=failed if not ok else 0,
        failed=max(failed, 0),
        detail=f"failed={failed}" + (f" total≈{total}" if total else ""),
        report=str(report),
    )


def run_lane_watcher(base: str, verbose: bool) -> LaneResult:
    wc = KEEPERS_HOST["word-count"]
    if not wc.is_dir() or not (wc / "src" / "lib.rs").is_file():
        return LaneResult(
            id="watcher",
            title="Watcher incremental (single+multi)",
            ok=False,
            exit_code=2,
            seconds=0.0,
            detail=f"missing fixture {wc}",
        )
    report = LANE_DIR / "watcher.json"
    env = os.environ.copy()
    env["BUTLER_URL"] = base
    env["BUTLER_WATCHER_PROBE_ROOT"] = str(wc)
    argv = py("butler_watcher_probe.py") + [
        "--base",
        base,
        "--mode",
        "all",
        "--json",
        str(report),
        "--wait",
        "15",
    ]
    if verbose:
        argv.append("-v")
    code, out, err, sec = run_cmd(argv, timeout=300, env=env)
    spec, soft = spectacular_soft_from_report(report)
    # watcher exits 1 only on spectacular
    ok = code == 0 and spec == 0
    return LaneResult(
        id="watcher",
        title="Watcher incremental (single+multi)",
        ok=ok,
        exit_code=code,
        seconds=sec,
        spectacular=spec,
        soft=soft,
        detail=f"spectacular={spec} soft={soft}",
        report=str(report),
    )


def run_lane_ffi(base: str, verbose: bool) -> LaneResult:
    report = LANE_DIR / "ffi.json"
    argv = py("butler_ffi_hole_probe.py") + [
        "--base",
        base,
        "--json",
        str(report),
    ]
    if verbose:
        argv.append("-v")
    code, out, err, sec = run_cmd(argv, timeout=600)
    spec, soft = spectacular_soft_from_report(report)
    if not report.is_file():
        # try parse summary from stdout
        m = re.search(r"spectacular=(\d+).*soft=(\d+)", out + err)
        if m:
            spec, soft = int(m.group(1)), int(m.group(2))
    ok = code == 0 and spec == 0
    return LaneResult(
        id="ffi",
        title="FFI / interconnect hole probe",
        ok=ok,
        exit_code=code,
        seconds=sec,
        spectacular=spec,
        soft=soft,
        detail=f"spectacular={spec} soft={soft}",
        report=str(report) if report.is_file() else "",
    )


def run_lane_hole(base: str, verbose: bool) -> LaneResult:
    report = LANE_DIR / "hole.json"
    # Prefer wisperer + click container paths when present
    roots: list[str] = []
    for key in ("wisperer", "click"):
        h = KEEPERS_HOST[key]
        if h.is_dir():
            roots.append(host_to_container(h))
    argv = py("butler_hole_probe.py") + [
        "--base",
        base,
        "--json",
        str(report),
        "--max-seeds",
        "12",
        "--bi-budget",
        "4",
    ]
    if roots:
        argv.extend(["--roots", ",".join(roots)])
    if verbose:
        argv.append("-v")
    code, out, err, sec = run_cmd(argv, timeout=900)
    spec, soft = spectacular_soft_from_report(report)
    if not report.is_file():
        m = re.search(r"spectacular=(\d+).*soft=(\d+)", out + err)
        if m:
            spec, soft = int(m.group(1)), int(m.group(2))
    ok = code == 0 and spec == 0
    return LaneResult(
        id="hole",
        title="Trace honesty hole probe (keepers)",
        ok=ok,
        exit_code=code,
        seconds=sec,
        spectacular=spec,
        soft=soft,
        detail=f"spectacular={spec} soft={soft} roots={roots or 'health-loaded'}",
        report=str(report) if report.is_file() else "",
    )


def run_lane_homonym(base: str, verbose: bool) -> LaneResult:
    report = LANE_DIR / "homonym.json"
    argv = py("butler_homonym_hole_probe.py") + [
        "--base",
        base,
        "--json",
        str(report),
    ]
    if verbose:
        argv.append("-v")
    code, out, err, sec = run_cmd(argv, timeout=600)
    spec, soft = spectacular_soft_from_report(report)
    if not report.is_file():
        m = re.search(r"spectacular=(\d+).*soft=(\d+)", out + err)
        if m:
            spec, soft = int(m.group(1)), int(m.group(2))
    ok = code == 0 and spec == 0
    return LaneResult(
        id="homonym",
        title="Homonym / multi-loc pin probe",
        ok=ok,
        exit_code=code,
        seconds=sec,
        spectacular=spec,
        soft=soft,
        detail=f"spectacular={spec} soft={soft}",
        report=str(report) if report.is_file() else "",
    )


def run_lane_spectacular(base: str, verbose: bool) -> LaneResult:
    """Silent-lie mill: multi-def pins + dual oracle (peer/qualifier/wrong ★)."""
    report = LANE_DIR / "spectacular.json"
    latest = RECEIPTS / "spectacular-latest.json"
    argv = py("butler_spectacular_probe.py") + [
        "--base",
        base,
        "--json",
        str(report),
    ]
    if verbose:
        argv.append("-v")
    code, out, err, sec = run_cmd(argv, timeout=1200)
    spec, soft = spectacular_soft_from_report(report)
    if not report.is_file():
        m = re.search(r"spectacular=(\d+).*soft=(\d+)", out + err)
        if m:
            spec, soft = int(m.group(1)), int(m.group(2))
    # Mirror latest for dogfood readers
    if report.is_file():
        try:
            latest.write_text(report.read_text(encoding="utf-8"), encoding="utf-8")
        except OSError:
            pass
    ok = code == 0 and spec == 0
    return LaneResult(
        id="spectacular",
        title="Spectacular silent-lie mill (collision + dual oracle)",
        ok=ok,
        exit_code=code,
        seconds=sec,
        spectacular=spec,
        soft=soft,
        detail=f"spectacular={spec} soft={soft}",
        report=str(report) if report.is_file() else "",
    )


def run_lane_adversarial_gold(base: str, verbose: bool) -> LaneResult:
    report = LANE_DIR / "adversarial_gold.json"
    argv = py("butler_dogfood_adversarial.py") + [
        "--base",
        base,
        "--suite",
        "gold",
        "--json",
        str(report),
        "--no-warm",  # alpha gate warms keepers up front
        "--warm-wait",
        "60",
    ]
    if verbose:
        argv.append("-v")
    code, out, err, sec = run_cmd(argv, timeout=900)
    info = load_summary(report)
    s = info.get("summary") or {}
    spec = int(s.get("spectacular") or 0)
    soft = int(s.get("soft") or 0)
    if not s and report.is_file():
        root = info.get("root") or {}
        fails = root.get("fails") or []
        if isinstance(fails, list):
            spec = sum(
                1
                for f in fails
                if isinstance(f, dict) and f.get("severity") == "spectacular"
            )
            soft = sum(
                1 for f in fails if isinstance(f, dict) and f.get("severity") == "soft"
            )
    ok = code == 0 and spec == 0
    return LaneResult(
        id="adversarial_gold",
        title="Adversarial dogfood (gold suite only)",
        ok=ok,
        exit_code=code,
        seconds=sec,
        spectacular=spec,
        soft=soft,
        detail=f"spectacular={spec} soft={soft}",
        report=str(report) if report.is_file() else "",
    )


def run_lane_desirability_curl(base: str, verbose: bool) -> LaneResult:
    script = SCRIPTS / "desirability_gate_curl.sh"
    if not script.is_file():
        return LaneResult(
            id="desirability_curl",
            title="Desirability P1 structural curl",
            ok=False,
            exit_code=2,
            seconds=0.0,
            detail="missing desirability_gate_curl.sh",
        )
    argv = ["bash", str(script), base]
    code, out, err, sec = run_cmd(argv, timeout=300)
    # script exits non-zero on fail
    ok = code == 0
    # crude soft/fail from FAIL lines
    fails = len(re.findall(r"^\s*FAIL\b", out, re.M))
    report = LANE_DIR / "desirability_curl.txt"
    report.write_text(out + ("\n" + err if err else ""), encoding="utf-8")
    return LaneResult(
        id="desirability_curl",
        title="Desirability P1 structural curl",
        ok=ok,
        exit_code=code,
        seconds=sec,
        spectacular=fails if not ok else 0,
        failed=fails,
        detail=f"exit={code} fail_lines≈{fails}",
        report=str(report),
    )


LANE_RUNNERS = {
    "health": lambda base, v: lane_health(base),
    "integrity": run_lane_integrity,
    "watcher": run_lane_watcher,
    "ffi": run_lane_ffi,
    "hole": run_lane_hole,
    "homonym": run_lane_homonym,
    "spectacular": run_lane_spectacular,
    "adversarial_gold": run_lane_adversarial_gold,
    "desirability_curl": run_lane_desirability_curl,
}

# Default order: MVP first, then A1.1 lanes
DEFAULT_LANES = [
    "health",
    "integrity",
    "watcher",
    "ffi",
    "hole",
    "homonym",
    "spectacular",
    "adversarial_gold",
    "desirability_curl",
]


def apply_soft_policy(lane: LaneResult, strict_soft: bool) -> LaneResult:
    if lane.skipped or not lane.ok:
        return lane
    if strict_soft and lane.soft > 0:
        lane.ok = False
        lane.detail = (lane.detail + f" [strict-soft: soft={lane.soft}]").strip()
    return lane


def main() -> int:
    ap = argparse.ArgumentParser(description="Butler Alpha gate suite (A1)")
    ap.add_argument("--base", default=DEFAULT_BASE, help="Butler base URL")
    ap.add_argument(
        "--lanes",
        default=",".join(DEFAULT_LANES),
        help=f"Comma list of lanes (default: all). Known: {','.join(LANE_RUNNERS)}",
    )
    ap.add_argument(
        "--strict-soft",
        action="store_true",
        help="Fail the gate if any lane reports soft>0",
    )
    ap.add_argument(
        "--no-warm",
        action="store_true",
        help="Skip /warm of Alpha keepers",
    )
    ap.add_argument(
        "--warm-wait",
        type=float,
        default=90.0,
        help="Seconds to wait for word-count ready after warm (default 90)",
    )
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument(
        "--json",
        default="",
        help="Override report path (default plans/receipts/alpha-gate-latest.json)",
    )
    args = ap.parse_args()
    base = args.base.rstrip("/")
    ensure_dirs()

    want = [x.strip() for x in args.lanes.split(",") if x.strip()]
    unknown = [x for x in want if x not in LANE_RUNNERS]
    if unknown:
        print(f"unknown lanes: {unknown}", file=sys.stderr)
        print(f"known: {', '.join(LANE_RUNNERS)}", file=sys.stderr)
        return 2

    print("=" * 64)
    print("BUTLER ALPHA GATE (A1)")
    print("=" * 64)
    print(f"base={base}")
    print(f"lanes={','.join(want)}")
    print(f"strict_soft={args.strict_soft}")
    print(f"repo={REPO}")
    print()

    # Preflight scripts
    for name in (
        "butler_integrity_gate.py",
        "butler_watcher_probe.py",
        "butler_ffi_hole_probe.py",
        "butler_hole_probe.py",
        "butler_homonym_hole_probe.py",
        "butler_dogfood_adversarial.py",
    ):
        if not (SCRIPTS / name).is_file():
            print(f"FAIL missing script {name}", file=sys.stderr)
            return 2

    t_all = time.perf_counter()
    results: list[LaneResult] = []

    # Health first always if present
    if "health" in want:
        r = LANE_RUNNERS["health"](base, args.verbose)
        results.append(r)
        print(f"[{'PASS' if r.ok else 'FAIL'}] health  {r.detail}  ({r.seconds:.1f}s)")
        if not r.ok:
            return _finish(base, results, time.perf_counter() - t_all, args, exit_code=2)

    if not args.no_warm:
        print("\n--- warm keepers ---")
        for note in warm_keepers(base, args.verbose):
            print(f"  {note}")
        wc_cont = host_to_container(KEEPERS_HOST["word-count"])
        if KEEPERS_HOST["word-count"].is_dir():
            ready = wait_ready(base, wc_cont, args.warm_wait, args.verbose)
            print(f"  word-count ready={ready} ({wc_cont})")

    print("\n--- lanes ---")
    for lid in want:
        if lid == "health":
            continue
        if args.verbose:
            print(f"→ {lid} …", file=sys.stderr)
        runner = LANE_RUNNERS[lid]
        r = runner(base, args.verbose)
        r = apply_soft_policy(r, args.strict_soft)
        results.append(r)
        mark = "PASS" if r.ok else "FAIL"
        if r.skipped:
            mark = "SKIP"
        print(
            f"[{mark}] {lid:<18} {r.detail}  "
            f"spec={r.spectacular} soft={r.soft}  ({r.seconds:.1f}s)"
        )
        if args.verbose and r.report:
            print(f"         report={r.report}")

    total_s = time.perf_counter() - t_all
    failed_lanes = [r for r in results if not r.ok and not r.skipped]
    exit_code = 0 if not failed_lanes else 1
    return _finish(base, results, total_s, args, exit_code=exit_code)


def _finish(
    base: str,
    results: list[LaneResult],
    total_s: float,
    args: argparse.Namespace,
    exit_code: int,
) -> int:
    health_body, _ = http_get_json(f"{base}/mcp/health")
    fp = (health_body or {}).get("fingerprint") or ""
    utc = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    total_spec = sum(r.spectacular for r in results)
    total_soft = sum(r.soft for r in results)
    passed = sum(1 for r in results if r.ok)
    failed = sum(1 for r in results if not r.ok and not r.skipped)

    report = {
        "gate": "alpha",
        "version": 1,
        "utc": utc,
        "base": base,
        "fingerprint": fp,
        "strict_soft": bool(args.strict_soft),
        "seconds": round(total_s, 2),
        "summary": {
            "ok": exit_code == 0,
            "lanes_passed": passed,
            "lanes_failed": failed,
            "spectacular": total_spec,
            "soft": total_soft,
            "exit_code": exit_code,
        },
        "lanes": [asdict(r) for r in results],
        "keepers_host": {k: str(v) for k, v in KEEPERS_HOST.items()},
    }

    latest = Path(args.json) if args.json else RECEIPTS / "alpha-gate-latest.json"
    stamped = RECEIPTS / f"alpha-gate-{utc}.json"
    latest.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(report, indent=2)
    latest.write_text(text, encoding="utf-8")
    if not args.json:
        stamped.write_text(text, encoding="utf-8")

    print()
    print("=" * 64)
    print("ALPHA GATE REPORT")
    print("=" * 64)
    for r in results:
        mark = "PASS" if r.ok else ("SKIP" if r.skipped else "FAIL")
        print(f"  {mark:<4} {r.id:<18} {r.detail}")
    print(
        f"\nTOTAL lanes_passed={passed} lanes_failed={failed} "
        f"spectacular={total_spec} soft={total_soft} wall={total_s:.1f}s"
    )
    print(f"fingerprint={fp}")
    print(f"Wrote {latest}")
    if not args.json:
        print(f"Wrote {stamped}")
    print(f"exit={exit_code}")
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
