#!/usr/bin/env python3
"""Butler FFI / interconnect boundary hole probe.

Property checks on *known* dual-stack gold pairs (Export · IPC), not random hubs.
Spectacular = receipt high + edges complete + invariant false.

Gold pairs (STACK_STATUS Track P.exit):
  Export · PyO3   pyo3/examples/word-count     search_py ↔ search
  Export · pybind pybind11                     test_function_overloading ↔ test_function1
  IPC · Tauri     tauri/examples/api           log_operation ← Communication_default

Negative (must NOT invent FFI):
  fastapi-ts — REST is not Tauri IPC / Export

Usage:
  python3 scripts/butler_ffi_hole_probe.py
  python3 scripts/butler_ffi_hole_probe.py --base http://127.0.0.1:8002 --json /tmp/ffi_holes.json -v

Exit 0 if no spectacular holes; 1 if any spectacular; 2 if no gold roots warm.
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


# relation stamps Butler paints on typed bridges (P.4)
BRIDGE_RELS = frozenset({"export", "ipc", "twin", "ffi"})


@dataclass
class GoldCase:
    """One dual-stack (or negative) probe case — type exemplar, not repo lock-in."""

    name: str  # short label
    # host path preferred; container alias applied at runtime
    project: str
    scope_paths: Optional[list[str]]
    # primary seed → expected peer names on bridge_callers or bridge_callees
    seed: str
    expect_peers: list[str]
    expect_relation: str  # export | ipc | twin
    # if True, reverse Trace(peer) must list seed
    reverse: bool
    # known dual-stack: high+complete + zero bridges = spectacular
    expect_bridge: bool = True
    # negative: any bridge is spectacular when high+complete
    negative: bool = False
    # optional scope for reverse peer (same-name IPC: ts vs rust)
    peer_scope_paths: Optional[list[str]] = None
    # if True, any one of expect_peers is enough (I-F1)
    expect_any_peer: bool = False


# Type exemplars (Export / Ipc / negative). Paths are fixtures, not product exclusivity.
GOLD: list[GoldCase] = [
    # --- Export · PyO3 ---
    GoldCase(
        name="export-pyo3-search_py→search",
        project=_hp("test_repos/pyo3/examples/word-count"),
        scope_paths=None,
        seed="search_py",
        expect_peers=["search"],
        expect_relation="export",
        reverse=True,
    ),
    GoldCase(
        name="export-pyo3-search→search_py",
        project=_hp("test_repos/pyo3/examples/word-count"),
        scope_paths=None,
        seed="search",
        expect_peers=["search_py"],
        expect_relation="export",
        reverse=True,
    ),
    # Rust-only pyfunctions (no py wrapper) — must stay silent (not invent peers).
    GoldCase(
        name="export-pyo3-search_sequential-silent",
        project=_hp("test_repos/pyo3/examples/word-count"),
        scope_paths=None,
        seed="search_sequential",
        expect_peers=[],
        expect_relation="export",
        reverse=False,
        expect_bridge=False,
        negative=False,
    ),
    # --- Export · pybind ---
    GoldCase(
        name="export-pybind-overloading→function1",
        project=_hp("test_repos/pybind11"),
        scope_paths=["tests/"],
        seed="test_function_overloading",
        expect_peers=["test_function1"],
        expect_relation="export",
        reverse=True,
    ),
    GoldCase(
        name="export-pybind-function1→overloading",
        project=_hp("test_repos/pybind11"),
        scope_paths=["tests/"],
        seed="test_function1",
        expect_peers=["test_function_overloading"],
        expect_relation="export",
        reverse=True,
    ),
    GoldCase(
        name="export-pybind-return_bytes←test_bytes",
        project=_hp("test_repos/pybind11"),
        scope_paths=["tests/"],
        seed="return_bytes",
        expect_peers=["test_bytes"],
        expect_relation="export",
        reverse=True,
        peer_scope_paths=["tests/test_constants_and_functions.py"],
    ),
    # Python test_bytes must not Export to TEST_SUBMODULE macro junk.
    # Preferred location (constants_and_functions) — real m.def peers.
    GoldCase(
        name="export-pybind-test_bytes-constants-ok",
        project=_hp("test_repos/pybind11"),
        scope_paths=["tests/test_constants_and_functions.py"],
        seed="test_bytes",
        expect_peers=["return_bytes", "print_bytes"],
        expect_relation="export",
        reverse=False,
        expect_bridge=True,
        expect_any_peer=True,
    ),
    # Homonym seed prefers test_pytypes.py — must not Export to TEST_SUBMODULE (silence OK).
    GoldCase(
        name="export-pybind-test_bytes-pytypes-no-macro",
        project=_hp("test_repos/pybind11"),
        scope_paths=["tests/"],
        seed="test_bytes",
        expect_peers=[],
        expect_relation="export",
        reverse=False,
        expect_bridge=False,
    ),
    # --- IPC · Tauri (api example) ---
    GoldCase(
        name="ipc-tauri-log_operation←log",
        project=_hp("test_repos/tauri/examples/api"),
        scope_paths=None,
        seed="log_operation",
        expect_peers=["log"],
        expect_relation="ipc",
        reverse=True,
        peer_scope_paths=["src/views/"],
    ),
    GoldCase(
        name="ipc-tauri-log→log_operation",
        project=_hp("test_repos/tauri/examples/api"),
        scope_paths=["src/views/"],
        seed="log",
        expect_peers=["log_operation"],
        expect_relation="ipc",
        reverse=True,
        peer_scope_paths=["src-tauri/"],
    ),
    GoldCase(
        name="ipc-tauri-perform_request←performRequest",
        project=_hp("test_repos/tauri/examples/api"),
        scope_paths=None,
        seed="perform_request",
        expect_peers=["performRequest"],
        expect_relation="ipc",
        reverse=True,
        peer_scope_paths=["src/views/"],
    ),
    GoldCase(
        name="ipc-tauri-performRequest→perform_request",
        project=_hp("test_repos/tauri/examples/api"),
        scope_paths=["src/views/"],
        seed="performRequest",
        expect_peers=["perform_request"],
        expect_relation="ipc",
        reverse=True,
        peer_scope_paths=["src-tauri/"],
    ),
    GoldCase(
        name="ipc-tauri-echo-ts→rs",
        project=_hp("test_repos/tauri/examples/api"),
        scope_paths=["src/views/"],
        seed="echo",
        expect_peers=["echo"],  # same Ident, rust peer
        expect_relation="ipc",
        reverse=True,
        peer_scope_paths=["src-tauri/"],
    ),
    GoldCase(
        name="ipc-tauri-spam-ts→rs",
        project=_hp("test_repos/tauri/examples/api"),
        scope_paths=["src/views/"],
        seed="spam",
        expect_peers=["spam"],
        expect_relation="ipc",
        reverse=True,
        peer_scope_paths=["src-tauri/"],
    ),
    # Negative: REST must not mint Export/Ipc under high trust.
    GoldCase(
        name="neg-fastapi-no-false-ffi",
        project=_hp("test_repos/fastapi-ts"),
        scope_paths=["scripts/"],
        seed="main",
        expect_peers=[],
        expect_relation="export",
        reverse=False,
        expect_bridge=False,
        negative=True,
    ),
    # Same-lang pure rust — AC off: no twin/export invent (P1 negative).
    GoldCase(
        name="neg-fd-main-no-false-ffi",
        project=_hp("test_repos/fd"),
        scope_paths=None,
        seed="main",
        expect_peers=[],
        expect_relation="export",
        reverse=False,
        expect_bridge=False,
        negative=True,
    ),
    GoldCase(
        name="neg-bat-main-no-false-ffi",
        project=_hp("test_repos/bat"),
        scope_paths=["src/bin/bat/"],
        seed="main",
        expect_peers=[],
        expect_relation="export",
        reverse=False,
        expect_bridge=False,
        negative=True,
    ),
]

# Junk export targets (macro/module shells) — never valid dual-stack peers.
JUNK_BRIDGE_PEERS = frozenset(
    {
        "TEST_SUBMODULE",
        "PYBIND11_MODULE",
        "unknown",
        "default",
        "mod",
        "module",
    }
)


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
    req = urllib.request.Request(url)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode())


def post_context(
    base: str, payload: dict[str, Any], timeout: float = 90.0
) -> tuple[dict[str, Any], float, Optional[str]]:
    url = f"{base}/context"
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        url, data=data, headers={"Content-Type": "application/json"}
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
    conf = r.get("confidence") or (st.get("state") or {}).get("confidence") or ""
    if isinstance(conf, str):
        conf = conf.lower()
    edges = r.get("edges") or ""
    if not edges:
        m = re.search(r"edges[=:]?\s*(complete|partial|building)", content, re.I)
        edges = m.group(1).lower() if m else ""
    if not conf:
        m = re.search(r"receipt:\s*(\w+)", content, re.I)
        conf = m.group(1).lower() if m else ""
    ladder = ""
    if isinstance(st.get("state"), dict):
        ladder = (st["state"].get("confidence") or "") or ""
    tel = st.get("telemetry") if isinstance(st.get("telemetry"), dict) else {}
    if tel.get("edges_complete") is True:
        edges = edges or "complete"
    return {
        "confidence": str(conf or ""),
        "edges": str(edges or ""),
        "ladder": str(ladder or ""),
        "basis": str((r.get("basis") or "")),
    }


def is_spectacular(rec: dict[str, str]) -> bool:
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
    return bool(high and complete)


def star_str(st: dict[str, Any]) -> str:
    t = st.get("target") or {}
    if not t:
        return ""
    return (
        f"{t.get('name')} @ {t.get('file')}:{t.get('line')} "
        f"kind={st.get('seed_kind') or t.get('kind') or '?'}"
    )


def all_bridges(st: dict[str, Any]) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for k in ("bridge_callers", "bridge_callees"):
        for n in st.get(k) or []:
            if isinstance(n, dict):
                out.append(n)
    return out


def bridge_names(st: dict[str, Any]) -> set[str]:
    return {n.get("name", "") for n in all_bridges(st) if n.get("name")}


def bridge_relations(st: dict[str, Any]) -> set[str]:
    rels = set()
    for n in all_bridges(st):
        r = (n.get("relation") or "").lower()
        if r:
            rels.add(r)
    return rels


def call_names_cross_lang(st: dict[str, Any], seed_lang: str) -> list[dict[str, Any]]:
    """CALL neighbors whose lang differs from seed (suspicious if no bridge stamp)."""
    seed_lang = (seed_lang or "").lower()
    bad = []
    for side in ("callers", "callees"):
        for n in st.get(side) or []:
            lang = (n.get("lang") or "").lower()
            if not lang or not seed_lang:
                continue
            # normalize rust/python/cpp/ts families lightly
            if lang != seed_lang and lang[:2] != seed_lang[:2]:
                # ignore same family (typescript vs ts)
                if {lang, seed_lang} <= {"typescript", "javascript", "ts", "js", "svelte"}:
                    continue
                if {lang, seed_lang} <= {"c", "cpp", "c++"}:
                    continue
                bad.append(n)
    return bad


def lang_of_seed(st: dict[str, Any]) -> str:
    t = st.get("target") or {}
    if t.get("lang"):
        return str(t["lang"])
    locs = st.get("locations") or []
    if locs and locs[0].get("lang"):
        return str(locs[0]["lang"])
    # cluster core:rs → rust
    cl = (st.get("active_cluster") or "") or ""
    if ":" in cl:
        return cl.split(":")[-1]
    return ""


def loaded_complete(health: dict[str, Any]) -> dict[str, dict[str, Any]]:
    out = {}
    for k, v in (health.get("loaded") or {}).items():
        if isinstance(v, dict) and v.get("edges_complete") and v.get("ready"):
            out[k] = v
    return out


def project_available(project: str, complete: dict[str, dict[str, Any]]) -> Optional[str]:
    """Return host project path if warehouse is Complete (host or container key)."""
    cont = host_to_container(project)
    host = container_to_host(project)
    for key in (cont, host, project):
        if key in complete:
            # always pass host path to /context (server translates)
            return host if host.startswith("/home/") else project
    # prefix match (word-count nested)
    for k in complete:
        if k.rstrip("/").endswith(project.rstrip("/").split("/")[-2] + "/" + project.rstrip("/").split("/")[-1]) or k.rstrip("/").endswith(
            project.rstrip("/").split("/")[-1]
        ):
            return container_to_host(k) if k.startswith("/projects") else k
    return None


def trace(
    base: str,
    project: str,
    symbol: str,
    scope: Optional[list[str]] = None,
    detail: str = "dense",
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


def check_case(
    base: str,
    case: GoldCase,
    project: str,
    verbose: bool,
) -> list[Hole]:
    holes: list[Hole] = []
    root = host_to_container(project)

    def hole(
        inv: str,
        detail: str,
        rec: dict[str, str],
        st: dict[str, Any],
        force_sev: Optional[str] = None,
        evidence: Optional[dict] = None,
    ) -> None:
        sev = force_sev or ("spectacular" if is_spectacular(rec) else "soft")
        # void / incomplete never spectacular
        content_l = (evidence or {}).get("content_head", "")
        if "lang void" in content_l.lower() or "Warehouse lang void" in detail:
            sev = "soft"
        holes.append(
            Hole(
                inv=inv,
                severity=sev,
                root=root,
                seed=f"{case.name}:{case.seed}",
                detail=detail,
                receipt=f"{rec.get('confidence')}|{rec.get('basis')}|{rec.get('edges')}",
                star=star_str(st),
                evidence=evidence or {},
            )
        )

    d, content, st, ms, err = trace(base, project, case.seed, case.scope_paths)
    if verbose:
        print(f"  trace {case.seed!r} {ms:.0f}ms | {content[:100].replace(chr(10), ' ')}")

    if err:
        hole(
            "I_F0_request_error",
            f"request error: {err}",
            {},
            st,
            force_sev="soft",
            evidence={"err": err},
        )
        return holes

    if "lang void" in content.lower() or (st.get("telemetry") or {}).get("lang_void"):
        hole(
            "I_F0_lang_void",
            f"root is lang void — FFI gold cannot run here: {content[:160]}",
            receipt_bits(st, content),
            st,
            force_sev="soft",
            evidence={"content_head": content[:200]},
        )
        return holes

    if content.startswith("Orchestrate error") or content.startswith("Disambiguate") or st.get(
        "blast_domain"
    ) == "disambiguate":
        # Disambiguate on gold seed is soft (homonym); miss is soft unless high complete lie
        rec = receipt_bits(st, content)
        if content.startswith("Disambiguate") or st.get("blast_domain") == "disambiguate":
            hole(
                "I_F0_disambiguate_gold",
                f"gold seed {case.seed!r} returned disambiguate — pin scope",
                rec,
                st,
                force_sev="soft",
                evidence={"content_head": content[:200]},
            )
        else:
            hole(
                "I_F0_symbol_miss",
                f"gold seed {case.seed!r} miss: {content[:160]}",
                rec,
                st,
                force_sev="soft" if not is_spectacular(rec) else "spectacular",
                evidence={"content_head": content[:200]},
            )
        return holes

    if not (st.get("target") or {}):
        hole(
            "I_F0_empty_star",
            f"no ★ for gold seed {case.seed!r}",
            receipt_bits(st, content),
            st,
            force_sev="soft",
        )
        return holes

    rec = receipt_bits(st, content)
    bridges = all_bridges(st)
    bnames = bridge_names(st)
    brels = bridge_relations(st)
    seed_lang = lang_of_seed(st)

    # --- I-F5 / negative: bridge presence policy ---
    if case.negative:
        typed = [n for n in bridges if (n.get("relation") or "").lower() in BRIDGE_RELS]
        if typed and is_spectacular(rec):
            hole(
                "I_F4_false_ffi",
                f"negative case {case.name}: high+complete Trace minted typed bridge(s) "
                f"{[(n.get('name'), n.get('relation')) for n in typed[:6]]} — silence > invent",
                rec,
                st,
                force_sev="spectacular",
                evidence={
                    "bridges": [
                        {"name": n.get("name"), "relation": n.get("relation"), "lang": n.get("lang")}
                        for n in typed[:10]
                    ]
                },
            )
        return holes

    # Silence-ok seeds (rust pyfunction without py wrapper): bridges must stay empty
    # or at least never point at junk macro hosts.
    if not case.expect_bridge and not case.expect_peers:
        junk_hit = sorted(bnames & JUNK_BRIDGE_PEERS)
        if junk_hit and is_spectacular(rec):
            hole(
                "I_F4_junk_bridge_peer",
                f"Trace({case.seed}) bridges to junk/macro peer(s) {junk_hit}",
                rec,
                st,
                force_sev="spectacular",
                evidence={"junk": junk_hit, "bridges": sorted(bnames)[:15]},
            )
        elif bridges and is_spectacular(rec):
            hole(
                "I_F4_unexpected_bridge",
                f"seed {case.seed!r} should be silent (no dual wrapper) but bridges="
                f"{sorted(bnames)[:12]}",
                rec,
                st,
                force_sev="soft",
                evidence={"bridges": sorted(bnames)[:15]},
            )
        return holes

    if case.expect_bridge and not bridges:
        # I-F5: known dual-stack empty bridges under high trust
        hole(
            "I_F5_missing_bridge",
            f"known dual-stack seed {case.seed!r} has 0 bridge_callers/callees "
            f"(expect peers {case.expect_peers} relation={case.expect_relation})",
            rec,
            st,
            force_sev="spectacular" if is_spectacular(rec) else "soft",
            evidence={
                "expect_peers": case.expect_peers,
                "expect_relation": case.expect_relation,
                "callers": [n.get("name") for n in (st.get("callers") or [])[:8]],
                "callees": [n.get("name") for n in (st.get("callees") or [])[:8]],
            },
        )
        return holes

    # --- I-F4 junk peer (TEST_SUBMODULE / macro shells) ---
    junk_hit = sorted(bnames & JUNK_BRIDGE_PEERS)
    if junk_hit and is_spectacular(rec):
        hole(
            "I_F4_junk_bridge_peer",
            f"Trace({case.seed}) bridges to junk/macro peer(s) {junk_hit} "
            f"(silence > invent / wrong export target)",
            rec,
            st,
            force_sev="spectacular",
            evidence={
                "junk": junk_hit,
                "bridges": [
                    {"name": n.get("name"), "relation": n.get("relation"), "lang": n.get("lang")}
                    for n in bridges[:12]
                ],
            },
        )

    # --- I-F1: expected peer on bridge lists ---
    if case.expect_any_peer and case.expect_peers:
        if not any(p in bnames for p in case.expect_peers):
            hole(
                "I_F1_peer_missing",
                f"Trace({case.seed}) bridges={sorted(bnames)} missing any of {case.expect_peers}",
                rec,
                st,
                force_sev="spectacular" if is_spectacular(rec) else "soft",
                evidence={
                    "bridges": [
                        {
                            "name": n.get("name"),
                            "relation": n.get("relation"),
                            "lang": n.get("lang"),
                        }
                        for n in bridges[:12]
                    ],
                    "expect_any": case.expect_peers,
                },
            )
        missing_peers = []
    else:
        missing_peers = [p for p in case.expect_peers if p not in bnames]
        if missing_peers:
            hole(
                "I_F1_peer_missing",
                f"Trace({case.seed}) bridges={sorted(bnames)} missing expected peer(s) {missing_peers}",
                rec,
                st,
                force_sev="spectacular" if is_spectacular(rec) else "soft",
                evidence={
                    "bridges": [
                        {
                            "name": n.get("name"),
                            "relation": n.get("relation"),
                            "lang": n.get("lang"),
                        }
                        for n in bridges[:12]
                    ],
                    "missing": missing_peers,
                },
            )

    # --- I-F3: relation stamp ---
    if bridges and case.expect_relation:
        if case.expect_relation.lower() not in brels and not brels.intersection(BRIDGE_RELS):
            hole(
                "I_F3_relation_stamp",
                f"bridges present but no typed relation (got {sorted(brels) or ['(none)']}; "
                f"want {case.expect_relation})",
                rec,
                st,
                force_sev="spectacular" if is_spectacular(rec) else "soft",
                evidence={"relations": sorted(brels), "expect": case.expect_relation},
            )
        elif case.expect_relation.lower() not in brels:
            # has some bridge rel but wrong kind
            hole(
                "I_F3_relation_mismatch",
                f"expected relation={case.expect_relation}, got {sorted(brels)}",
                rec,
                st,
                force_sev="soft",
                evidence={"relations": sorted(brels)},
            )

    # --- I-F3b: cross-lang CALL without bridge (lie direction) ---
    cross = call_names_cross_lang(st, seed_lang)
    if cross and is_spectacular(rec):
        # soft if we also have proper bridges (CALL noise); spectacular if ONLY cross CALL
        if not bridges:
            hole(
                "I_F3_cross_lang_call_no_bridge",
                f"cross-lang CALL neighbors without bridges: "
                f"{[(n.get('name'), n.get('lang')) for n in cross[:6]]}",
                rec,
                st,
                force_sev="spectacular",
                evidence={
                    "cross": [
                        {"name": n.get("name"), "lang": n.get("lang"), "file": n.get("file")}
                        for n in cross[:10]
                    ]
                },
            )
        else:
            hole(
                "I_F3_cross_lang_call_alongside_bridge",
                f"cross-lang CALL present alongside bridges (prefer bridge-only paint): "
                f"{[(n.get('name'), n.get('lang')) for n in cross[:4]]}",
                rec,
                st,
                force_sev="soft",
            )

    # --- I-F2: reverse asymmetry ---
    if case.reverse and case.expect_peers:
        peers_to_check = (
            [p for p in case.expect_peers if p in bnames]
            if case.expect_any_peer
            else list(case.expect_peers)
        )
        for peer in peers_to_check:
            if peer not in bnames and missing_peers:
                continue  # already failed F1
            peer_scope = case.peer_scope_paths or case.scope_paths
            d2, c2, st2, ms2, err2 = trace(base, project, peer, peer_scope)
            if verbose:
                print(f"    reverse {peer!r} scope={peer_scope} {ms2:.0f}ms | {c2[:80].replace(chr(10), ' ')}")
            if err2 or c2.startswith("Orchestrate error") or c2.startswith("Disambiguate"):
                hole(
                    "I_F2_reverse_untraceable",
                    f"Trace({case.seed}) lists bridge peer {peer!r} but reverse Trace failed: {c2[:120]}",
                    receipt_bits(st2, c2),
                    st2,
                    force_sev="soft",
                    evidence={"peer": peer, "content_head": c2[:200]},
                )
                continue
            if not (st2.get("target") or {}):
                continue
            rec2 = receipt_bits(st2, c2)
            rev_names = bridge_names(st2)
            # Same-name IPC: seed and peer share Ident — reverse is OK if seed appears OR
            # peer star is the other-lang definition with us in bridge_callers.
            seed_ok = case.seed in rev_names
            if not seed_ok and case.seed == peer:
                # same Ident both sides: any bridge back to our file/lang is enough
                seed_ok = bool(rev_names) or bool(all_bridges(st2))
            if not seed_ok:
                sev = "spectacular" if is_spectacular(rec2) and is_spectacular(rec) else "soft"
                hole(
                    "I_F2_reverse_asymmetry",
                    f"Trace({case.seed}) bridges include {peer!r}, but Trace({peer}) "
                    f"bridge lists missing {case.seed!r} (got {sorted(rev_names)[:12]})",
                    rec2,
                    st2,
                    force_sev=sev,
                    evidence={
                        "parent": case.seed,
                        "peer": peer,
                        "peer_scope": peer_scope,
                        "peer_bridges": sorted(rev_names)[:20],
                        "parent_bridges": sorted(bnames)[:20],
                    },
                )

    return holes


def main() -> int:
    ap = argparse.ArgumentParser(description="Butler FFI / interconnect hole probe")
    ap.add_argument("--base", default=DEFAULT_BASE)
    ap.add_argument(
        "--json",
        default="/tmp/butler_ffi_hole_probe.json",
        help="Write full report JSON",
    )
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument(
        "--include-soft-exit",
        action="store_true",
        help="Exit 1 on soft holes too",
    )
    ap.add_argument(
        "--cases",
        default="",
        help="Comma substring filter on case name (default: all)",
    )
    ap.add_argument(
        "--nearest-root",
        action="store_true",
        default=True,
        help="Also check word-count package root self-heal (P2)",
    )
    ap.add_argument(
        "--no-nearest-root",
        action="store_true",
        help="Skip nearest-root dual-stack check",
    )
    args = ap.parse_args()

    print(f"Butler FFI hole probe  base={args.base}")
    print(
        "Note: Twin AC invent smoke needs server BUTLER_POLYGLOT_AC=1 "
        "(default OFF — neg-fd/bat cases assert no invent with AC off)."
    )
    try:
        health = get_json(f"{args.base}/mcp/health")
    except Exception as e:
        print(f"health failed: {e}")
        return 2

    complete = loaded_complete(health)
    print(f"Complete warehouses: {len(complete)}")
    for k, v in sorted(complete.items(), key=lambda x: -x[1].get("nodes", 0))[:12]:
        print(f"  {k}  nodes={v.get('nodes')}")

    filt = [s.strip() for s in args.cases.split(",") if s.strip()]
    cases = GOLD
    if filt:
        cases = [c for c in GOLD if any(f in c.name for f in filt)]

    all_holes: list[Hole] = []
    stats: list[dict[str, Any]] = []
    skipped: list[str] = []

    # P2: package-root self-heal — open word-count via parent pyo3 path must still ★ under word-count/
    if args.nearest_root and not args.no_nearest_root:
        wc = project_available(
            _hp("test_repos/pyo3/examples/word-count"), complete
        )
        if wc:
            print("\n=== P2 nearest-root dual-stack (word-count package)")
            d, c, st, ms, err = trace(args.base, wc, "search_py", None)
            rec = receipt_bits(st, c)
            t = st.get("target") or {}
            f = (t.get("file") or "").replace("\\", "/")
            print(f"  search_py {ms:.0f}ms ★={f}")
            if err:
                print(f"  skip: {err}")
            elif "word-count" not in f and is_spectacular(rec) and t:
                all_holes.append(
                    Hole(
                        inv="I_P2_nearest_root",
                        severity="spectacular",
                        root=host_to_container(wc),
                        seed="nearest-root:search_py",
                        detail=(
                            f"word-count package Trace(search_py) ★ outside word-count/: {f}"
                        ),
                        receipt=f"{rec.get('confidence')}|{rec.get('basis')}|{rec.get('edges')}",
                        star=star_str(st),
                        evidence={"file": f},
                    )
                )
                print("  hole: ★ left package root")
            else:
                print("  ok: ★ under word-count package")
            stats.append({"case": "nearest-root-word-count", "ms": ms, "holes": 0})

    for case in cases:
        proj = project_available(case.project, complete)
        if not proj:
            # try warm hint
            skipped.append(case.name)
            print(f"\n=== SKIP {case.name} (not Complete): {case.project}")
            continue
        print(f"\n=== CASE {case.name}  project={proj} seed={case.seed}")
        t0 = time.perf_counter()
        holes = check_case(args.base, case, proj, args.verbose)
        ms = (time.perf_counter() - t0) * 1000
        stats.append(
            {
                "case": case.name,
                "project": proj,
                "seed": case.seed,
                "ms": ms,
                "holes": len(holes),
                "spectacular": sum(1 for h in holes if h.severity == "spectacular"),
            }
        )
        all_holes.extend(holes)
        spec = sum(1 for h in holes if h.severity == "spectacular")
        soft = sum(1 for h in holes if h.severity == "soft")
        print(f"  holes spectacular={spec} soft={soft} ms={ms:.0f}")

    # Report
    print("\n" + "=" * 72)
    print("FFI HOLE REPORT")
    print("=" * 72)
    if skipped:
        print(f"\nSkipped (not warm Complete): {', '.join(skipped)}")
        print("  warm e.g.: curl -sS -X POST $BUTLER/warm -H 'Content-Type: application/json' \\")
        print(
            '    -d \'{"roots":[_hp("test_repos/pyo3/examples/word-count"),'
            '_hp("test_repos/pybind11"),_hp("test_repos/tauri/examples/api")]}\''
        )

    by_inv: dict[str, list[Hole]] = defaultdict(list)
    for h in all_holes:
        by_inv[h.inv].append(h)

    for inv in sorted(by_inv.keys()):
        group = by_inv[inv]
        spec_n = sum(1 for h in group if h.severity == "spectacular")
        print(f"\n## {inv}  (n={len(group)}, spectacular={spec_n})")
        group_sorted = sorted(group, key=lambda h: (0 if h.severity == "spectacular" else 1))
        for h in group_sorted[:8]:
            print(f"  [{h.severity}] {h.seed}")
            print(f"    root={h.root}")
            print(f"    ★ {h.star}")
            print(f"    receipt={h.receipt}")
            print(f"    {h.detail}")

    spectacular = [h for h in all_holes if h.severity == "spectacular"]
    soft = [h for h in all_holes if h.severity == "soft"]
    print("\n" + "=" * 72)
    print(
        f"TOTAL spectacular={len(spectacular)}  soft={len(soft)}  "
        f"cases_run={len(stats)} skipped={len(skipped)}"
    )
    print("=" * 72)

    report = {
        "base": args.base,
        "stats": stats,
        "skipped": skipped,
        "holes": [asdict(h) for h in all_holes],
        "summary": {
            "spectacular": len(spectacular),
            "soft": len(soft),
            "cases_run": len(stats),
            "skipped": len(skipped),
            "by_inv": {k: len(v) for k, v in by_inv.items()},
        },
    }
    Path(args.json).write_text(json.dumps(report, indent=2))
    print(f"Wrote {args.json}")

    if not stats and skipped:
        return 2
    if spectacular:
        return 1
    if args.include_soft_exit and soft:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
