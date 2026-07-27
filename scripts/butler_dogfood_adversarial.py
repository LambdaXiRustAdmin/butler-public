#!/usr/bin/env python3
"""Adversarial dogfood: try to make Butler fail (supported languages).

Suites:
  gold     — dual-stack + stack monorepos already in hole probes (fast)
  extended — Gem-style other repos (gin, bevy, click, django, vite, arrow, …)
  all      — gold + extended (default)

Other repos *do* present different failure modes (scale, traits, re-exports,
mega-homonyms, node_modules, C++ APIs). Warming is fine — not a product gate.

Spectacular = high|complete + invariant broken, or hot-path hang cliff.
Soft = incomplete / honest limit / expected miss.

Usage:
  python3 scripts/butler_dogfood_adversarial.py
  python3 scripts/butler_dogfood_adversarial.py --suite extended -v
  python3 scripts/butler_dogfood_adversarial.py --no-warm   # assume already warm
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
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Callable, Optional

DEFAULT_BASE = os.environ.get("BUTLER_URL", "http://127.0.0.1:8002").rstrip("/")

# Host projects root (override with BUTLER_HOST_MOUNT)
HOST_ROOT = Path(os.environ.get("BUTLER_HOST_MOUNT", str(Path.home() / "projects")))

def _hp(rel: str) -> str:
    """Absolute path under HOST_ROOT (or container-friendly host mount)."""
    return str(HOST_ROOT / rel)


# --- Gold (existing dual-stack / stack) ---
WC = _hp("test_repos/pyo3/examples/word-count")
PYBIND = _hp("test_repos/pybind11")
TAURI_API = _hp("test_repos/tauri/examples/api")
FASTAPI = _hp("test_repos/fastapi-ts")
FD = _hp("test_repos/fd")
BAT = _hp("test_repos/bat")
BUTLER = _hp("lambda-wisperer")
EVE = _hp("lambda-eve")
XI = _hp("Lambda-xi-rust")

# --- Extended (other repos — different failure surfaces) ---
GIN = _hp("test_repos/gin")  # Go
BEVY = _hp("test_repos/bevy")  # Rust mega-homonym / traits
CLICK = _hp("test_repos/click")  # Python decorators
DJANGO = _hp("test_repos/django")  # Python re-export
VITE = _hp("test_repos/vite")  # node_modules / TS scale
ARROW = _hp("test_repos/arrow")  # C++ / multi-lang
PYO3 = _hp("test_repos/pyo3")  # Rust+Py package root
TAURI = _hp("test_repos/tauri")  # full monorepo
CPYTHON = _hp("test_repos/cpython")  # dense hub / scale

GOLD_ROOTS = [WC, PYBIND, TAURI_API, FASTAPI, FD, BAT, BUTLER, EVE, XI]
EXTENDED_ROOTS = [GIN, BEVY, CLICK, DJANGO, VITE, ARROW, PYO3, TAURI, CPYTHON]

# Hot ghost miss should be fast once warehouse is Complete.
GHOST_MS_SPECTACULAR = 500.0
GHOST_MS_SOFT = 150.0
# Dense hub payload (content+structured JSON) blowout.
DENSE_PAYLOAD_SOFT_BYTES = 400_000
DENSE_PAYLOAD_SPEC_BYTES = 1_500_000
DENSE_MS_SOFT = 15_000.0


@dataclass
class Fail:
    attack: str
    severity: str  # spectacular | soft
    project: str
    detail: str
    receipt: str = ""
    ms: float = 0.0
    evidence: dict[str, Any] = field(default_factory=dict)


def get_json(url: str, timeout: float = 30.0) -> dict[str, Any]:
    with urllib.request.urlopen(url, timeout=timeout) as resp:
        return json.loads(resp.read().decode())


def post_json(url: str, payload: dict[str, Any], timeout: float = 120.0) -> tuple[dict, float, Optional[str]]:
    data = json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            body = resp.read().decode()
            ms = (time.perf_counter() - t0) * 1000
            try:
                return json.loads(body), ms, None
            except json.JSONDecodeError:
                return {"content": body}, ms, None
    except Exception as e:
        return {}, (time.perf_counter() - t0) * 1000, str(e)


def post_ok(
    base: str, payload: dict[str, Any], timeout: float = 120.0
) -> tuple[dict, str, dict, float, Optional[str]]:
    d, ms, err = post_json(f"{base}/context", payload, timeout=timeout)
    if err:
        return {}, "", {}, ms, err
    c = d.get("content")
    if isinstance(c, list) and c:
        content = (c[0] or {}).get("text", "") or ""
    elif isinstance(c, str):
        content = c
    else:
        content = ""
    st = d.get("structured") or d.get("structuredContent") or {}
    if not isinstance(st, dict):
        st = {}
    return d, content, st, ms, None


def receipt_bits(st: dict, content: str) -> dict[str, str]:
    rec = st.get("receipt") if isinstance(st.get("receipt"), dict) else {}
    if not rec and isinstance(st.get("telemetry"), dict):
        tr = st["telemetry"].get("receipt")
        if isinstance(tr, dict):
            rec = tr
    conf = (rec.get("confidence") or "").lower()
    basis = (rec.get("basis") or "").lower()
    edges = (rec.get("edges") or "").lower()
    ladder = (rec.get("ladder") or "").lower()
    if not conf and "receipt:" in content.lower():
        m = re.search(r"receipt:\s*(\w+)\s*\|\s*([^|]+)\s*\|\s*(\w+)", content, re.I)
        if m:
            conf, basis, edges = m.group(1).lower(), m.group(2).strip().lower(), m.group(3).lower()
    return {"confidence": conf, "basis": basis, "edges": edges, "ladder": ladder}


def is_spectacular(rec: dict) -> bool:
    return rec.get("confidence") == "high" and rec.get("edges") in ("complete", "full", "")


def rec_s(rec: dict) -> str:
    return f"{rec.get('confidence')}|{rec.get('basis')}|{rec.get('edges')}"


def hop1(rows: list) -> list:
    out = []
    for r in rows or []:
        if not isinstance(r, dict):
            continue
        h = r.get("hop")
        if h is None or int(h) <= 1:
            out.append(r)
    return out


def names(rows: list) -> set[str]:
    return {r.get("name") for r in rows if r.get("name")}


def bridges_of(st: dict) -> list[dict]:
    out = []
    for key in ("bridge_callers", "bridge_callees"):
        for b in st.get(key) or []:
            if isinstance(b, dict):
                out.append(b)
    return out


def payload_bytes(d: dict, content: str) -> int:
    try:
        return len(json.dumps(d, default=str).encode()) + len(content.encode())
    except Exception:
        return len(content.encode())


def is_building(content: str, st: dict) -> bool:
    if content.startswith("=== Building") or "BUILDING" in content[:200]:
        return True
    status = (st.get("state") or {}) if isinstance(st.get("state"), dict) else {}
    # various shapes
    return "building" in content[:300].lower() and "retry" in content[:500].lower()


def attack_trace(
    base: str,
    project: str,
    symbol: str,
    scope: Optional[list[str]] = None,
    goal: str = "TraceBlastRadius",
    detail: str = "compact",
    timeout: float = 120.0,
    confirm_long_wait: bool = False,
) -> tuple[str, dict, dict, float, Optional[str], dict]:
    payload: dict[str, Any] = {
        "project": project,
        "target_symbol": symbol,
        "goal": goal,
        "detail": detail,
    }
    if scope:
        payload["scope_paths"] = scope
    if confirm_long_wait:
        payload["confirm_long_wait"] = True
    d, content, st, ms, err = post_ok(base, payload, timeout=timeout)
    # One retry on BUILDING (hydrate race)
    if not err and is_building(content, st):
        time.sleep(2.0)
        d, content, st, ms2, err = post_ok(base, payload, timeout=timeout)
        ms += ms2
    return content, st, receipt_bits(st, content), ms, err, d


def attack_arch(
    base: str,
    project: str,
    scope: Optional[list[str]] = None,
    detail: str = "compact",
    timeout: float = 180.0,
) -> tuple[str, dict, dict, float, Optional[str], dict]:
    payload: dict[str, Any] = {
        "project": project,
        "goal": "ArchitecturalSummary",
        "detail": detail,
    }
    if scope:
        payload["scope_paths"] = scope
    d, content, st, ms, err = post_ok(base, payload, timeout=timeout)
    if not err and is_building(content, st):
        time.sleep(2.0)
        d, content, st, ms2, err = post_ok(base, payload, timeout=timeout)
        ms += ms2
    return content, st, receipt_bits(st, content), ms, err, d


def host_to_container(path: str) -> str:
    host = os.environ.get("BUTLER_HOST_MOUNT", str(Path.home() / "projects"))
    cont = os.environ.get("BUTLER_CONTAINER_MOUNT", "/projects")
    p = path.rstrip("/")
    if p.startswith(host + "/") or p == host:
        return cont + p[len(host) :]
    return p


def warm_roots(base: str, roots: list[str], wait_s: float = 900.0, verbose: bool = False) -> list[str]:
    """POST /warm and wait until edges_complete for each existing root."""
    existing = [r for r in roots if Path(r).is_dir()]
    missing = [r for r in roots if not Path(r).is_dir()]
    if missing and verbose:
        for m in missing:
            print(f"  skip missing root: {m}")
    if not existing:
        return []
    print(f"Warming {len(existing)} root(s)…")
    d, ms, err = post_json(f"{base}/warm", {"roots": existing}, timeout=60)
    if err:
        print(f"  warm request failed: {err}")
    elif verbose:
        print(f"  warm ack {ms:.0f}ms: {d.get('message') or d.get('ok')}")

    want = {host_to_container(r) for r in existing}
    deadline = time.time() + wait_s
    last = 0
    while time.time() < deadline:
        try:
            health = get_json(f"{base}/mcp/health", timeout=15)
        except Exception as e:
            if verbose:
                print(f"  health err: {e}")
            time.sleep(3)
            continue
        loaded = health.get("loaded") or {}
        ready = 0
        for k in want:
            v = loaded.get(k) or {}
            if v.get("ready") and v.get("edges_complete"):
                ready += 1
        if ready != last or verbose:
            print(f"  complete {ready}/{len(want)}")
            last = ready
        if ready >= len(want):
            print("  ALL_WARM")
            return existing
        time.sleep(4)
    print(f"  WARN: warm timeout; complete {last}/{len(want)} — continuing")
    return existing


def existing_roots(roots: list[str]) -> list[str]:
    return [r for r in roots if Path(r).is_dir()]


def main() -> int:
    ap = argparse.ArgumentParser(description="Adversarial dogfood (supported langs + other repos)")
    ap.add_argument("--base", default=DEFAULT_BASE)
    ap.add_argument(
        "--suite",
        choices=("gold", "extended", "all"),
        default="all",
        help="gold=dual-stack/stack; extended=other repos; all=both (default)",
    )
    ap.add_argument("--json", default="/tmp/butler_dogfood_adversarial.json")
    ap.add_argument("--no-warm", action="store_true", help="Skip /warm (assume ready)")
    ap.add_argument("--warm-wait", type=float, default=900.0, help="Seconds to wait for Complete")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    fails: list[Fail] = []
    attacks = 0
    stats: list[dict] = []

    def record(
        attack: str,
        sev: str,
        project: str,
        detail: str,
        rec: Optional[dict] = None,
        ms: float = 0.0,
        **ev,
    ):
        fails.append(
            Fail(
                attack=attack,
                severity=sev,
                project=project,
                detail=detail,
                receipt=rec_s(rec or {}),
                ms=ms,
                evidence=ev,
            )
        )

    def run(name: str, project: str, fn: Callable[[], None]):
        nonlocal attacks
        attacks += 1
        t0 = time.perf_counter()
        try:
            fn()
            ok = True
        except Exception as e:
            record(name, "spectacular", project, f"attack harness error: {e}")
            ok = False
        ms = (time.perf_counter() - t0) * 1000
        stats.append({"attack": name, "project": project, "ms": ms, "ok": ok})
        if args.verbose:
            print(f"  [{name}] {ms:.0f}ms ok={ok}")

    # Roots for this suite
    roots: list[str] = []
    if args.suite in ("gold", "all"):
        roots.extend(GOLD_ROOTS)
    if args.suite in ("extended", "all"):
        roots.extend(EXTENDED_ROOTS)
    # de-dupe preserve order
    seen: set[str] = set()
    deduped: list[str] = []
    for r in roots:
        if r not in seen:
            seen.add(r)
            deduped.append(r)
    roots = existing_roots(deduped)

    print(f"Butler adversarial dogfood  base={args.base}  suite={args.suite}")
    print("Langs: Rust · Python · TS/JS · C++ · Go · Svelte  |  no void langs (Ruby…)")
    print(f"Roots ({len(roots)}): " + ", ".join(Path(r).name for r in roots))
    print()

    if not args.no_warm:
        warm_roots(args.base, roots, wait_s=args.warm_wait, verbose=args.verbose)
    else:
        print("Skipping warm (--no-warm)\n")

    # =========================================================================
    # GOLD attacks (A1–A12) — dual-stack / stack monorepos
    # =========================================================================
    if args.suite in ("gold", "all"):

        def a1():
            c, st, rec, ms, err, _ = attack_trace(
                args.base, BUTLER, "open", ["cli/src/harvester/mcp_api.rs"]
            )
            t = st.get("target") or {}
            tf = (t.get("file") or "").replace("\\", "/")
            if t and "session.rs" in tf and is_spectacular(rec):
                record(
                    "A1_wrong_file_pin",
                    "spectacular",
                    BUTLER,
                    f"pin mcp_api ★ session.rs {rec_s(rec)}",
                    rec,
                    ms,
                    star=tf,
                )
            if args.verbose:
                print(f"    A1 open@mcp_api → ★={tf or 'MISS'} {rec_s(rec)} {ms:.0f}ms")

        run("A1_wrong_file_pin", BUTLER, a1)

        def a2():
            for sym in ("echo", "spam"):
                c, st, rec, ms, err, _ = attack_trace(args.base, TAURI_API, sym)
                domain = (st.get("blast_domain") or "").lower()
                if domain != "disambiguate" and not c.startswith("Disambiguate"):
                    if is_spectacular(rec) and (st.get("target") or {}):
                        record(
                            "A2_dual_lang_no_disambiguate",
                            "spectacular",
                            TAURI_API,
                            f"{sym}: no disambiguate ★ {(st.get('target') or {}).get('file')}",
                            rec,
                            ms,
                        )
                if args.verbose:
                    print(f"    A2 {sym} domain={domain or c[:40]!r} {ms:.0f}ms")

        run("A2_dual_lang_disambiguate", TAURI_API, a2)

        def a3():
            for proj, seed in ((FD, "main"), (BAT, "main"), (FASTAPI, "main")):
                c, st, rec, ms, err, _ = attack_trace(args.base, proj, seed)
                typed = [
                    b
                    for b in bridges_of(st)
                    if (b.get("relation") or "").lower() in ("export", "ipc", "twin", "ffi")
                ]
                if typed and is_spectacular(rec):
                    record(
                        "A3_false_ffi_invent",
                        "spectacular",
                        proj,
                        f"{seed}: invented {[b.get('name') for b in typed[:5]]}",
                        rec,
                        ms,
                    )
                if args.verbose:
                    print(f"    A3 {Path(proj).name}/{seed} br={len(typed)} {rec_s(rec)}")

        run("A3_false_ffi_invent", FD, a3)

        def a4():
            gold = [
                (WC, "search_py", "search", "export", None),
                (WC, "search", "search_py", "export", None),
                (TAURI_API, "log", "log_operation", "ipc", ["src/views/Communication.svelte"]),
                (TAURI_API, "log_operation", "log", "ipc", ["src-tauri/src/cmd.rs"]),
                (PYBIND, "return_bytes", "test_bytes", "export", None),
            ]
            for proj, seed, peer, rel, scope in gold:
                c, st, rec, ms, err, _ = attack_trace(args.base, proj, seed, scope)
                if err or c.startswith("Disambiguate"):
                    continue
                br_names = {b.get("name") for b in bridges_of(st) if b.get("name")}
                if peer not in br_names:
                    sev = "spectacular" if is_spectacular(rec) else "soft"
                    record(
                        "A4_gold_bridge_missing",
                        sev,
                        proj,
                        f"{seed}: expected {rel} peer {peer!r} got {sorted(br_names)[:8]}",
                        rec,
                        ms,
                    )
                if args.verbose:
                    print(f"    A4 {seed}→{peer} br={sorted(br_names)[:6]} {rec_s(rec)}")

        run("A4_gold_bridges", WC, a4)

        def a5():
            c, st, rec, ms, err, _ = attack_trace(args.base, WC, "search_sequential")
            br = bridges_of(st)
            if br and is_spectacular(rec):
                record(
                    "A5_silent_export_invent",
                    "spectacular",
                    WC,
                    f"search_sequential invented {[b.get('name') for b in br]}",
                    rec,
                    ms,
                )

        run("A5_silent_export", WC, a5)

        def a6():
            bogus = "ZzNoSuchButlerSymbol_9f3a2"
            for proj in (BUTLER, WC, TAURI_API, FD):
                c, st, rec, ms, err, _ = attack_trace(args.base, proj, bogus)
                t = st.get("target") or {}
                if t and is_spectacular(rec):
                    record(
                        "A6_ghost_symbol_star",
                        "spectacular",
                        proj,
                        f"ghost got ★ {t.get('file')}",
                        rec,
                        ms,
                    )
                # latency cliff on hot miss
                if ms > GHOST_MS_SPECTACULAR and not is_building(c, st):
                    record(
                        "A6_ghost_symbol_hang",
                        "spectacular",
                        proj,
                        f"ghost miss took {ms:.0f}ms (>{GHOST_MS_SPECTACULAR:.0f})",
                        rec,
                        ms,
                    )
                elif ms > GHOST_MS_SOFT and not is_building(c, st):
                    record(
                        "A6_ghost_symbol_hang",
                        "soft",
                        proj,
                        f"ghost miss took {ms:.0f}ms (>{GHOST_MS_SOFT:.0f})",
                        rec,
                        ms,
                    )
                if args.verbose:
                    print(f"    A6 {Path(proj).name} ghost target={bool(t)} {ms:.0f}ms {rec_s(rec)}")

        run("A6_ghost_symbol", BUTLER, a6)

        def a7():
            c, st, rec, ms, err, _ = attack_trace(
                args.base, BAT, "config", ["src/bin/bat/config.rs"]
            )
            t = st.get("target") or {}
            tf = (t.get("file") or "").replace("\\", "/")
            if t and "app.rs" in tf and is_spectacular(rec):
                record(
                    "A7_scope_lift_out",
                    "spectacular",
                    BAT,
                    f"pin config.rs ★ app.rs {rec_s(rec)}",
                    rec,
                    ms,
                )

        run("A7_scope_lift_out", BAT, a7)

        def a8():
            pairs = [
                (BUTLER, "run_http_proxy", "get", ["cli/src/server/query_cache.rs"]),
                (EVE, "build_flat_plan", "node_count", ["src/gnn/graph.rs"]),
            ]
            for proj, parent, child, pin in pairs:
                c1, st1, rec1, ms1, err1, _ = attack_trace(args.base, proj, parent)
                if err1:
                    continue
                callees = hop1(st1.get("callees") or [])
                if not any(x.get("name") == child for x in callees):
                    continue
                c2, st2, rec2, ms2, err2, _ = attack_trace(args.base, proj, child, pin)
                if err2 or not (st2.get("target") or {}):
                    continue
                cnames = names(st2.get("callers") or [])
                tel = st2.get("telemetry") if isinstance(st2.get("telemetry"), dict) else {}
                omitted = int(tel.get("callers_omitted") or 0)
                try:
                    in_i = int(tel.get("seed_in_degree") or 0)
                except (TypeError, ValueError):
                    in_i = 0
                if parent in cnames:
                    if args.verbose:
                        print(f"    A8 {parent}→{child} reverse OK")
                    continue
                if omitted > 0 or in_i > 12 or len(st2.get("callers") or []) >= 10:
                    record(
                        "A8_reverse_pack_omit",
                        "soft",
                        proj,
                        f"{parent}→{child} pack_omit in={in_i} om={omitted}",
                        rec2,
                        ms2,
                    )
                elif is_spectacular(rec2):
                    record(
                        "A8_reverse_missing",
                        "spectacular",
                        proj,
                        f"{parent}→{child} reverse missing high|complete",
                        rec2,
                        ms2,
                    )

        run("A8_reverse_call", BUTLER, a8)

        def a9():
            for proj, needle in ((WC, "search_py"), (TAURI_API, "performRequest")):
                content, st, rec, ms, err, _ = attack_arch(args.base, proj)
                if err:
                    record("A9_arch_interconnect", "soft", proj, f"err {err}", ms=ms)
                    continue
                has_line = "interconnect bridge" in content.lower()
                br = st.get("bridges") or []
                if not has_line and not br:
                    record(
                        "A9_arch_interconnect",
                        "soft",
                        proj,
                        "Arch compact has no interconnect bridges",
                        ms=ms,
                        content_head=content[:200],
                    )
                if args.verbose:
                    print(f"    A9 {Path(proj).name} interconnect={has_line or bool(br)} {ms:.0f}ms")

        run("A9_arch_interconnect", WC, a9)

        def a10():
            c, st, rec, ms, err, _ = attack_trace(
                args.base, BUTLER, "arch_map_hub_path_soft_delta"
            )
            spine = st.get("caller_path") or []
            callers = st.get("callers") or []
            if spine:
                s0 = (spine[0] or {}).get("name")
                cnames = names(callers)
                if s0 and s0 not in cnames and callers:
                    sev = "spectacular" if is_spectacular(rec) else "soft"
                    record(
                        "A10_spine_not_in_callers",
                        sev,
                        BUTLER,
                        f"spine[0]={s0} not in {sorted(cnames)[:10]}",
                        rec,
                        ms,
                    )
                if args.verbose:
                    print(f"    A10 spine0={s0} in={s0 in cnames if s0 else None}")

        run("A10_spine", BUTLER, a10)

        def a11():
            c, st, rec, ms, err, _ = attack_trace(
                args.base,
                BUTLER,
                "open",
                ["this/path/does/not/exist_anywhere.rs"],
            )
            t = st.get("target") or {}
            domain = (st.get("blast_domain") or "").lower()
            if t and is_spectacular(rec) and domain not in ("scope_not_found",):
                record(
                    "A11_junk_scope",
                    "spectacular",
                    BUTLER,
                    f"junk scope still ★ {t.get('file')} domain={domain}",
                    rec,
                    ms,
                )
            if args.verbose:
                print(f"    A11 junk domain={domain} target={bool(t)} {ms:.0f}ms")

        run("A11_junk_scope", BUTLER, a11)

        def a12():
            for proj, sym in ((WC, "search"), (TAURI_API, "log_operation"), (FD, "main")):
                c, st, rec, ms, err, _ = attack_trace(args.base, proj, sym)
                kind = (
                    st.get("seed_kind") or (st.get("target") or {}).get("kind") or ""
                ).lower()
                if "call_expression" in kind and is_spectacular(rec):
                    record(
                        "A12_call_expression_star",
                        "spectacular",
                        proj,
                        f"{sym} ★ kind={kind}",
                        rec,
                        ms,
                    )
                if args.verbose:
                    print(f"    A12 {sym} kind={kind or '?'} {rec_s(rec)}")

        run("A12_call_expression", WC, a12)

    # =========================================================================
    # EXTENDED — other repos (Gem vectors, Butler-native assertions)
    # =========================================================================
    if args.suite in ("extended", "all"):

        def b1_ghost_latency():
            """O(N) furnace: ghost on foreign repos must be fast once warm."""
            bogus = "i_do_not_exist_anywhere_ever_123"
            for proj in existing_roots([GIN, BEVY, CLICK, FD, VITE]):
                c, st, rec, ms, err, _ = attack_trace(args.base, proj, bogus)
                t = st.get("target") or {}
                if t and is_spectacular(rec):
                    record(
                        "B1_ghost_false_star",
                        "spectacular",
                        proj,
                        f"ghost ★ {t.get('file')}",
                        rec,
                        ms,
                    )
                if is_building(c, st):
                    if args.verbose:
                        print(f"    B1 {Path(proj).name} still BUILDING — skip latency")
                    continue
                if ms > GHOST_MS_SPECTACULAR:
                    record(
                        "B1_ghost_hang",
                        "spectacular",
                        proj,
                        f"ghost took {ms:.0f}ms (>{GHOST_MS_SPECTACULAR:.0f})",
                        rec,
                        ms,
                    )
                elif ms > GHOST_MS_SOFT:
                    record(
                        "B1_ghost_hang",
                        "soft",
                        proj,
                        f"ghost took {ms:.0f}ms (>{GHOST_MS_SOFT:.0f})",
                        rec,
                        ms,
                    )
                if args.verbose:
                    print(f"    B1 {Path(proj).name} ghost {ms:.0f}ms target={bool(t)} {rec_s(rec)}")

        run("B1_ghost_latency", GIN, b1_ghost_latency)

        def b2_regex_poison():
            """Name index must not explode / invent ★ on glob-like symbols."""
            for sym in (".*", "handle_*", "Py*"):
                for proj in existing_roots([BUTLER, BEVY, GIN]):
                    c, st, rec, ms, err, _ = attack_trace(
                        args.base, proj, sym, detail="compact", timeout=60
                    )
                    t = st.get("target") or {}
                    # high|complete on a glob is a silent lie
                    if t and is_spectacular(rec) and t.get("name") not in (sym,):
                        # name may not match query — fail
                        if (t.get("name") or "") != sym:
                            record(
                                "B2_regex_poison_star",
                                "spectacular",
                                proj,
                                f"symbol={sym!r} ★ name={t.get('name')!r} file={t.get('file')}",
                                rec,
                                ms,
                            )
                    if ms > GHOST_MS_SPECTACULAR and not is_building(c, st):
                        record(
                            "B2_regex_poison_hang",
                            "spectacular",
                            proj,
                            f"symbol={sym!r} took {ms:.0f}ms",
                            rec,
                            ms,
                        )
                    if args.verbose:
                        print(
                            f"    B2 {Path(proj).name} {sym!r} {ms:.0f}ms "
                            f"target={bool(t)} {rec_s(rec)}"
                        )

        run("B2_regex_poison", BUTLER, b2_regex_poison)

        def b3_dense_hub():
            """Dense mega-hub: payload / latency bounds (not unbounded dump)."""
            cases = [
                (CPYTHON, "PyObject", ["Objects/"]),
                (BEVY, "App", ["crates/bevy_app/"]),
                (BUTLER, "CodeGraph", ["code_graph/src/"]),
            ]
            for proj, sym, scope in cases:
                if not Path(proj).is_dir():
                    continue
                c, st, rec, ms, err, d = attack_trace(
                    args.base,
                    proj,
                    sym,
                    scope,
                    detail="dense",
                    timeout=180,
                )
                if err:
                    if args.verbose:
                        print(f"    B3 {Path(proj).name}/{sym} err={err[:80]}")
                    continue
                if is_building(c, st):
                    if args.verbose:
                        print(f"    B3 {Path(proj).name}/{sym} BUILDING skip")
                    continue
                nbytes = payload_bytes(d, c)
                if nbytes >= DENSE_PAYLOAD_SPEC_BYTES:
                    record(
                        "B3_dense_payload_bomb",
                        "spectacular",
                        proj,
                        f"{sym} dense payload {nbytes} bytes (>{DENSE_PAYLOAD_SPEC_BYTES})",
                        rec,
                        ms,
                        bytes=nbytes,
                    )
                elif nbytes >= DENSE_PAYLOAD_SOFT_BYTES:
                    record(
                        "B3_dense_payload_bomb",
                        "soft",
                        proj,
                        f"{sym} dense payload {nbytes} bytes (>{DENSE_PAYLOAD_SOFT_BYTES})",
                        rec,
                        ms,
                        bytes=nbytes,
                    )
                if ms > DENSE_MS_SOFT:
                    record(
                        "B3_dense_slow",
                        "soft",
                        proj,
                        f"{sym} dense took {ms:.0f}ms",
                        rec,
                        ms,
                    )
                if args.verbose:
                    print(
                        f"    B3 {Path(proj).name}/{sym} {ms:.0f}ms "
                        f"bytes={nbytes} {rec_s(rec)} callers={len(st.get('callers') or [])}"
                    )

        run("B3_dense_hub", CPYTHON, b3_dense_hub)

        def b4_path_poison():
            """Scope normalization: ./ and ../ and absolute host paths."""
            # Relative traps on pyo3 package
            if Path(PYO3).is_dir():
                for scope in (["./src/"], ["src/../src/"], ["src/"]):
                    content, st, rec, ms, err, _ = attack_arch(
                        args.base, PYO3, scope=scope, detail="compact"
                    )
                    if err:
                        record(
                            "B4_path_poison",
                            "soft",
                            PYO3,
                            f"scope={scope} err={err}",
                            ms=ms,
                        )
                        continue
                    # Should not scope_not_found for normalized ./src
                    domain = (st.get("blast_domain") or "").lower()
                    bad = (
                        "scope not found" in content.lower()
                        or domain == "scope_not_found"
                    )
                    if bad and scope in (["./src/"], ["src/../src/"], ["src/"]):
                        # src/ should work; ./ and ../ should normalize
                        record(
                            "B4_path_poison",
                            "soft",
                            PYO3,
                            f"scope={scope} → not found (normalization?)",
                            rec,
                            ms,
                        )
                    if args.verbose:
                        print(
                            f"    B4 pyo3 scope={scope} {ms:.0f}ms "
                            f"arch={'Architectural' in content} bad={bad}"
                        )

            # Absolute host path must not silently ★/map as if relative
            if Path(BEVY).is_dir():
                abs_scope = [f"{BEVY}/crates/"]
                content, st, rec, ms, err, _ = attack_arch(
                    args.base, BEVY, scope=abs_scope, detail="compact"
                )
                t = st.get("target")
                # Arch has no target; look for scope_not_found or successful relative heal
                snf = "scope not found" in content.lower() or "scope_not_found" in content.lower()
                healed = "Architectural summary" in content or "skeleton" in content.lower()
                if args.verbose:
                    print(
                        f"    B4 bevy abs_scope snf={snf} healed={healed} {ms:.0f}ms"
                    )
                # Spectacular only if we get a confident false map with host path still baked wrong
                # Soft: either honest snf or successful heal is OK
                if not snf and not healed and not is_building(content, st):
                    record(
                        "B4_abs_scope",
                        "soft",
                        BEVY,
                        f"abs scope neither snf nor arch: {content[:120]!r}",
                        rec,
                        ms,
                    )

        run("B4_path_poison", PYO3, b4_path_poison)

        def b5_node_modules():
            """node_modules Arch: refuse / rollup honesty — not 100k-file dump hang forever."""
            if not Path(VITE).is_dir():
                return
            content, st, rec, ms, err, d = attack_arch(
                args.base,
                VITE,
                scope=["node_modules/"],
                detail="compact",
                timeout=180,
            )
            nbytes = payload_bytes(d, content)
            if err:
                if args.verbose:
                    print(f"    B5 vite node_modules err={err[:100]}")
                return
            if is_building(content, st):
                if args.verbose:
                    print(f"    B5 BUILDING {ms:.0f}ms")
                return
            # Soft if huge payload or multi-minute
            if nbytes > DENSE_PAYLOAD_SOFT_BYTES:
                record(
                    "B5_node_modules_bloat",
                    "soft",
                    VITE,
                    f"node_modules arch payload {nbytes} bytes in {ms:.0f}ms",
                    rec,
                    ms,
                    bytes=nbytes,
                )
            if ms > 60_000:
                record(
                    "B5_node_modules_hang",
                    "spectacular",
                    VITE,
                    f"node_modules arch took {ms:.0f}ms",
                    rec,
                    ms,
                )
            elif ms > 20_000:
                record(
                    "B5_node_modules_hang",
                    "soft",
                    VITE,
                    f"node_modules arch took {ms:.0f}ms",
                    rec,
                    ms,
                )
            if args.verbose:
                print(
                    f"    B5 vite node_modules {ms:.0f}ms bytes={nbytes} "
                    f"head={content[:100]!r}"
                )

        run("B5_node_modules", VITE, b5_node_modules)

        def b6_homonym_frankenstein():
            """Mega-homonym must disambiguate — not merge all `new`/`App` into one ★."""
            cases = [
                (BEVY, "new", None),
                (BEVY, "App", None),
                (BEVY, "app", None),
                (CLICK, "command", None),
                (DJANGO, "Model", ["django/db/models/"]),
            ]
            for proj, sym, scope in cases:
                if not Path(proj).is_dir():
                    continue
                c, st, rec, ms, err, _ = attack_trace(
                    args.base, proj, sym, scope, timeout=120
                )
                domain = (st.get("blast_domain") or "").lower()
                locs = st.get("locations") or []
                t = st.get("target") or {}
                dis = domain == "disambiguate" or c.startswith("Disambiguate")
                # Frankenstein: high|complete single ★ when many serious alts and no pin
                if (
                    not dis
                    and is_spectacular(rec)
                    and t
                    and not scope
                    and len(locs) >= 5
                    and sym.lower() in ("new", "app", "command", "model")
                ):
                    # short danger names with many locations should disambiguate
                    record(
                        "B6_frankenstein_homonym",
                        "spectacular",
                        proj,
                        f"{sym}: high|complete ★ without disambiguate "
                        f"({len(locs)} locations) file={t.get('file')}",
                        rec,
                        ms,
                        n_locs=len(locs),
                    )
                if args.verbose:
                    print(
                        f"    B6 {Path(proj).name}/{sym} dis={dis} "
                        f"locs={len(locs)} ★={bool(t)} {ms:.0f}ms {rec_s(rec)}"
                    )

        run("B6_homonym_frankenstein", BEVY, b6_homonym_frankenstein)

        def b7_macro_trait_limits():
            """Macros / traits: honest miss or type neighborhood — not fake CALL invent."""
            cases = [
                (BUTLER, "Serialize", None),  # derive ghost often
                (BEVY, "Plugin", ["crates/bevy_app/"]),
            ]
            for proj, sym, scope in cases:
                if not Path(proj).is_dir():
                    continue
                c, st, rec, ms, err, _ = attack_trace(args.base, proj, sym, scope)
                t = st.get("target") or {}
                domain = (st.get("blast_domain") or "").lower()
                # If we ★ a random unrelated function with high|complete — fail
                if t and is_spectacular(rec):
                    name = (t.get("name") or "")
                    if name and name != sym and not name.endswith(sym):
                        record(
                            "B7_macro_wrong_star",
                            "spectacular",
                            proj,
                            f"{sym} ★ wrong name={name} file={t.get('file')}",
                            rec,
                            ms,
                        )
                if args.verbose:
                    print(
                        f"    B7 {Path(proj).name}/{sym} domain={domain} "
                        f"★={ (t.get('name') if t else None) } {ms:.0f}ms {rec_s(rec)}"
                    )

        run("B7_macro_trait", BEVY, b7_macro_trait_limits)

        def b8_python_reexport():
            """Django Model / click command — disambiguate or real def, not call_expression ★."""
            if Path(DJANGO).is_dir():
                c, st, rec, ms, err, _ = attack_trace(
                    args.base,
                    DJANGO,
                    "Model",
                    ["django/db/models/"],
                    timeout=120,
                )
                kind = (
                    st.get("seed_kind") or (st.get("target") or {}).get("kind") or ""
                ).lower()
                if "call_expression" in kind and is_spectacular(rec):
                    record(
                        "B8_python_call_star",
                        "spectacular",
                        DJANGO,
                        f"Model ★ call_expression {rec_s(rec)}",
                        rec,
                        ms,
                    )
                if args.verbose:
                    t = st.get("target") or {}
                    print(
                        f"    B8 django Model kind={kind} "
                        f"file={ (t.get('file') or '')[-50:] } {ms:.0f}ms"
                    )
            if Path(CLICK).is_dir():
                c, st, rec, ms, err, _ = attack_trace(args.base, CLICK, "command")
                domain = (st.get("blast_domain") or "").lower()
                kind = (
                    st.get("seed_kind") or (st.get("target") or {}).get("kind") or ""
                ).lower()
                if "call_expression" in kind and is_spectacular(rec):
                    record(
                        "B8_python_call_star",
                        "spectacular",
                        CLICK,
                        f"command ★ call_expression",
                        rec,
                        ms,
                    )
                if args.verbose:
                    print(
                        f"    B8 click command domain={domain} kind={kind} "
                        f"{ms:.0f}ms {rec_s(rec)}"
                    )

        run("B8_python_reexport", DJANGO, b8_python_reexport)

        def b9_cpp_arrow():
            """C++ API symbol — miss or type neighborhood OK; hang / wrong invent not OK."""
            if not Path(ARROW).is_dir():
                return
            c, st, rec, ms, err, _ = attack_trace(
                args.base, ARROW, "RecordBatch", timeout=180
            )
            t = st.get("target") or {}
            if is_building(c, st):
                if args.verbose:
                    print(f"    B9 arrow BUILDING {ms:.0f}ms")
                return
            if ms > 30_000:
                record(
                    "B9_arrow_slow",
                    "soft",
                    ARROW,
                    f"RecordBatch took {ms:.0f}ms",
                    rec,
                    ms,
                )
            if t and is_spectacular(rec):
                # name should match
                if (t.get("name") or "") not in ("RecordBatch", "record_batch"):
                    if "RecordBatch" not in (t.get("name") or ""):
                        record(
                            "B9_arrow_wrong_star",
                            "spectacular",
                            ARROW,
                            f"RecordBatch ★ {t.get('name')} @ {t.get('file')}",
                            rec,
                            ms,
                        )
            if args.verbose:
                print(
                    f"    B9 arrow RecordBatch ★={t.get('name') if t else None} "
                    f"{ms:.0f}ms {rec_s(rec)} domain={st.get('blast_domain')}"
                )

        run("B9_arrow_cpp", ARROW, b9_cpp_arrow)

        def b10_tauri_ipc_wall():
            """Full tauri monorepo: short IPC names should disambiguate or bridge, not invent."""
            if not Path(TAURI).is_dir():
                return
            for sym in ("invoke", "command", "Plugin"):
                c, st, rec, ms, err, _ = attack_trace(
                    args.base, TAURI, sym, timeout=120
                )
                domain = (st.get("blast_domain") or "").lower()
                t = st.get("target") or {}
                dis = domain == "disambiguate" or c.startswith("Disambiguate")
                if (
                    not dis
                    and is_spectacular(rec)
                    and t
                    and len(st.get("locations") or []) >= 8
                ):
                    record(
                        "B10_tauri_frankenstein",
                        "spectacular",
                        TAURI,
                        f"{sym}: high|complete with {len(st.get('locations') or [])} locs no disambiguate",
                        rec,
                        ms,
                    )
                if args.verbose:
                    print(
                        f"    B10 tauri/{sym} dis={dis} locs={len(st.get('locations') or [])} "
                        f"{ms:.0f}ms {rec_s(rec)}"
                    )

        run("B10_tauri_monorepo", TAURI, b10_tauri_ipc_wall)

        def b11_go_gin():
            """Go gin: real symbols OK; ghost hang already B1; Context mega-homonym."""
            if not Path(GIN).is_dir():
                return
            c, st, rec, ms, err, _ = attack_trace(args.base, GIN, "Context")
            domain = (st.get("blast_domain") or "").lower()
            dis = domain == "disambiguate" or c.startswith("Disambiguate")
            locs = st.get("locations") or []
            t = st.get("target") or {}
            if not dis and is_spectacular(rec) and t and len(locs) >= 5:
                record(
                    "B11_gin_frankenstein",
                    "spectacular",
                    GIN,
                    f"Context high|complete without disambiguate locs={len(locs)}",
                    rec,
                    ms,
                )
            if args.verbose:
                print(
                    f"    B11 gin Context dis={dis} locs={len(locs)} "
                    f"★={t.get('name') if t else None} {ms:.0f}ms {rec_s(rec)}"
                )

        run("B11_go_gin", GIN, b11_go_gin)

        def b12_callback_chasm():
            """0 CALL callers must not read as dead code (callback / decorator / map).

            Not inventing extractors — product honesty only. Spectacular if content
            implies delete-safe dead code when warehouse CALL fan-in is 0.
            """
            cases = [
                # FastAPI route handler — often decorator-registered, weak CALL reverse
                (
                    FASTAPI,
                    "read_items",
                    ["backend/app/api/routes/"],
                ),
                (
                    FASTAPI,
                    "health_check",
                    ["backend/app/api/routes/"],
                ),
                # Vite: named fn passed to .map(buildOutputOptions) — callback shape
                (
                    VITE,
                    "buildOutputOptions",
                    ["packages/vite/src/node/"],
                ),
                # Dual-stack: 0 CALL may still have IPC bridge
                (
                    TAURI_API,
                    "log_operation",
                    ["src-tauri/src/"],
                ),
            ]
            for proj, sym, scope in cases:
                if not Path(proj).is_dir():
                    continue
                c, st, rec, ms, err, _ = attack_trace(
                    args.base, proj, sym, scope, timeout=120
                )
                if err or is_building(c, st):
                    if args.verbose:
                        print(f"    B12 {Path(proj).name}/{sym} skip err/building")
                    continue
                if c.startswith("Disambiguate") or (st.get("blast_domain") or "") == "disambiguate":
                    if args.verbose:
                        print(f"    B12 {Path(proj).name}/{sym} disambiguate (ok)")
                    continue
                t = st.get("target") or {}
                if not t:
                    if args.verbose:
                        print(f"    B12 {Path(proj).name}/{sym} miss")
                    continue
                tel = st.get("telemetry") if isinstance(st.get("telemetry"), dict) else {}
                try:
                    in_d = int(tel.get("seed_in_degree") or 0)
                except (TypeError, ValueError):
                    in_d = 0
                callers = st.get("callers") or []
                br = bridges_of(st)
                # Only assert when true 0 CALL warehouse + empty sample
                if in_d == 0 and not callers:
                    low = c.lower()
                    honest = (
                        "not proof of dead code" in low
                        or "not dead code" in low
                        or "do not treat as dead" in low
                        or "do not delete as dead" in low
                        or ("bridges" in low and br)
                    )
                    if not honest and is_spectacular(rec):
                        record(
                            "B12_callback_dead_code_lie",
                            "spectacular",
                            proj,
                            f"{sym}: 0 CALL callers but no dead-code honesty in content",
                            rec,
                            ms,
                            content_head=c[:240],
                        )
                    elif not honest:
                        record(
                            "B12_callback_dead_code_lie",
                            "soft",
                            proj,
                            f"{sym}: 0 CALL callers; weak honesty wording",
                            rec,
                            ms,
                            content_head=c[:240],
                        )
                if args.verbose:
                    print(
                        f"    B12 {Path(proj).name}/{sym} in_d={in_d} "
                        f"callers={len(callers)} br={len(br)} {ms:.0f}ms {rec_s(rec)}"
                    )

        run("B12_callback_chasm", FASTAPI, b12_callback_chasm)

        def b13_wildcard_wipeout():
            """Wildcard re-export surface (django forms import *) — no high invent.

            Soft: BoundField may still have CALL callers; assert no frankenstein ★
            under wrong name and disambiguate if multi-loc danger. Document limit.
            """
            if not Path(DJANGO).is_dir():
                return
            # Public surface re-exported via forms/__init__.py import *
            c, st, rec, ms, err, _ = attack_trace(
                args.base,
                DJANGO,
                "BoundField",
                ["django/forms/"],
                timeout=120,
            )
            if err or is_building(c, st):
                if args.verbose:
                    print(f"    B13 BoundField skip {err or 'building'}")
                return
            t = st.get("target") or {}
            domain = (st.get("blast_domain") or "").lower()
            if t and (t.get("name") or "") not in ("BoundField", "boundfield"):
                if is_spectacular(rec):
                    record(
                        "B13_wildcard_wrong_star",
                        "spectacular",
                        DJANGO,
                        f"BoundField ★ wrong name={t.get('name')}",
                        rec,
                        ms,
                    )
            # If 0 CALL callers, must not lie as dead (wildcard may hide reverse)
            tel = st.get("telemetry") if isinstance(st.get("telemetry"), dict) else {}
            try:
                in_d = int(tel.get("seed_in_degree") or 0)
            except (TypeError, ValueError):
                in_d = 0
            if in_d == 0 and not (st.get("callers") or []):
                low = c.lower()
                if "dead code" in low and "not" not in low.split("dead")[0][-20:]:
                    # crude: "not dead" is ok; bare "dead code" implication is bad
                    pass
                if "not proof of dead" not in low and "not dead code" not in low:
                    if is_spectacular(rec):
                        record(
                            "B13_wildcard_zero_call_lie",
                            "soft",
                            DJANGO,
                            "BoundField 0 CALL without dead-code honesty (wildcard limit)",
                            rec,
                            ms,
                        )
            if args.verbose:
                print(
                    f"    B13 BoundField domain={domain} ★={t.get('name') if t else None} "
                    f"in_d={in_d} callers={len(st.get('callers') or [])} {ms:.0f}ms"
                )

        run("B13_wildcard_wipeout", DJANGO, b13_wildcard_wipeout)

    # Report
    print("\n" + "=" * 72)
    print(f"ADVERSARIAL DOGFOOD REPORT  suite={args.suite}")
    print("=" * 72)
    by: dict[str, list[Fail]] = {}
    for f in fails:
        by.setdefault(f.attack, []).append(f)
    for attack, items in sorted(by.items()):
        spec = sum(1 for x in items if x.severity == "spectacular")
        soft = sum(1 for x in items if x.severity == "soft")
        print(f"\n## {attack}  (n={len(items)}, spectacular={spec}, soft={soft})")
        for x in items:
            print(f"  [{x.severity}] {Path(x.project).name}  {x.ms:.0f}ms")
            print(f"    {x.detail}")
            if x.receipt:
                print(f"    receipt={x.receipt}")

    spec_n = sum(1 for f in fails if f.severity == "spectacular")
    soft_n = sum(1 for f in fails if f.severity == "soft")
    print("\n" + "=" * 72)
    print(f"TOTAL spectacular={spec_n}  soft={soft_n}  attacks_run={attacks}")
    print("=" * 72)

    report = {
        "base": args.base,
        "suite": args.suite,
        "roots": roots,
        "stats": stats,
        "fails": [asdict(f) for f in fails],
        "summary": {
            "spectacular": spec_n,
            "soft": soft_n,
            "attacks": attacks,
        },
        "thresholds": {
            "ghost_ms_soft": GHOST_MS_SOFT,
            "ghost_ms_spectacular": GHOST_MS_SPECTACULAR,
            "dense_payload_soft": DENSE_PAYLOAD_SOFT_BYTES,
            "dense_payload_spec": DENSE_PAYLOAD_SPEC_BYTES,
        },
    }
    Path(args.json).write_text(json.dumps(report, indent=2))
    print(f"Wrote {args.json}")
    return 1 if spec_n else 0


if __name__ == "__main__":
    sys.exit(main())
