#!/usr/bin/env python3
"""Watcher / incremental graph dogfood: edit → settle → Trace sees change.

Proves the desirability lever: warm warehouse + FS watcher re-edge without
full monorepo rebuild. Default root: pyo3 word-count (tiny, dual-stack gold).

Modes:
  single  — inject into src/lib.rs (same-file CALL)
  multi   — two new .rs files + cross-file CALL + dual-file remove (batch)
  all     — single then multi (default; highest value)

Checks:
  W1  After insert: Trace finds new symbol
  W2  CALL edge parent → callee (and reverse)
  W3  After remove: no stale high ★
  M1–M3 same for multi-file / cross-file
  M4  Both files gone after delete (Remove re-edge)

Usage:
  python3 scripts/butler_watcher_probe.py
  python3 scripts/butler_watcher_probe.py --mode multi -v
  python3 scripts/butler_watcher_probe.py --base http://127.0.0.1:8002 --mode all

Exit 0 if no spectacular failures.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.request
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Callable, Optional

DEFAULT_BASE = os.environ.get("BUTLER_URL", "http://127.0.0.1:8002").rstrip("/")

# Host projects root (override with BUTLER_HOST_MOUNT)
HOST_ROOT = Path(os.environ.get("BUTLER_HOST_MOUNT", str(Path.home() / "projects")))

def _hp(rel: str) -> str:
    """Absolute path under HOST_ROOT (or container-friendly host mount)."""
    return str(HOST_ROOT / rel)

WC = Path(
    os.environ.get(
        "BUTLER_WATCHER_PROBE_ROOT",
        _hp("test_repos/pyo3/examples/word-count"),
    )
)
LIB = WC / "src" / "lib.rs"
MARKER_BEGIN = "// === BUTLER_WATCHER_PROBE_BEGIN ==="
MARKER_END = "// === BUTLER_WATCHER_PROBE_END ==="
MF_A = "butler_mf_probe_a.rs"
MF_B = "butler_mf_probe_b.rs"


def _suffix() -> str:
    return f"{int(time.time()) % 100000:05d}"


@dataclass
class Fail:
    inv: str
    severity: str
    detail: str
    ms: float = 0.0
    evidence: dict[str, Any] = field(default_factory=dict)


def post_context(
    base: str, payload: dict, timeout: float = 60.0
) -> tuple[dict, float, Optional[str]]:
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        f"{base}/context",
        data=data,
        headers={"Content-Type": "application/json"},
    )
    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            body = resp.read().decode()
            ms = (time.perf_counter() - t0) * 1000
            return json.loads(body), ms, None
    except Exception as e:
        return {}, (time.perf_counter() - t0) * 1000, str(e)


def structured(d: dict) -> dict:
    st = d.get("structured") or d.get("structuredContent") or {}
    return st if isinstance(st, dict) else {}


def content_text(d: dict) -> str:
    c = d.get("content")
    if isinstance(c, list) and c:
        return (c[0] or {}).get("text", "") or ""
    if isinstance(c, str):
        return c
    return ""


def receipt(st: dict, content: str) -> dict[str, str]:
    rec = st.get("receipt") if isinstance(st.get("receipt"), dict) else {}
    if not rec and isinstance(st.get("telemetry"), dict):
        tr = st["telemetry"].get("receipt")
        if isinstance(tr, dict):
            rec = tr
    return {
        "confidence": (rec.get("confidence") or "").lower(),
        "basis": (rec.get("basis") or "").lower(),
        "edges": (rec.get("edges") or "").lower(),
    }


def is_building(content: str) -> bool:
    return content.startswith("=== Building") or "BUILDING" in content[:200]


def is_miss(content: str, st: dict) -> bool:
    if st.get("error") and not st.get("target"):
        return True
    if content.startswith("Orchestrate error") or "not found" in content.lower()[:200]:
        return True
    if (st.get("blast_domain") or "") in ("scope_not_found",):
        return True
    return not (st.get("target") or {})


def warm(base: str, root: Path) -> None:
    data = json.dumps({"roots": [str(root)]}).encode()
    req = urllib.request.Request(
        f"{base}/warm",
        data=data,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            resp.read()
    except Exception:
        pass


def wait_ready(base: str, cont_key: str, timeout_s: float = 120.0) -> bool:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f"{base}/mcp/health", timeout=10) as resp:
                h = json.loads(resp.read().decode())
            v = (h.get("loaded") or {}).get(cont_key) or {}
            if v.get("ready") and v.get("edges_complete"):
                return True
        except Exception:
            pass
        time.sleep(2)
    return False


def strip_probe_block(src: str) -> str:
    if MARKER_BEGIN not in src:
        return src
    out = []
    skip = False
    for line in src.splitlines(keepends=True):
        if MARKER_BEGIN in line:
            skip = True
            continue
        if MARKER_END in line:
            skip = False
            continue
        if not skip:
            out.append(line)
    return "".join(out)


def inject_probe(src: str, parent: str, callee: str) -> str:
    clean = strip_probe_block(src).rstrip() + "\n"
    block = f"""
{MARKER_BEGIN}
/// Butler watcher probe — safe to delete; not product code.
#[allow(dead_code)]
pub fn {parent}() {{
    {callee}();
}}

