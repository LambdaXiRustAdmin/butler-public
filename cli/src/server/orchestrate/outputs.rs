//! BUILDING / scope-repair / error orchestrate outputs (P1c peel).
//!
//! Message + DTO assemblers only — zero intentional behavior change.
//! Re-exported from `mod.rs` so `arch` / `detail_tests` keep `super::…` paths.

use crate::server::build_status::{self, get_telemetry};
use crate::server::dto::*;
use crate::server::scope::format_scope_paths_for_error;
use crate::server::state::AppState;
use crate::vprintln;
use code_graph::CodeGraph;
use std::path::Path;

use super::disambiguate::{is_homonym_risk_name, sanitize_scope_prefix};
use super::receipt::{
    attach_trace_receipt, next_action_building, next_action_symbol_miss, set_next_action,
};
use super::OrchestrateOutput;

pub(super) fn make_state_info(
    edge_build: impl Into<String>,
    jit: impl Into<String>,
    confidence: build_status::ConfidenceRung,
    percent: usize,
) -> StateInfo {
    StateInfo {
        edge_build: edge_build.into(),
        jit: jit.into(),
        confidence: Some(confidence.as_str().to_string()),
        percent: Some(percent),
    }
}

pub(super) fn error_structured_report(
    error: &str,
    edge_build: &str,
    jit: &str,
    confidence: build_status::ConfidenceRung,
    percent: usize,
) -> StructuredReport {
    let mut st = StructuredReport {
        state: make_state_info(edge_build, jit, confidence, percent),
        error: Some(error.to_string()),
        target: None,
        callers: vec![],
        callees: vec![],
        caller_path: vec![],
        peer_callers: vec![],
        bridge_callers: vec![],
        bridge_callees: vec![],
        blast_domain: None,
        seed_kind: None,
        receipt: None,
        next_action: None,
        telemetry: serde_json::json!({
            "error": error,
            "confidence": confidence.as_str(),
            "percent": percent,
        }),
        suggested_scopes: vec![],
        skeleton: None,
        hubs: None,
        module_resolved_from: None,
        module_interior_candidates: None,
        locations: None,
        clusters: None,
        bridges: None,
        active_cluster: None,
    };
    attach_trace_receipt(&mut st);
    st
}

