#!/usr/bin/env python3
"""Butler accuracy/completeness hole probe (repo-agnostic).

Finds *spectacular* failures: receipt high + edges complete + invariant false.
Not a volume stress test — property checks on sampled seeds from warm Complete
warehouses.

Usage:
  python3 scripts/butler_hole_probe.py
  python3 scripts/butler_hole_probe.py --base http://127.0.0.1:8002 --roots /projects/lambda-wisperer,/projects/test_repos/click
  python3 scripts/butler_hole_probe.py --max-seeds 40 --json /tmp/holes.json -v

Exit 0 if no *spectacular* holes (high+complete+fail). Soft fails still printed.
Exit 1 if any spectacular hole.
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
from pathlib import Path
from typing import Any, Optional

DEFAULT_BASE = os.environ.get("BUTLER_URL", "http://127.0.0.1:8002").rstrip("/")

# Short names that often multi-seed (probe disambiguate / pin).
HOMONYM_BAIT = [
    "parse",
    "new",
    "main",
    "open",
    "load",
    "get",
    "run",
    "build",
    "init",
    "create",
    "update",
    "handle",
    "dispatch",
    "format",
    "config",
    "state",
    "context",
    "error",
    "node",
    "path",
    "app",
    "command",
]

# Common scope candidates (first that Arch-complete wins).
SCOPE_CANDIDATES = [
    ["src/"],
    ["cli/src/"],
    ["cli/"],
    ["src/click/"],
    ["lib/"],
    ["packages/"],
    ["crates/"],
    ["app/"],
    ["."],
]


@dataclass
class Hole:
    inv: str
    severity: str  # spectacular | soft
    root: str
    seed: str
    detail: str
    receipt: str = ""
    star: str = ""
    evidence: dict[str, Any] = field(default_factory=dict)


def get_json(url: str, timeout: float = 30.0) -> dict[str, Any]:
    req = urllib.request.Request(url)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode())


def post_context(base: str, payload: dict[str, Any], timeout: float = 90.0) -> tuple[dict[str, Any], float, Optional[str]]:
    url = f"{base}/context"
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
        ms = (time.perf_counter() - t0) * 1000
        return {}, ms, str(e)


def content_text(d: dict[str, Any]) -> str:
    c = d.get("content")
    if isinstance(c, list) and c:
        return (c[0] or {}).get("text", "") or ""
    if isinstance(c, str):
        return c
    return ""


def structured(d: dict[str, Any]) -> dict[str, Any]:
    st = d.get("structured")
    return st if isinstance(st, dict) else {}


def receipt_bits(st: dict[str, Any], content: str) -> dict[str, str]:
    r = st.get("receipt") if isinstance(st.get("receipt"), dict) else {}
    conf = (r.get("confidence") or st.get("state", {}).get("confidence") or "")
    if isinstance(conf, str):
        conf = conf.lower()
    edges = (r.get("edges") or "")
    if not edges:
        m = re.search(r"edges[=:]?\s*(complete|partial|building)", content, re.I)
        edges = m.group(1).lower() if m else ""
    if not conf:
        m = re.search(r"receipt:\s*(\w+)", content, re.I)
        conf = m.group(1).lower() if m else ""
    # ladder in state
    ladder = ""
    if isinstance(st.get("state"), dict):
        ladder = (st["state"].get("confidence") or "") or ""
    tel = st.get("telemetry") if isinstance(st.get("telemetry"), dict) else {}
    edges_complete = tel.get("edges_complete")
    if edges_complete is True:
        edges = edges or "complete"
    elif edges_complete is False and not edges:
        edges = "partial"
    return {
        "confidence": str(conf or ""),
        "edges": str(edges or ""),
        "ladder": str(ladder or ""),
        "basis": str((r.get("basis") or "")),
    }


def is_spectacular(rec: dict[str, str]) -> bool:
    # Error / repair responses must never count as high-trust spectaculars.
    if (rec.get("basis") or "").lower() in ("error", "scope_not_found"):
        return False
    conf = rec.get("confidence", "")
    edges = rec.get("edges", "")
    ladder = rec.get("ladder", "")
    if conf in ("low", "error") or ladder in ("error", "inventory"):
        return False
    if edges in ("n/a", "na", "none"):
        return False
    high = conf in ("high",) or ladder in ("edges_full",)
    complete = edges in ("complete",) or ladder == "edges_full"
    # receipt line "high | … | complete"
    return bool(high and complete)


def star_str(st: dict[str, Any]) -> str:
    t = st.get("target") or {}
    if not t:
        return ""
    return f"{t.get('name')} @ {t.get('file')}:{t.get('line')} kind={st.get('seed_kind') or t.get('kind') or '?'}"


def hop1(neighbors: list[dict[str, Any]]) -> list[dict[str, Any]]:
    out = []
    for n in neighbors or []:
        hop = n.get("hop", 1)
        try:
            hop = int(hop)
        except (TypeError, ValueError):
            hop = 1
        if hop <= 1:
            out.append(n)
    return out


def names(neighbors: list[dict[str, Any]]) -> set[str]:
    return {n.get("name", "") for n in neighbors if n.get("name")}


def parse_called_by(content: str) -> Optional[int]:
    m = re.search(r"called by (\d+)", content)
    if m:
        return int(m.group(1))
    if "isolated in CALL graph" in content or "none fan-in" in content:
        # may still have called by 0
        m2 = re.search(r"called by (\d+)", content)
        return int(m2.group(1)) if m2 else 0
    return None


def complete_roots(health: dict[str, Any]) -> list[tuple[str, dict[str, Any]]]:
    loaded = health.get("loaded") or {}
    out = []
    for k, v in loaded.items():
        if isinstance(v, dict) and v.get("edges_complete") and v.get("ready") and (v.get("nodes") or 0) > 0:
            out.append((k, v))
    return sorted(out, key=lambda x: -x[1].get("nodes", 0))


def host_project_alias(root: str) -> str:
    """Prefer host path for client if mounts known; server translates."""
    host = os.environ.get("BUTLER_HOST_MOUNT", str(Path.home() / "projects"))
    cont = os.environ.get("BUTLER_CONTAINER_MOUNT", "/projects")
    if root.startswith(cont + "/") or root == cont:
        return host + root[len(cont) :]
    return root


def _project_root_aliases(project: str) -> list[str]:
    """Host + container forms of project root (trailing slash stripped)."""
    host = os.environ.get("BUTLER_HOST_MOUNT", str(Path.home() / "projects"))
    cont = os.environ.get("BUTLER_CONTAINER_MOUNT", "/projects")
    p = project.replace("\\", "/").rstrip("/")
    aliases = {p}
    if p.startswith(host + "/") or p == host:
        aliases.add(cont + p[len(host) :])
    if p.startswith(cont + "/") or p == cont:
        aliases.add(host + p[len(cont) :])
    return [a.rstrip("/") for a in aliases if a]


def file_scope_pin(project: str, file_path: str) -> Optional[list[str]]:
    """Repo-relative scope pin for a file (root-anchored: <project>/<pin>).

    Never use last-N absolute path segments — that yields pins like
    `fd/src/exit_codes.rs` which fail with scope_not_found under project=…/fd.
    """
    cf = (file_path or "").replace("\\", "/").strip()
    if not cf:
        return None
    for root in _project_root_aliases(project):
        if cf == root:
            return None
        prefix = root + "/"
        if cf.startswith(prefix):
            rel = cf[len(prefix) :]
            return [rel] if rel else None
    # Last resort: drop absolute prefix noise; prefer …/src/… tail
    parts = [p for p in cf.split("/") if p]
    if "src" in parts:
        i = parts.index("src")
        return ["/".join(parts[i:])]
    if len(parts) >= 2:
        return ["/".join(parts[-2:])]
    return [parts[-1]] if parts else None


def pick_scope(base: str, project: str) -> list[str]:
    for scope in SCOPE_CANDIDATES:
        d, _, err = post_context(
            base,
            {
                "project": project,
                "goal": "architecture",
                "scope_paths": scope,
                "detail": "compact",
            },
            timeout=60,
        )
        if err:
            continue
        c = content_text(d)
        if "coverage" in c and "complete" in c:
            return scope
        if "Architectural summary" in c and "Scope not found" not in c and "refused" not in c.lower():
            # accept mid-size maps
            if "skeleton" in c or "hubs" in c:
                return scope
    return ["src/"]


def extract_hub_names(content: str) -> list[str]:
    names_out: list[str] = []
    for line in content.splitlines():
        # "  - name · lang · cluster  score=…"
        interesting = "score=" in line or "·" in line or "hub" in line.lower()
        m = re.search(r"^\s*[-*]\s+`?([A-Za-z_][A-Za-z0-9_]*)`?", line)
        if m and interesting:
            names_out.append(m.group(1))
        m2 = re.search(r"^\s*[-*]\s+([A-Za-z_][A-Za-z0-9_]*)\s", line)
        if m2 and interesting:
            names_out.append(m2.group(1))
    # also backtick names on hub-ish lines
    for line in content.splitlines():
        if "score=" not in line and "·" not in line:
            continue
        for m in re.finditer(r"`([A-Za-z_][A-Za-z0-9_]{2,})`", line):
            names_out.append(m.group(1))
    # dedupe preserve order
    seen = set()
    out = []
    skip = {
        "coverage",
        "skeleton",
        "hubs",
        "next",
        "format",
        "files",
        "paths",
        "complete",
        "incomplete",
    }
    for n in names_out:
        if n.lower() in skip:
            continue
        if n not in seen:
            seen.add(n)
            out.append(n)
    return out[:30]


def trace(
    base: str,
    project: str,
    symbol: str,
    scope: Optional[list[str]] = None,
    detail: str = "compact",
) -> tuple[dict[str, Any], str, dict[str, Any], float, Optional[str]]:
    payload: dict[str, Any] = {
        "project": project,
        "goal": "trace",
        "target_symbol": symbol,
        "detail": detail,
    }
    if scope:
        payload["scope_paths"] = scope
    d, ms, err = post_context(base, payload)
    return d, content_text(d), structured(d), ms, err


def check_invariants(
    root: str,
    symbol: str,
    content: str,
    st: dict[str, Any],
    scope: Optional[list[str]],
) -> list[Hole]:
    holes: list[Hole] = []
    rec = receipt_bits(st, content)
    spectacular = is_spectacular(rec)
    sev = "spectacular" if spectacular else "soft"
    star = star_str(st)
    rec_s = f"{rec.get('confidence')}|{rec.get('basis')}|{rec.get('edges')}|ladder={rec.get('ladder')}"

    def hole(inv: str, detail: str, evidence: Optional[dict] = None, force_sev: Optional[str] = None):
        holes.append(
            Hole(
                inv=inv,
                severity=force_sev or sev,
                root=root,
                seed=symbol,
                detail=detail,
                receipt=rec_s,
                star=star,
                evidence=evidence or {},
            )
        )

    domain = (st.get("blast_domain") or "") or ""
    tel = st.get("telemetry") if isinstance(st.get("telemetry"), dict) else {}
    err = st.get("error") or ""
    if content.startswith("Orchestrate error") or err:
        # I1 miss vs complete
        if "not found" in content.lower() or "not found" in str(err).lower():
            if spectacular or rec.get("edges") == "complete" or rec.get("ladder") == "edges_full":
                # miss with complete edges is OK for true miss; only flag if disambiguate-ish short name
                # Soft: short name miss without disambiguate locations is ok product behavior
                pass
        if domain == "disambiguate" or content.startswith("Disambiguate"):
            # ok path
            pass
        return holes

    if domain == "disambiguate" or content.startswith("Disambiguate"):
        # I9 later with two pins
        return holes

    target = st.get("target") or {}
    seed_kind = (st.get("seed_kind") or target.get("kind") or "").lower()
    callers = st.get("callers") or []
    callees = st.get("callees") or []
    spine = st.get("caller_path") or []
    c1 = hop1(callers)
    e1 = hop1(callees)

    # I5 type ⇒ no spine
    if domain == "type_neighborhood" or "struct" in seed_kind or "class" in seed_kind:
        if spine:
            hole(
                "I5_type_no_spine",
                f"type/domain={domain} kind={seed_kind} but caller_path has {len(spine)} steps",
                {"spine": [s.get("name") for s in spine]},
            )

    # I7 called_by vs seed_in_degree
    called_by = parse_called_by(content)
    in_deg = tel.get("seed_in_degree")
    if in_deg is not None and called_by is not None:
        try:
            if int(in_deg) != int(called_by):
                hole(
                    "I7_called_by_vs_telemetry",
                    f"content called by {called_by} != telemetry seed_in_degree {in_deg}",
                    {"called_by": called_by, "seed_in_degree": in_deg},
                )
        except (TypeError, ValueError):
            pass

    # I7b: seed_in_degree 0 but hop-1 callers present (bootstrap / reverse hole)
    try:
        in_deg_i = int(in_deg) if in_deg is not None else None
    except (TypeError, ValueError):
        in_deg_i = None
    if in_deg_i == 0 and c1:
        hole(
            "I7b_reverse_zero_with_callers",
            f"seed_in_degree=0 but hop-1 callers listed: {[n.get('name') for n in c1[:5]]}",
            {
                "callers_hop1": [{"name": n.get("name"), "file": n.get("file"), "line": n.get("line")} for n in c1[:8]],
                "note": "loc-fallback/bootstrap or reverse-index hole; must stay honest in scope line",
            },
            force_sev="spectacular" if spectacular else "soft",
        )
    if called_by == 0 and c1:
        hole(
            "I7c_scope_zero_with_callers",
            f"scope 'called by 0' but hop-1 callers non-empty: {[n.get('name') for n in c1[:5]]}",
            {"callers_hop1": [n.get("name") for n in c1]},
            force_sev="spectacular" if spectacular else "soft",
        )

    # I6 spine hop1 in callers or bootstrap
    if spine:
        s0 = spine[0]
        sname = s0.get("name")
        cnames = names(c1)
        all_cnames = names(callers)
        if sname and sname not in cnames and sname not in all_cnames:
            # allow if only bootstrap and callers empty
            if c1 or callers:
                hole(
                    "I6_spine_not_in_callers",
                    f"spine[0]={sname} not in callers list {sorted(all_cnames)[:12]}",
                    {"spine0": s0, "callers": list(all_cnames)[:20]},
                )

    # I8 seed kind for likely function query (symbol looks like function name)
    if symbol and symbol[0].islower() or (symbol[:1].isupper() and domain == "call"):
        if seed_kind and "call_expression" in seed_kind:
            hole(
                "I8_call_expression_star",
                f"★ kind is call_expression for symbol={symbol} (prefer def)",
                {"seed_kind": seed_kind, "star": star},
                force_sev="soft",  # often soft; promote spectacular if high confidence
            )
            if spectacular:
                holes[-1].severity = "spectacular"

    # I8b: function-like name but type domain without being a type query
    # skip

    # Confidence lie: high + error?
    if spectacular and st.get("error"):
        hole(
            "I0_high_with_error",
            f"high/complete receipt with error field: {st.get('error')}",
            {},
            force_sev="spectacular",
        )

    return holes


def check_bidirectional(
    base: str,
    project: str,
    root: str,
    symbol: str,
    st: dict[str, Any],
    scope: Optional[list[str]],
    budget: list[int],
) -> list[Hole]:
    """I4: if B in hop-1 callees(A), Trace B should list A as hop-1 caller (or soft incomplete)."""
    holes: list[Hole] = []
    if budget[0] <= 0:
        return holes
    # Type neighborhood "callees" are often methods of the type, not CALL edges
    # from an invoker named like the type — reverse check is meaningless.
    if (st.get("blast_domain") or "") == "type_neighborhood":
        return holes
    callees = hop1(st.get("callees") or [])
    if not callees:
        return holes
    # pick up to 2 external-ish callees
    picks = []
    seed_file = ((st.get("target") or {}).get("file") or "").replace("\\", "/")
    for c in callees:
        if c.get("name") and c.get("name") != symbol:
            picks.append(c)
        if len(picks) >= 2:
            break
    for c in picks:
        if budget[0] <= 0:
            break
        budget[0] -= 1
        child = c.get("name")
        # pin to child file if possible for homonyms — must be root-anchored
        # (project-relative), never last-N of an absolute host path.
        cf = (c.get("file") or "").replace("\\", "/")
        pin = file_scope_pin(project, cf)
        child_scope = pin if pin else scope
        d, content, st2, _, err = trace(base, project, child, child_scope)
        if err or content.startswith("Orchestrate error") or content.startswith("Disambiguate"):
            continue
        # Pin miss / repair — not a reverse CALL hole (probe hygiene).
        bd = (st2.get("blast_domain") or "") or ""
        if bd in ("scope_not_found",) or st2.get("error") or not (st2.get("target") or {}):
            continue
        rec = receipt_bits(st2, content)
        if (rec.get("basis") or "").lower() in ("error", "scope_not_found"):
            continue
        callers2 = st2.get("callers") or []
        cnames = names(callers2)
        t2 = st2.get("target") or {}
        # Child ★ should match callee file (else homonym — not reverse hole).
        child_star_file = (t2.get("file") or "").replace("\\", "/")
        callee_file = (c.get("file") or "").replace("\\", "/")
        if child_star_file and callee_file:
            if Path(child_star_file).name != Path(callee_file).name:
                continue  # wrong seed for child name
        if symbol in cnames:
            continue
        tel2 = st2.get("telemetry") if isinstance(st2.get("telemetry"), dict) else {}
        in_deg = tel2.get("seed_in_degree")
        try:
            in_deg_i = int(in_deg) if in_deg is not None else None
        except (TypeError, ValueError):
            in_deg_i = None
        omitted = tel2.get("callers_omitted")
        try:
            omitted_i = int(omitted) if omitted is not None else 0
        except (TypeError, ValueError):
            omitted_i = 0
        # Hub / capped list: parent may be real but not in sample — soft only.
        pack_may_omit = (
            (in_deg_i is not None and in_deg_i > 12)
            or omitted_i > 0
            or len(callers2) >= 10
        )
        if pack_may_omit:
            sev = "soft"
            inv = "I4_reverse_asymmetry_possible_pack_omit"
        else:
            sev = "spectacular" if is_spectacular(rec) else "soft"
            inv = "I4_reverse_asymmetry"
        holes.append(
            Hole(
                inv=inv,
                severity=sev,
                root=root,
                seed=f"{symbol} → {child}",
                detail=(
                    f"Trace({symbol}) hop-1 callees has {child} @ {c.get('file')}:{c.get('line')}, "
                    f"but Trace({child}) hop-1 callers missing {symbol}. "
                    f"child_in_deg={in_deg_i} omitted={omitted_i} "
                    f"child_callers={sorted(cnames)[:15]}. "
                    f"Product fix: re-Trace({child}) with focus_symbol={symbol} "
                    f"(injects real CALL parent into sample; Soft I4 is pack-omit)."
                ),
                receipt=f"{rec.get('confidence')}|{rec.get('edges')}",
                star=star_str(st2),
                evidence={
                    "parent": symbol,
                    "child": child,
                    "child_callers": list(cnames)[:20],
                    "child_seed_in_degree": in_deg_i,
                    "callers_omitted": omitted_i,
                    "parent_seed_file": seed_file,
                    "child_scope_pin": child_scope,
                },
            )
        )
    return holes


def check_pin_shift(
    base: str,
    project: str,
    root: str,
    symbol: str,
    content: str,
    st: dict[str, Any],
) -> list[Hole]:
    """I9: two different location pins ⇒ different ★ file when multi-alt."""
    holes: list[Hole] = []
    if not (content.startswith("Disambiguate") or st.get("blast_domain") == "disambiguate"):
        return holes
    locs = st.get("locations") or []
    files = []
    for loc in locs:
        f = (loc.get("file") or "").replace("\\", "/")
        if f and f not in files:
            files.append(f)
    if len(files) < 2:
        return holes
    stars = []
    pins_used: list[Optional[list[str]]] = []
    for f in files[:2]:
        # Repo-relative pin (never last-N of absolute host path).
        pin = file_scope_pin(project, f)
        pins_used.append(pin)
        if not pin:
            stars.append(None)
            continue
        d, c, st2, _, err = trace(base, project, symbol, pin)
        if err or c.startswith("Disambiguate") or c.startswith("Orchestrate error"):
            stars.append(None)
            continue
        bd = (st2.get("blast_domain") or "") or ""
        if bd in ("scope_not_found",) or st2.get("error") or not (st2.get("target") or {}):
            stars.append(None)
            continue
        t = st2.get("target") or {}
        stars.append((t.get("file") or "", t.get("line"), t.get("name")))
    if stars[0] and stars[1]:
        p0 = stars[0][0].replace("\\", "/")
        p1 = stars[1][0].replace("\\", "/")
        if p0 and p1 and p0 == p1:
            holes.append(
                Hole(
                    inv="I9_pin_no_shift",
                    severity="spectacular",
                    root=root,
                    seed=symbol,
                    detail=f"two pins from disambiguate resolved to same ★ {p0}:{stars[0][1]}",
                    evidence={
                        "pins": files[:2],
                        "scope_pins": pins_used,
                        "stars": stars,
                    },
                )
            )
    return holes


def sample_and_probe(
    base: str,
    root: str,
    max_seeds: int,
    bi_budget: int,
    verbose: bool,
    force_seeds: Optional[list[str]] = None,
) -> tuple[list[Hole], dict[str, Any]]:
    project = host_project_alias(root)
    holes: list[Hole] = []
    stats: dict[str, Any] = {
        "root": root,
        "project_client": project,
        "traces": 0,
        "seeds": [],
        "ms_total": 0.0,
    }

    scope = pick_scope(base, project)
    stats["scope"] = scope
    if verbose:
        print(f"  scope={scope}")

    # Arch for hubs
    d, ms, err = post_context(
        base,
        {"project": project, "goal": "architecture", "scope_paths": scope, "detail": "compact"},
    )
    stats["ms_total"] += ms
    hubs = extract_hub_names(content_text(d)) if not err else []
    if verbose:
        print(f"  hubs/sample names: {hubs[:12]}")

    seed_q: list[tuple[str, Optional[list[str]]]] = []
    # Forced seeds first (known regressions / operator interest).
    # Prefer bare + wide scopes so pin does not hide reverse CALL parents.
    wide = []
    for s in (scope, ["cli/"], ["src/"], ["src/click/"], None):
        if s not in wide:
            wide.append(s)
    for fs in force_seeds or []:
        fs = fs.strip()
        if not fs:
            continue
        for sc in wide:
            seed_q.append((fs, sc))
    for h in hubs:
        seed_q.append((h, scope))
    for bait in HOMONYM_BAIT:
        seed_q.append((bait, None))  # bare first

    seen_seed: set[str] = set()
    bi_left = [bi_budget]
    expanded = 0

    while seed_q and len(stats["seeds"]) < max_seeds:
        symbol, sc = seed_q.pop(0)
        key = f"{symbol}|{sc}"
        if key in seen_seed:
            continue
        seen_seed.add(key)

        d, content, st, ms, err = trace(base, project, symbol, sc)
        stats["ms_total"] += ms
        stats["traces"] += 1
        stats["seeds"].append(symbol)
        if verbose:
            head = content.splitlines()[0][:100] if content else err
            print(f"  trace {symbol!r} scope={sc} {ms:.0f}ms | {head}")

        if err:
            holes.append(
                Hole(
                    inv="I_http",
                    severity="soft",
                    root=root,
                    seed=symbol,
                    detail=err,
                )
            )
            continue

        holes.extend(check_invariants(root, symbol, content, st, sc))

        # disambiguate pin shift
        if content.startswith("Disambiguate") or st.get("blast_domain") == "disambiguate":
            holes.extend(check_pin_shift(base, project, root, symbol, content, st))
            # also queue preferred location as scoped seed
            for loc in (st.get("locations") or [])[:2]:
                f = loc.get("file") or ""
                if f:
                    parts = [p for p in f.replace("\\", "/").split("/") if p]
                    pin = ["/".join(parts[-3:])]
                    seed_q.append((symbol, pin))
            continue

        if content.startswith("Orchestrate error"):
            continue

        # expand BFS from neighbors
        if expanded < max_seeds:
            for n in hop1(st.get("callers") or [])[:3] + hop1(st.get("callees") or [])[:3]:
                nm = n.get("name")
                if nm and nm not in seen_seed:
                    seed_q.append((nm, scope))
            expanded += 1

        # bidirectional checks (budgeted)
        holes.extend(check_bidirectional(base, project, root, symbol, st, sc or scope, bi_left))

    return holes, stats


def main() -> int:
    ap = argparse.ArgumentParser(description="Butler accuracy hole probe")
    ap.add_argument("--base", default=DEFAULT_BASE)
    ap.add_argument(
        "--roots",
        default="",
        help="Comma list of roots (default: all health loaded edges_complete)",
    )
    ap.add_argument("--max-seeds", type=int, default=24, help="Max seeds per root")
    ap.add_argument("--bi-budget", type=int, default=8, help="Max reverse-asymmetry child traces per root")
    ap.add_argument(
        "--force-seeds",
        default="",
        help="Comma symbols always traced first (scoped) — e.g. handle_orchestrate,Command",
    )
    ap.add_argument("--json", default="", help="Write full report JSON")
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument(
        "--include-soft-exit",
        action="store_true",
        help="Exit 1 on soft holes too (default: only spectacular)",
    )
    args = ap.parse_args()

    print(f"Butler hole probe  base={args.base}")
    try:
        health = get_json(f"{args.base}/mcp/health")
    except Exception as e:
        print(f"FAIL health: {e}", file=sys.stderr)
        return 2

    if args.roots.strip():
        roots = [(r.strip(), {}) for r in args.roots.split(",") if r.strip()]
    else:
        roots = complete_roots(health)
        # Prefer keepers first if present
        prefer = [
            "/projects/lambda-wisperer",
            "/projects/test_repos/click",
            "/projects/lambda-eve",
            "/projects/Lambda-xi-rust",
        ]
        order = {p: i for i, p in enumerate(prefer)}
        roots = sorted(roots, key=lambda x: order.get(x[0], 100))

    if not roots:
        print("No edges_complete ready roots in health.loaded")
        return 2

    print("Roots:")
    for r, meta in roots:
        print(f"  {r}  {meta}")

    all_holes: list[Hole] = []
    all_stats: list[dict[str, Any]] = []

    force = [s.strip() for s in args.force_seeds.split(",") if s.strip()]

    for root, meta in roots:
        print(f"\n=== PROBE {root} ===")
        holes, stats = sample_and_probe(
            args.base,
            root,
            args.max_seeds,
            args.bi_budget,
            args.verbose,
            force_seeds=force,
        )
        stats["nodes"] = meta.get("nodes")
        all_stats.append(stats)
        all_holes.extend(holes)
        spec = [h for h in holes if h.severity == "spectacular"]
        soft = [h for h in holes if h.severity == "soft"]
        print(f"  traces={stats['traces']} seeds={len(stats['seeds'])} ms={stats['ms_total']:.0f}")
        print(f"  holes spectacular={len(spec)} soft={len(soft)}")

    # Dedupe repeated forced-scope traces
    deduped: list[Hole] = []
    seen_h: set[tuple[str, str, str, str]] = set()
    for h in all_holes:
        key = (h.inv, h.root, h.seed, h.star)
        if key in seen_h:
            continue
        seen_h.add(key)
        deduped.append(h)
    all_holes = deduped

    # Group report
    print("\n" + "=" * 72)
    print("HOLE REPORT")
    print("=" * 72)
    by_inv: dict[str, list[Hole]] = defaultdict(list)
    for h in all_holes:
        by_inv[h.inv].append(h)

    for inv in sorted(by_inv.keys()):
        group = by_inv[inv]
        spec_n = sum(1 for h in group if h.severity == "spectacular")
        print(f"\n## {inv}  (n={len(group)}, spectacular={spec_n})")
        # show up to 5 examples, prefer spectacular
        group_sorted = sorted(group, key=lambda h: (0 if h.severity == "spectacular" else 1))
        for h in group_sorted[:5]:
            print(f"  [{h.severity}] root={h.root}")
            print(f"    seed={h.seed}")
            print(f"    ★ {h.star}")
            print(f"    receipt={h.receipt}")
            print(f"    {h.detail}")

    spectacular = [h for h in all_holes if h.severity == "spectacular"]
    soft = [h for h in all_holes if h.severity == "soft"]
    print("\n" + "=" * 72)
    print(f"TOTAL spectacular={len(spectacular)}  soft={len(soft)}  traces={sum(s['traces'] for s in all_stats)}")
    print("=" * 72)

    report = {
        "base": args.base,
        "stats": all_stats,
        "holes": [asdict(h) for h in all_holes],
        "summary": {
            "spectacular": len(spectacular),
            "soft": len(soft),
            "by_inv": {k: len(v) for k, v in by_inv.items()},
        },
    }
    out_path = args.json or "/tmp/butler_hole_probe.json"
    Path(out_path).write_text(json.dumps(report, indent=2))
    print(f"Wrote {out_path}")

    if spectacular:
        return 1
    if args.include_soft_exit and soft:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