#[allow(dead_code)]
pub fn {callee}() {{}}
{MARKER_END}
"""
    return clean + block


def trace(
    base: str, project: str, symbol: str, scope: Optional[list[str]] = None
) -> tuple[str, dict, dict, float, Optional[str]]:
    payload: dict[str, Any] = {
        "project": project,
        "target_symbol": symbol,
        "goal": "TraceBlastRadius",
        "detail": "short",
    }
    if scope:
        payload["scope_paths"] = scope
    d, ms, err = post_context(base, payload)
    c = content_text(d)
    st = structured(d)
    if not err and is_building(c):
        time.sleep(1.5)
        d, ms2, err = post_context(base, payload)
        c = content_text(d)
        st = structured(d)
        ms += ms2
    return c, st, receipt(st, c), ms, err


def poll_until(
    pred: Callable,
    base: str,
    project: str,
    symbol: str,
    scope: Optional[list[str]],
    timeout_s: float,
    interval: float = 0.35,
) -> tuple[bool, str, dict, dict, float]:
    deadline = time.time() + timeout_s
    total_ms = 0.0
    last = ("", {}, {}, 0.0)
    while time.time() < deadline:
        c, st, rec, ms, err = trace(base, project, symbol, scope)
        total_ms += ms
        last = (c, st, rec, total_ms)
        if err:
            time.sleep(interval)
            continue
        if is_building(c):
            time.sleep(interval)
            continue
        if pred(c, st):
            return True, c, st, rec, total_ms
        time.sleep(interval)
    c, st, rec, total_ms = last
    return False, c, st, rec, total_ms


def run_single(
    base: str,
    wait: float,
    verbose: bool,
    fail: Callable,
    stats: dict,
) -> None:
    """Same-file inject into lib.rs."""
    print("\n--- single-file (lib.rs inject) ---")
    original = LIB.read_text(encoding="utf-8")
    cleaned = strip_probe_block(original)
    if cleaned != original:
        LIB.write_text(cleaned, encoding="utf-8")
        if verbose:
            print("  stripped leftover probe block")
        time.sleep(1.0)
    original = LIB.read_text(encoding="utf-8")

    suf = _suffix()
    parent = f"butler_watcher_probe_parent_{suf}"
    callee = f"butler_watcher_probe_callee_{suf}"
    scope = ["src/"]
    project = str(WC)

    c0, st0, _, ms0, err0 = trace(base, project, parent, scope)
    if err0:
        fail("W0_baseline", "soft", f"baseline Trace error: {err0}", ms0)
    elif not is_miss(c0, st0) and (st0.get("target") or {}).get("name") == parent:
        fail(
            "W0_baseline_ghost",
            "spectacular",
            f"symbol {parent} already exists before insert",
            ms0,
        )
    stats["steps"].append({"step": "single_baseline", "miss": is_miss(c0, st0)})
    if verbose:
        print(f"  baseline miss={is_miss(c0, st0)}")

    restored = False
    try:
        LIB.write_text(inject_probe(original, parent, callee), encoding="utf-8")
        t_write = time.time()
        if verbose:
            print(f"  wrote {parent} / {callee}")

        def has_star(c, st):
            t = st.get("target") or {}
            return (t.get("name") or "") == parent and not is_miss(c, st)

        ok1, c1, st1, rec1, ms1 = poll_until(
            has_star, base, project, parent, scope, wait
        )
        appear_s = time.time() - t_write
        stats["steps"].append(
            {"step": "single_appear", "ok": ok1, "wall_s": appear_s, "receipt": rec1}
        )
        if not ok1:
            fail(
                "W1_appear",
                "spectacular",
                f"after insert, Trace({parent}) no ★ within {wait}s wall={appear_s:.1f}s",
                ms1,
            )
        elif appear_s > 8.0:
            fail(
                "W1_appear_slow",
                "soft",
                f"appeared but slow wall={appear_s:.1f}s",
                ms1,
            )
        if verbose:
            print(f"  W1 appear ok={ok1} wall={appear_s:.2f}s")

        if ok1:
            cnames = {x.get("name") for x in (st1.get("callees") or []) if x.get("name")}
            _, st2, _, ms2, _ = trace(base, project, callee, scope)
            pnames = {x.get("name") for x in (st2.get("callers") or []) if x.get("name")}
            edge_fwd, edge_rev = callee in cnames, parent in pnames
            stats["steps"].append(
                {"step": "single_edge", "fwd": edge_fwd, "rev": edge_rev}
            )
            if not edge_fwd and not edge_rev:
                fail(
                    "W2_call_edge",
                    "spectacular",
                    f"no CALL {parent}→{callee}",
                    ms2,
                )
            elif not edge_fwd or not edge_rev:
                fail(
                    "W2_call_edge_asymmetry",
                    "soft",
                    f"CALL partial fwd={edge_fwd} rev={edge_rev}",
                    ms2,
                )
            if verbose:
                print(f"  W2 call edge fwd={edge_fwd} rev={edge_rev}")

        LIB.write_text(original, encoding="utf-8")
        restored = True
        t_rm = time.time()

        def is_gone(c, st):
            t = st.get("target") or {}
            if is_miss(c, st):
                return True
            return (t.get("name") or "") != parent

        ok3, c3, st3, rec3, ms3 = poll_until(
            is_gone, base, project, parent, scope, wait
        )
        gone_s = time.time() - t_rm
        still_star = (st3.get("target") or {}).get("name") == parent and not is_miss(
            c3, st3
        )
        conf = (rec3.get("confidence") or "").lower()
        stats["steps"].append(
            {
                "step": "single_disappear",
                "ok": ok3,
                "still_star": still_star,
                "wall_s": gone_s,
            }
        )
        if still_star and conf == "high":
            fail(
                "W3_stale_star",
                "spectacular",
                f"after remove still high ★ {parent} wall={gone_s:.1f}s",
                ms3,
            )
        elif still_star:
            fail(
                "W3_stale_star",
                "soft",
                f"after remove still ★ {parent} conf={conf}",
                ms3,
            )
        elif not ok3:
            fail(
                "W3_disappear",
                "soft",
                f"miss not confirmed within {wait}s",
                ms3,
            )
        if verbose:
            print(f"  W3 disappear ok={ok3} still_star={still_star} wall={gone_s:.2f}s")
    finally:
        if not restored:
            try:
                LIB.write_text(original, encoding="utf-8")
                print("  restored lib.rs (finally)")
            except Exception as e:
                print(f"  CRITICAL: restore lib.rs failed: {e}")


def run_multi(
    base: str,
    wait: float,
    verbose: bool,
    fail: Callable,
    stats: dict,
) -> None:
    """Two new files: cross-file CALL + dual Create/Remove batch."""
    print("\n--- multi-file (cross-file CALL + batch create/remove) ---")
    src = WC / "src"
    path_a = src / MF_A
    path_b = src / MF_B
    # cleanup leftovers
    for p in (path_a, path_b):
        if p.exists():
            p.unlink()
            if verbose:
                print(f"  removed leftover {p.name}")
    if path_a.exists() or path_b.exists():
        time.sleep(1.2)

    suf = _suffix()
    parent = f"butler_mf_parent_{suf}"
    callee = f"butler_mf_callee_{suf}"
    scope = ["src/"]
    project = str(WC)

    # Baseline miss
    c0, st0, _, ms0, _ = trace(base, project, parent, scope)
    if not is_miss(c0, st0) and (st0.get("target") or {}).get("name") == parent:
        fail(
            "M0_baseline_ghost",
            "spectacular",
            f"multi parent {parent} exists before create",
            ms0,
        )
    stats["steps"].append({"step": "multi_baseline", "miss": is_miss(c0, st0)})

    created = False
    try:
        # Callee file first, then parent that calls it (cross-file by name).
        body_b = f"""// Butler multi-file watcher probe B — safe to delete
