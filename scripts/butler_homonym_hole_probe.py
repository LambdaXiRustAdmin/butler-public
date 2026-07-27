#!/usr/bin/env python3
"""Butler homonym / multi-loc pin hole probe (repo-agnostic cases).

P0 accuracy: wrong ★ under multi-location seeds is the classic green-receipt lie.

Invariants:
  I-H1  Unscoped multi-loc seed → disambiguate OR ≥2 locations
  I-H2  Pin scope to each serious location file → ★ file basename matches pin
  I-H3  High|complete Trace must not bridge to junk peers (TEST_SUBMODULE, …)

Usage:
  python3 scripts/butler_homonym_hole_probe.py -v --json /tmp/homonym_holes.json
Exit 0 = no spectacular; 1 = spectacular; 2 = no cases runnable.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
import urllib.request
from collections import defaultdict
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Optional

DEFAULT_BASE = os.environ.get("BUTLER_URL", "http://127.0.0.1:8002").rstrip("/")

# Host projects root (override with BUTLER_HOST_MOUNT)
HOST_ROOT = Path(os.environ.get("BUTLER_HOST_MOUNT", str(Path.home() / "projects")))

def _hp(rel: str) -> str:
    """Absolute path under HOST_ROOT (or container-friendly host mount)."""
    return str(HOST_ROOT / rel)


JUNK_PEERS = frozenset(
    {"TEST_SUBMODULE", "PYBIND11_MODULE", "unknown", "default", "TEST_CASE"}
)


@dataclass
class Case:
    name: str
    project: str
    seed: str
    # Optional explicit location basenames that must be pinable
    expect_loc_basenames: list[str] = field(default_factory=list)
    min_locations: int = 2


# Type exemplars — multi-loc / dual-lang seeds (not repo lock-in).
CASES: list[Case] = [
    Case(
        "pybind-test_bytes",
        _hp("test_repos/pybind11"),
        "test_bytes",
        ["test_constants_and_functions.py", "test_pytypes.py"],
        2,
    ),
    Case(
        "tauri-echo",
        _hp("test_repos/tauri/examples/api"),
        "echo",
        ["Communication.svelte", "cmd.rs"],
        2,
    ),
    Case(
        "tauri-spam",
        _hp("test_repos/tauri/examples/api"),
        "spam",
        ["Communication.svelte", "cmd.rs"],
        2,
    ),
    Case(
        "pyo3-search",
        _hp("test_repos/pyo3/examples/word-count"),
        "search",
        ["lib.rs"],  # may also have py twin under different name
        1,
    ),
    Case(
        "fastapi-main",
        _hp("test_repos/fastapi-ts"),
        "main",
        [],
        2,
    ),
]


@dataclass
class Hole:
    inv: str
    severity: str
    root: str
    seed: str
    detail: str
    receipt: str = ""
    star: str = ""
    evidence: dict[str, Any] = field(default_factory=dict)


def host_to_container(path: str) -> str:
    host = os.environ.get("BUTLER_HOST_MOUNT", str(Path.home() / "projects"))
    cont = os.environ.get("BUTLER_CONTAINER_MOUNT", "/projects")
    p = path.rstrip("/")
    if p.startswith(host + "/") or p == host:
        return cont + p[len(host) :]
    return p


def container_to_host(path: str) -> str:
    host = os.environ.get("BUTLER_HOST_MOUNT", str(Path.home() / "projects"))
    cont = os.environ.get("BUTLER_CONTAINER_MOUNT", "/projects")
    p = path.rstrip("/")
    if p.startswith(cont + "/") or p == cont:
        return host + p[len(cont) :]
    return p


def get_json(url: str, timeout: float = 30.0) -> dict[str, Any]:
    with urllib.request.urlopen(urllib.request.Request(url), timeout=timeout) as r:
        return json.loads(r.read().decode())


def post_context(base: str, payload: dict[str, Any], timeout: float = 90.0):
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        f"{base}/context", data=data, headers={"Content-Type": "application/json"}
    )
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


def content_text(d: dict) -> str:
    c = d.get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list) and c:
        return (c[0] or {}).get("text", "") or ""
    return ""


def structured(d: dict) -> dict:
    st = d.get("structured")
    return st if isinstance(st, dict) else {}


def receipt_bits(st: dict, content: str) -> dict[str, str]:
    r = st.get("receipt") if isinstance(st.get("receipt"), dict) else {}
    conf = str(r.get("confidence") or (st.get("state") or {}).get("confidence") or "").lower()
    edges = str(r.get("edges") or "")
    if not edges:
        m = re.search(r"edges[=:]?\s*(complete|partial|building)", content, re.I)
        edges = m.group(1).lower() if m else ""
    ladder = str((st.get("state") or {}).get("confidence") or "")
    return {
        "confidence": conf,
        "edges": edges,
        "ladder": ladder,
        "basis": str(r.get("basis") or ""),
    }


def is_spectacular(rec: dict[str, str]) -> bool:
    if rec.get("basis", "").lower() in ("error", "scope_not_found"):
        return False
    if rec.get("confidence") in ("low", "error"):
        return False
    high = rec.get("confidence") == "high" or rec.get("ladder") == "edges_full"
    complete = rec.get("edges") == "complete" or rec.get("ladder") == "edges_full"
    return bool(high and complete)


def star_str(st: dict) -> str:
    t = st.get("target") or {}
    if not t:
        return ""
    return f"{t.get('name')} @ {t.get('file')}:{t.get('line')} kind={st.get('seed_kind')}"


def project_available(project: str, complete: dict) -> Optional[str]:
    cont = host_to_container(project)
    host = container_to_host(project)
    for key in (cont, host, project):
        if key in complete:
            return host if "/home/" in host or host.startswith("/home") else project
    # suffix match
    tail = project.rstrip("/").split("/")[-1]
    for k in complete:
        if k.rstrip("/").endswith(tail) or project.rstrip("/") in k or k in project:
            if "word-count" in project and "word-count" not in k:
                continue
            return container_to_host(k) if k.startswith("/projects") else k
    # nested word-count
    for k in complete:
        if "word-count" in project and "word-count" in k:
            return container_to_host(k) if k.startswith("/projects") else k
    return None


def trace(base, project, symbol, scope=None):
    payload = {
        "project": project,
        "goal": "trace",
        "target_symbol": symbol,
        "detail": "dense",
    }
    if scope:
        payload["scope_paths"] = scope
    d, ms, err = post_context(base, payload)
    return d, content_text(d), structured(d), ms, err


def serious_locations(st: dict) -> list[dict]:
    locs = st.get("locations") or []
    out = []
    for loc in locs:
        f = (loc.get("file") or "").replace("\\", "/")
        fl = f.lower()
        if "/benchmarks/" in fl or "/benches/" in fl:
            continue
        kind = (loc.get("kind") or "").lower()
        if "call_expression" in kind or kind in ("expression_statement",):
            continue
        out.append(loc)
    return out if out else list(locs)


def check_case(base: str, case: Case, project: str, verbose: bool) -> list[Hole]:
    holes: list[Hole] = []
    root = host_to_container(project)

    def add(inv, detail, rec, st, sev=None, evidence=None):
        holes.append(
            Hole(
                inv=inv,
                severity=sev or ("spectacular" if is_spectacular(rec) else "soft"),
                root=root,
                seed=f"{case.name}:{case.seed}",
                detail=detail,
                receipt=f"{rec.get('confidence')}|{rec.get('basis')}|{rec.get('edges')}",
                star=star_str(st),
                evidence=evidence or {},
            )
        )

    d, content, st, ms, err = trace(base, project, case.seed, None)
    if verbose:
        print(f"  unscoped {case.seed!r} {ms:.0f}ms | {content[:90].replace(chr(10),' ')}")
    if err or "lang void" in content.lower():
        add("I_H0_skip", f"skip: {err or 'lang void'}", {}, st, sev="soft")
        return holes

    rec = receipt_bits(st, content)
    locs = serious_locations(st)
    is_disambig = (
        content.startswith("Disambiguate")
        or st.get("blast_domain") == "disambiguate"
        or rec.get("basis") == "disambiguate"
    )

    # I-H1 multi-loc awareness
    if case.min_locations >= 2:
        nloc = len(locs) if locs else len(st.get("locations") or [])
        if nloc < case.min_locations and not is_disambig:
            # might still be single preferred with alts hidden — check content
            m = re.search(r"(\d+)\s+alt", content)
            alts = int(m.group(1)) if m else 0
            if alts + 1 < case.min_locations and is_spectacular(rec):
                add(
                    "I_H1_multi_loc_hidden",
                    f"seed {case.seed!r} expected multi-loc (min={case.min_locations}) "
                    f"but got locs={nloc} alts={alts} without disambiguate under high trust",
                    rec,
                    st,
                    sev="spectacular",
                    evidence={"nloc": nloc, "alts": alts},
                )
        # If high complete full Trace with many alts — soft pressure toward disambiguate
        if is_spectacular(rec) and not is_disambig and nloc >= 3:
            add(
                "I_H1_high_with_many_alts",
                f"high|complete Trace with {nloc} serious locations (prefer disambiguate/pin)",
                rec,
                st,
                sev="soft",
                evidence={"nloc": nloc},
            )

    # I-H2 pin each location
    pins: list[tuple[str, str]] = []
    for loc in locs[:6]:
        f = (loc.get("file") or "").replace("\\", "/")
        if not f:
            continue
        base_name = Path(f).name
        # repo-relative pin: last 2–3 components under project
        parts = [p for p in f.split("/") if p]
        try:
            proj_parts = project.rstrip("/").split("/")
            # relative from project root if possible
            if f.startswith(project.rstrip("/") + "/"):
                rel = f[len(project.rstrip("/")) + 1 :]
            else:
                # take from known package segment
                rel = "/".join(parts[-3:]) if len(parts) >= 3 else base_name
        except Exception:
            rel = base_name
        pins.append((base_name, rel))

    # Also force expected basenames if listed
    for bn in case.expect_loc_basenames:
        if not any(p[0] == bn for p in pins):
            pins.append((bn, bn))

    seen_pin = set()
    for base_name, rel in pins:
        if base_name in seen_pin:
            continue
        seen_pin.add(base_name)
        # pin as file path fragment
        pin = [rel if "/" in rel else f"**/{base_name}"]
        # root-anchored: try path containing basename
        # Prefer last-2 components from any location with that basename
        for loc in locs:
            f = (loc.get("file") or "").replace("\\", "/")
            if Path(f).name == base_name:
                if f.startswith(project.rstrip("/") + "/"):
                    pin = [f[len(project.rstrip("/")) + 1 :]]
                else:
                    parts = [p for p in f.split("/") if p]
                    pin = ["/".join(parts[-3:])] if len(parts) >= 3 else [base_name]
                break
        d2, c2, st2, ms2, err2 = trace(base, project, case.seed, pin)
        if verbose:
            print(f"    pin {pin} {ms2:.0f}ms | {c2[:70].replace(chr(10),' ')}")
        if err2 or "Scope not found" in c2 or c2.startswith("Orchestrate error"):
            # try simpler basename pin under tests/ or src/
            for guess in ([f"tests/{base_name}"], [f"src/{base_name}"], [base_name]):
                d2, c2, st2, ms2, err2 = trace(base, project, case.seed, guess)
                if not err2 and "Scope not found" not in c2 and not c2.startswith("Orchestrate"):
                    pin = guess
                    break
            else:
                add(
                    "I_H2_pin_failed",
                    f"could not pin {case.seed!r} to {base_name}: {c2[:100]}",
                    receipt_bits(st2, c2),
                    st2,
                    sev="soft",
                )
                continue
        if c2.startswith("Disambiguate") or st2.get("blast_domain") == "disambiguate":
            continue  # pin not strong enough — soft
        t2 = st2.get("target") or {}
        star_file = (t2.get("file") or "").replace("\\", "/")
        if star_file and Path(star_file).name != base_name:
            rec2 = receipt_bits(st2, c2)
            add(
                "I_H2_pin_star_mismatch",
                f"pin for {base_name} → ★ file {Path(star_file).name} "
                f"(path={star_file})",
                rec2,
                st2,
                sev="spectacular" if is_spectacular(rec2) else "soft",
                evidence={"pin": pin, "want_basename": base_name, "got": star_file},
            )

        # I-H3 junk bridges
        bridges = (st2.get("bridge_callers") or []) + (st2.get("bridge_callees") or [])
        junk = [
            n.get("name")
            for n in bridges
            if (n.get("name") or "") in JUNK_PEERS
            or str(n.get("name") or "").startswith("TEST_")
        ]
        if junk:
            rec2 = receipt_bits(st2, c2)
            add(
                "I_H3_junk_bridge",
                f"pin {base_name}: junk bridge peers {junk}",
                rec2,
                st2,
                sev="spectacular" if is_spectacular(rec2) else "soft",
                evidence={"junk": junk},
            )

    return holes


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default=DEFAULT_BASE)
    ap.add_argument("--json", default="/tmp/butler_homonym_holes.json")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    print(f"Butler homonym hole probe  base={args.base}")
    try:
        health = get_json(f"{args.base}/mcp/health")
    except Exception as e:
        print(f"health failed: {e}")
        return 2

    complete = {
        k: v
        for k, v in (health.get("loaded") or {}).items()
        if isinstance(v, dict) and v.get("edges_complete") and v.get("ready")
    }
    print(f"Complete warehouses: {len(complete)}")

    all_holes: list[Hole] = []
    stats = []
    skipped = []
    for case in CASES:
        proj = project_available(case.project, complete)
        if not proj:
            skipped.append(case.name)
            print(f"\n=== SKIP {case.name}")
            continue
        print(f"\n=== CASE {case.name} seed={case.seed} project={proj}")
        t0 = time.perf_counter()
        holes = check_case(args.base, case, proj, args.verbose)
        ms = (time.perf_counter() - t0) * 1000
        all_holes.extend(holes)
        spec = sum(1 for h in holes if h.severity == "spectacular")
        soft = sum(1 for h in holes if h.severity == "soft")
        print(f"  holes spectacular={spec} soft={soft} ms={ms:.0f}")
        stats.append({"case": case.name, "ms": ms, "spec": spec, "soft": soft})

    by_inv: dict[str, list[Hole]] = defaultdict(list)
    for h in all_holes:
        by_inv[h.inv].append(h)
    print("\n" + "=" * 72)
    print("HOMONYM HOLE REPORT")
    print("=" * 72)
    for inv in sorted(by_inv):
        g = by_inv[inv]
        print(f"\n## {inv} (n={len(g)}, spectacular={sum(1 for h in g if h.severity=='spectacular')})")
        for h in sorted(g, key=lambda x: 0 if x.severity == "spectacular" else 1)[:6]:
            print(f"  [{h.severity}] {h.seed}")
            print(f"    {h.detail}")
            print(f"    receipt={h.receipt} ★={h.star}")

    spectacular = [h for h in all_holes if h.severity == "spectacular"]
    soft = [h for h in all_holes if h.severity == "soft"]
    print(f"\nTOTAL spectacular={len(spectacular)} soft={len(soft)} run={len(stats)} skip={len(skipped)}")
    Path(args.json).write_text(
        json.dumps(
            {
                "stats": stats,
                "skipped": skipped,
                "holes": [asdict(h) for h in all_holes],
                "summary": {"spectacular": len(spectacular), "soft": len(soft)},
            },
            indent=2,
        )
    )
    print(f"Wrote {args.json}")
    if not stats and skipped:
        return 2
    return 1 if spectacular else 0


if __name__ == "__main__":
    sys.exit(main())