/// ArchitecturalSummary refuse with top-level `suggested_scopes` (no heavy work).
pub(super) fn arch_scope_refused_output(
    graph: &CodeGraph,
    root_path: &Path,
    pp: &code_graph::ProjectPaths,
    n_scoped: usize,
    total_nodes: usize,
    edge_build: &str,
    jit: &str,
    confidence: build_status::ConfidenceRung,
    percent: usize,
    why: &str,
) -> OrchestrateOutput {
    const ARCH_SCOPED_HARD: usize = 80_000;
    let mut rels: Vec<String> = if !graph.file_hashes.is_empty() {
        graph.file_hashes.keys().cloned().collect()
    } else {
        graph
            .nodes
            .values()
            .take(50_000)
            .map(|b| pp.key(&b.file))
            .collect()
    };
    rels.sort();
    rels.dedup();
    let suggested = crate::server::monorepo_scope::top_level_dir_scopes(&rels, 12);
    let examples = if suggested.is_empty() {
        "src/ or a product top-level dir".to_string()
    } else {
        suggested
            .iter()
            .take(4)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let msg = if n_scoped == 0 {
        format!(
            "ArchitecturalSummary refused ({why}): scope too broad for this warehouse \
             ({total_nodes} nodes, cap {ARCH_SCOPED_HARD}). \
             Pass narrower scope_paths (e.g. {examples})."
        )
    } else {
        format!(
            "ArchitecturalSummary refused ({why}): scope has {n_scoped} blocks (cap {ARCH_SCOPED_HARD}). \
             Pass narrower scope_paths (e.g. {examples})."
        )
    };
    vprintln!(
        "⚡ Arch scope refused ({why}): n_scoped={n_scoped} nodes={total_nodes} root={}",
        root_path.display()
    );
    let mut st = error_structured_report(&msg, edge_build, jit, confidence, percent);
    st.suggested_scopes = suggested.clone();
    st.telemetry["arch_refused"] = true.into();
    st.telemetry["block_count"] = n_scoped.into();
    st.telemetry["refuse_reason"] = why.into();
    let next = if suggested.is_empty() {
        "pass narrower scope_paths (subdir with product code) and re-run ArchitecturalSummary"
            .to_string()
    } else {
        format!(
            "pass scope_paths from suggested_scopes (e.g. {}) and re-run",
            suggested.first().map(|s| s.as_str()).unwrap_or("…")
        )
    };
    set_next_action(&mut st, next);
    // Dense content: always list machine-actionable scopes (L4.1) — agents that
    // drop structuredContent still see what to pass next.
    let content = if suggested.is_empty() {
        format!(
            "{msg}\nnext: {}",
            st.next_action.as_deref().unwrap_or("")
        )
    } else {
        format!(
            "{msg}\nsuggested_scopes: {}\nnext: {}\n",
            suggested.join(", "),
            st.next_action.as_deref().unwrap_or("")
        )
    };
    OrchestrateOutput {
        content,
        mermaid: None,
        structured: Some(st),
    }
}

pub(super) fn error_orchestrate_output(
    error: &str,
    edge_build: &str,
    jit: &str,
    confidence: build_status::ConfidenceRung,
    percent: usize,
) -> OrchestrateOutput {
    let e = error.to_string();
    let mut content = e.clone();
    if !confidence.is_full() {
        let banner = build_status::honest_partial_banner(percent, confidence, Some("provisional"));
        if !banner.is_empty() {
            content = format!("{banner}\n\n{e}");
        }
    }
    OrchestrateOutput {
        content,
        mermaid: None,
        structured: Some(error_structured_report(
            &e, edge_build, jit, confidence, percent,
        )),
    }
}

pub(super) fn no_blocks_in_scope_message(scope_paths: &Option<Vec<String>>) -> String {
    format!(
        "No blocks matched in scope {}. Ensure the scope is a valid directory and the symbol exists. next: widen scope_paths or drop ignore_paths that exclude product code",
        format_scope_paths_for_error(scope_paths)
    )
}

/// True when empty collect is from a **fat** scope (refuse), not a tiny miss/empty.
///
/// Bevy `src/` with 1 file / ~100 est nodes must be **false** (repair path).
pub(crate) fn scope_working_set_truly_too_big(
    file_hits: usize,
    est_nodes: usize,
    warehouse_nodes: usize,
) -> bool {
    const ARCH_SCOPED_HARD: usize = 80_000;
    est_nodes > ARCH_SCOPED_HARD
        || file_hits > 2_000
        || (warehouse_nodes > ARCH_SCOPED_HARD && file_hits > 400)
}

/// Scope matched inventory files but produced **0 blocks** (not "too broad").
///
/// Classic: bevy root `src/lib.rs` re-exports only — 1 file hit, empty collect → agent
/// should pin a crate (`crates/bevy_app/`), not be told the warehouse is 140k-wide.
pub(super) fn scope_empty_blocks_repair_output(
    graph: &CodeGraph,
    root_path: &Path,
    pp: &code_graph::ProjectPaths,
    scope_paths: &Option<Vec<String>>,
    file_hits: usize,
    edge_build: &str,
    jit: &str,
    confidence: build_status::ConfidenceRung,
    percent: usize,
) -> OrchestrateOutput {
    let requested = format_scope_paths_for_error(scope_paths);
    let inv: Vec<String> = if !graph.file_hashes.is_empty() {
        graph.file_hashes.keys().cloned().collect()
    } else {
        graph
            .nodes
            .values()
            .take(40_000)
            .map(|b| pp.key(&b.file))
            .collect()
    };
    let mut suggested: Vec<String> = Vec::new();
    if let Some(scopes) = scope_paths {
        for s in scopes {
            let t = s.trim();
            if t.is_empty() || t == "." {
                continue;
            }
            for pin in code_graph::snooper::suggest_scope_repairs_for_token(
                inv.iter().map(|s| s.as_str()),
                t,
                8,
            ) {
                if let Some(clean) = sanitize_scope_prefix(root_path, &pin) {
                    if !suggested.iter().any(|e| e == &clean) {
                        suggested.push(clean);
                    }
                } else if !pin.starts_with('/') && !suggested.iter().any(|e| e == &pin) {
                    suggested.push(pin);
                }
            }
        }
    }
    if suggested.is_empty() {
        suggested = crate::server::monorepo_scope::top_level_dir_scopes(&inv, 8);
    }
    suggested.retain(|s| !s.starts_with('/') && !s.starts_with("home/") && !s.contains("/home/"));
    suggested.truncate(8);

    let examples = if suggested.is_empty() {
        "a denser package path (e.g. crates/bevy_app/, django/core/)".into()
    } else {
        suggested
            .iter()
            .take(4)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let msg = format!(
        "Scope matched {file_hits} file(s) under {requested} but 0 parse blocks in the working set \
         (empty re-export crate, unparsed path, or index gap). \
         This is **not** a whole-warehouse size refuse. Try a denser pin: {examples}."
    );
    let mut st = error_structured_report(&msg, edge_build, jit, confidence, percent);
    st.suggested_scopes = suggested.clone();
    st.telemetry["scope_empty_blocks"] = true.into();
    st.telemetry["scope_file_hits"] = file_hits.into();
    st.telemetry["scope_root_anchored"] = true.into();
    st.blast_domain = Some("scope_empty_blocks".into());
    let next = if suggested.is_empty() {
        "pass a denser repo-relative scope_paths under a package with real source and re-run"
            .to_string()
    } else {
        format!(
            "update scope_paths to a suggested pin (e.g. {}) and re-run; keep project= unchanged",
            suggested.first().map(|s| s.as_str()).unwrap_or("…")
        )
    };
    set_next_action(&mut st, next);
    let content = if suggested.is_empty() {
        format!("{msg}\nnext: {}", st.next_action.as_deref().unwrap_or(""))
    } else {
        format!(
            "{msg}\nsuggested_scopes: {}\nnext: {}\n",
            suggested.join(", "),
            st.next_action.as_deref().unwrap_or("")
        )
    };
    OrchestrateOutput {
        content,
        mermaid: None,
        structured: Some(st),
    }
}

/// Root-anchored scope miss: 0 inventory hits — repair with repo-relative candidates.
///
/// `src/` means `<root>/src/**` only; nested `cli/src/` is a **different** pin.
pub(super) fn scope_not_found_repair_output(
    graph: &CodeGraph,
    root_path: &Path,
    pp: &code_graph::ProjectPaths,
    scope_paths: &Option<Vec<String>>,
    edge_build: &str,
    jit: &str,
    confidence: build_status::ConfidenceRung,
    percent: usize,
) -> OrchestrateOutput {
    let requested = format_scope_paths_for_error(scope_paths);
    let inv: Vec<String> = if !graph.file_hashes.is_empty() {
        graph.file_hashes.keys().cloned().collect()
    } else {
        graph
            .nodes
            .values()
            .take(40_000)
            .map(|b| pp.key(&b.file))
            .collect()
    };
    let mut suggested: Vec<String> = Vec::new();
    if let Some(scopes) = scope_paths {
        for s in scopes {
            let t = s.trim();
            if t.is_empty() || t == "." || t == "./" {
                continue;
            }
            for pin in code_graph::snooper::suggest_scope_repairs_for_token(
                inv.iter().map(|s| s.as_str()),
                t,
                8,
            ) {
                if let Some(clean) = sanitize_scope_prefix(root_path, &pin) {
                    if !suggested.iter().any(|e| e == &clean) {
                        suggested.push(clean);
                    }
                } else if !pin.starts_with('/') && !suggested.iter().any(|e| e == &pin) {
                    suggested.push(pin);
                }
            }
        }
    }
    if suggested.is_empty() {
        // Fall back to top-level product dirs (monorepo map).
        suggested = crate::server::monorepo_scope::top_level_dir_scopes(&inv, 8);
    }
    // Cap + ensure repo-relative
    suggested.retain(|s| {
        !s.starts_with('/')
            && !s.starts_with("home/")
            && !s.contains("/home/")
    });
    suggested.truncate(8);

    let examples = if suggested.is_empty() {
        "a repo-relative dir that exists at project root (e.g. cli/src/, django/core/)".into()
    } else {
        suggested
            .iter()
            .take(4)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let msg = format!(
        "Scope not found at project root (root-anchored): {requested}. \
         Dir scopes match only <project>/<scope>/** — not **/scope/**. \
         Suggested pins: {examples}."
    );
    let mut st = error_structured_report(&msg, edge_build, jit, confidence, percent);
    st.suggested_scopes = suggested.clone();
    st.telemetry["scope_not_found"] = true.into();
    st.telemetry["scope_root_anchored"] = true.into();
    st.blast_domain = Some("scope_not_found".into());
    // Recompute receipt after blast_domain so basis=scope_not_found (not generic error).
    attach_trace_receipt(&mut st);
    let next = if suggested.is_empty() {
        "pass a repo-relative scope_paths prefix under the project root and re-run; keep project= unchanged"
            .to_string()
    } else {
        format!(
            "update scope_paths to a suggested pin (e.g. {}) and re-run; keep project= unchanged",
            suggested.first().map(|s| s.as_str()).unwrap_or("…")
        )
    };
    set_next_action(&mut st, next);
    let content = if suggested.is_empty() {
        format!("{msg}\nnext: {}", st.next_action.as_deref().unwrap_or(""))
    } else {
        format!(
            "{msg}\nsuggested_scopes: {}\nnext: {}\n",
            suggested.join(", "),
            st.next_action.as_deref().unwrap_or("")
        )
    };
    OrchestrateOutput {
        content,
        mermaid: None,
        structured: Some(st),
    }
}

pub(super) fn symbol_not_found_message(
    symbol: &str,
    scope_paths: &Option<Vec<String>>,
    edges_complete: bool,
    percent: usize,
) -> String {
    let next = next_action_symbol_miss(symbol, edges_complete, percent);
    if !edges_complete {
        return format!(
            "symbol_not_seen_yet@{}%: Symbol '{}' not seen yet (graph {}% complete, scope {}). \
             Do not treat as missing/dead — rewalk when percent climbs. next: {next}",
            percent.min(99),
            symbol,
            percent.min(99),
            format_scope_paths_for_error(scope_paths)
        );
    }
    let homonym = if is_homonym_risk_name(symbol) {
        " Short name — high collision risk."
    } else {
        ""
    };
    format!(
        "Symbol '{}' not found in scope {}.{} next: {next}",
        symbol,
        format_scope_paths_for_error(scope_paths),
        homonym
    )
}

pub(super) fn is_live_background_build(state: &AppState, root: &str, graph: Option<&CodeGraph>) -> bool {
    build_status::is_live_build(state, root, graph)
}

#[allow(dead_code)] // HTTP/context may call without a symbol
pub(super) fn building_graph_output(
    state: &AppState,
    root: &str,
    graph: Option<&CodeGraph>,
    live: bool,
) -> OrchestrateOutput {
    building_graph_output_for_symbol(state, root, graph, live, None)
}

/// Cold/incomplete response with usable TOC + BUILDING contract (never empty adventure).
pub(super) fn building_graph_output_for_symbol(
    state: &AppState,
    root: &str,
    graph: Option<&CodeGraph>,
    live: bool,
    symbol: Option<&str>,
) -> OrchestrateOutput {
    building_graph_output_for_symbol_confirm(state, root, graph, live, symbol, false)
}

/// Cold BUILDING pack with soft-wall confirm (`confirm_long_wait` on request).
pub(super) fn building_graph_output_for_symbol_confirm(
    state: &AppState,
    root: &str,
    graph: Option<&CodeGraph>,
    live: bool,
    symbol: Option<&str>,
    confirm_long_wait: bool,
) -> OrchestrateOutput {
    let (content, percent, phase, toc, provisional, wait_json) =
        build_status::usable_building_pack_confirm(
            state,
            root,
            graph,
            live,
            symbol,
            confirm_long_wait,
        );
    let soft_wall = content.contains("BUILDING_SOFT_WALL");
    let status = if soft_wall {
        "BUILDING_SOFT_WALL"
    } else {
        "BUILDING"
    };
    let state_label = get_telemetry(state, root)
        .map(|t| format!("{:?}", t.state()))
        .or_else(|| graph.map(|g| format!("{:?}", g.background_edge_build_state)))
        .unwrap_or_else(|| "Incomplete".to_string());
    let edge_build = format!("{percent}% | {state_label}");
    let error = if soft_wall {
        format!(
            "status=BUILDING_SOFT_WALL progress={percent}% phase={phase}. Soft wall — set confirm_long_wait=true to keep polling, or abort."
        )
    } else if live {
        format!(
            "status=BUILDING progress={percent}% phase={phase}. Use toc/scope_paths; rewalk when percent climbs."
        )
    } else {
        format!(
            "status=BUILDING edges incomplete ({state_label}, {percent}%). Background build resuming."
        )
    };
    let conf = if percent >= 100 {
        build_status::ConfidenceRung::EdgesFull
    } else {
        build_status::ConfidenceRung::Inventory
    };
    let mut st = error_structured_report(&error, &edge_build, "building", conf, percent);
    st.skeleton = if toc.is_empty() {
        None
    } else {
        Some(toc.clone())
    };
    st.suggested_scopes = toc;
    let mut telemetry = serde_json::json!({
        "status": status,
        "progress": percent.min(99),
        "phase": phase,
        "live": live,
        "confidence": conf.as_str(),
        "percent": percent.min(99),
        "provisional_seed": provisional,
        "usable_while_building": !soft_wall,
        "error": error,
    });
    if let Some(wp) = wait_json.get("wait_policy") {
        telemetry["wait_policy"] = wp.clone();
    }
    st.telemetry = telemetry;
    set_next_action(&mut st, next_action_building(percent, soft_wall));
    OrchestrateOutput {
        content,
        mermaid: None,
        structured: Some(st),
    }
}

pub(super) fn edge_build_status_label(
    state: &AppState,
    root: &str,
    graph: Option<&CodeGraph>,
    live: bool,
) -> (usize, String) {
    build_status::state_label(state, root, graph, live)
}