#[allow(dead_code)]
pub fn {callee}() {{}}
"""
        body_a = f"""// Butler multi-file watcher probe A — safe to delete
#[allow(dead_code)]
pub fn {parent}() {{
    {callee}();
}}
"""
        # Near-simultaneous writes → one watcher batch if possible
        path_b.write_text(body_b, encoding="utf-8")
        path_a.write_text(body_a, encoding="utf-8")
        created = True
        t_write = time.time()
        if verbose:
            print(f"  created {MF_A} + {MF_B} ({parent} → {callee})")

        def has_star(c, st, name=parent):
            t = st.get("target") or {}
            return (t.get("name") or "") == name and not is_miss(c, st)

        ok1, c1, st1, rec1, ms1 = poll_until(
            lambda c, st: has_star(c, st, parent),
            base,
            project,
            parent,
            scope,
            wait,
        )
        appear_s = time.time() - t_write
        star_file = ((st1.get("target") or {}).get("file") or "").replace("\\", "/")
        stats["steps"].append(
            {
                "step": "multi_appear",
                "ok": ok1,
                "wall_s": appear_s,
                "star_file": star_file,
            }
        )
        if not ok1:
            fail(
                "M1_appear",
                "spectacular",
                f"cross-file parent {parent} no ★ within {wait}s wall={appear_s:.1f}s",
                ms1,
            )
        elif MF_A not in star_file and "butler_mf_probe_a" not in star_file:
            fail(
                "M1_wrong_file",
                "soft",
                f"★ not in {MF_A}: {star_file}",
                ms1,
            )
        if verbose:
            print(f"  M1 appear ok={ok1} wall={appear_s:.2f}s file={star_file[-40:]}")

        if ok1:
            # Wait for callee file re-edge (batch may process A before B finishes).
            ok_cal, c2, st2, rec2, ms2 = poll_until(
                lambda c, st: has_star(c, st, callee),
                base,
                project,
                callee,
                scope,
                max(wait, 8.0),
            )
            cal_file = ((st2.get("target") or {}).get("file") or "").replace("\\", "/")
            if not ok_cal:
                fail(
                    "M2_callee_appear",
                    "spectacular",
                    f"cross-file callee {callee} no ★",
                    ms2,
                )
            else:
                # Re-Trace both after both files settled (cross-file link needs both sides).
                def edges_ready(_c, st):
                    cnames = {
                        x.get("name") for x in (st.get("callees") or []) if x.get("name")
                    }
                    return callee in cnames

                ok_fwd, _, st_p, _, ms_f = poll_until(
                    edges_ready, base, project, parent, scope, max(wait, 8.0)
                )
                _, st_c, _, ms_r, _ = trace(base, project, callee, scope)
                cnames = {
                    x.get("name") for x in (st_p.get("callees") or []) if x.get("name")
                }
                pnames = {
                    x.get("name") for x in (st_c.get("callers") or []) if x.get("name")
                }
                edge_fwd = callee in cnames
                edge_rev = parent in pnames
                stats["steps"].append(
                    {
                        "step": "multi_cross_edge",
                        "fwd": edge_fwd,
                        "rev": edge_rev,
                        "callee_ok": True,
                        "callee_file": cal_file,
                        "callees": sorted(cnames)[:12],
                        "callers": sorted(pnames)[:12],
                    }
                )
                if not edge_fwd and not edge_rev:
                    fail(
                        "M2_cross_call_edge",
                        "spectacular",
                        f"no cross-file CALL {parent}→{callee} (fwd={edge_fwd} rev={edge_rev})",
                        ms_f + ms_r,
                        callees=list(cnames)[:15],
                        callers=list(pnames)[:15],
                    )
                elif not edge_fwd or not edge_rev:
                    fail(
                        "M2_cross_call_asymmetry",
                        "soft",
                        f"cross-file CALL partial fwd={edge_fwd} rev={edge_rev}",
                        ms_f + ms_r,
                    )
                if verbose:
                    print(
                        f"  M2 cross-file CALL fwd={edge_fwd} rev={edge_rev} "
                        f"callee_file={cal_file[-40:]}"
                    )

            # Dual modify: touch both files (batch coalesce)
            path_a.write_text(
                body_a + f"\n// touch {time.time()}\n", encoding="utf-8"
            )
            path_b.write_text(
                body_b + f"\n// touch {time.time()}\n", encoding="utf-8"
            )
            t_touch = time.time()
            ok_t, _, st_t, _, ms_t = poll_until(
                lambda c, st: has_star(c, st, parent),
                base,
                project,
                parent,
                scope,
                wait,
            )
            touch_s = time.time() - t_touch
            stats["steps"].append(
                {"step": "multi_dual_touch", "ok": ok_t, "wall_s": touch_s}
            )
            if not ok_t:
                fail(
                    "M3_dual_touch",
                    "soft",
                    f"after dual-file modify, parent ★ lost wall={touch_s:.1f}s",
                    ms_t,
                )
            elif verbose:
                print(f"  M3 dual-touch still ★ ok wall={touch_s:.2f}s")

        # Delete both files (Remove path)
        path_a.unlink(missing_ok=True)
        path_b.unlink(missing_ok=True)
        created = False
        t_rm = time.time()
        if verbose:
            print("  deleted both probe files")

        def both_gone(_c, st, name=parent):
            t = st.get("target") or {}
            if is_miss(_c, st):
                return True
            return (t.get("name") or "") != name

        ok_p, c_p, st_p, rec_p, ms_p = poll_until(
            both_gone, base, project, parent, scope, wait
        )
        ok_c, c_c, st_c, rec_c, ms_c = poll_until(
            lambda c, st: both_gone(c, st, callee),
            base,
            project,
            callee,
            scope,
            wait,
        )
        gone_s = time.time() - t_rm
        still_p = (st_p.get("target") or {}).get("name") == parent and not is_miss(
            c_p, st_p
        )
        still_c = (st_c.get("target") or {}).get("name") == callee and not is_miss(
            c_c, st_c
        )
        stats["steps"].append(
            {
                "step": "multi_remove",
                "parent_gone": ok_p and not still_p,
                "callee_gone": ok_c and not still_c,
                "wall_s": gone_s,
            }
        )
        if still_p and (rec_p.get("confidence") or "") == "high":
            fail(
                "M4_stale_parent",
                "spectacular",
                f"after file delete still high ★ parent {parent}",
                ms_p,
            )
        elif still_p:
            fail(
                "M4_stale_parent",
                "soft",
                f"after delete still ★ parent {parent}",
                ms_p,
            )
        if still_c and (rec_c.get("confidence") or "") == "high":
            fail(
                "M4_stale_callee",
                "spectacular",
                f"after file delete still high ★ callee {callee}",
                ms_c,
            )
        elif still_c:
            fail(
                "M4_stale_callee",
                "soft",
                f"after delete still ★ callee {callee}",
                ms_c,
            )
        if verbose:
            print(
                f"  M4 remove parent_gone={not still_p} callee_gone={not still_c} "
                f"wall={gone_s:.2f}s"
            )
    finally:
        if created or path_a.exists() or path_b.exists():
            path_a.unlink(missing_ok=True)
            path_b.unlink(missing_ok=True)
            print("  cleaned multi-file probes (finally)")


def main() -> int:
    ap = argparse.ArgumentParser(description="Butler watcher incremental dogfood")
    ap.add_argument("--base", default=DEFAULT_BASE)
    ap.add_argument("--json", default="/tmp/butler_watcher_probe.json")
    ap.add_argument(
        "--mode",
        choices=("all", "single", "multi"),
        default="all",
        help="single=lib inject; multi=cross-file; all=both (default)",
    )
    ap.add_argument(
        "--wait",
        type=float,
        default=12.0,
        help="Seconds to wait for watcher re-edge after edit (default 12)",
    )
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    fails: list[Fail] = []
    stats: dict[str, Any] = {"project": str(WC), "mode": args.mode, "steps": []}

    def fail(inv: str, sev: str, detail: str, ms: float = 0.0, **ev):
        fails.append(Fail(inv=inv, severity=sev, detail=detail, ms=ms, evidence=ev))

    if not LIB.is_file():
        print(f"missing fixture {LIB}")
        return 2

    host = os.environ.get("BUTLER_HOST_MOUNT", str(Path.home() / "projects"))
    cont = os.environ.get("BUTLER_CONTAINER_MOUNT", "/projects")
    cont_key = "/projects/test_repos/pyo3/examples/word-count"
    if str(WC).startswith(host):
        cont_key = cont + str(WC)[len(host) :]

    print(f"Butler watcher probe  base={args.base}  mode={args.mode}")
    print(f"  root={WC}")

    warm(args.base, WC)
    if not wait_ready(args.base, cont_key, 90):
        print("  WARN: word-count not Complete yet — continuing")
    else:
        print("  warm Complete")

    if args.mode in ("all", "single"):
        run_single(args.base, args.wait, args.verbose, fail, stats)
    if args.mode in ("all", "multi"):
        run_multi(args.base, args.wait, args.verbose, fail, stats)

    print("\n" + "=" * 64)
    print("WATCHER INCREMENTAL PROBE REPORT")
    print("=" * 64)
    spec = sum(1 for f in fails if f.severity == "spectacular")
    soft = sum(1 for f in fails if f.severity == "soft")
    for f in fails:
        print(f"  [{f.severity}] {f.inv}  {f.ms:.0f}ms")
        print(f"    {f.detail}")
    print(f"\nTOTAL spectacular={spec} soft={soft}")
    report = {
        "base": args.base,
        "project": str(WC),
        "mode": args.mode,
        "stats": stats,
        "fails": [asdict(f) for f in fails],
        "summary": {"spectacular": spec, "soft": soft},
    }
    Path(args.json).write_text(json.dumps(report, indent=2))
    print(f"Wrote {args.json}")
    return 1 if spec else 0


if __name__ == "__main__":
    sys.exit(main())
