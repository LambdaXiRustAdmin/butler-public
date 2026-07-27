//! Trace memo Early Exit (Thesis O2 peel).
//! HIT path before lobby/seed/BFS — zero intentional behavior change.

use crate::server::build_status;
use crate::server::dto::*;
use crate::server::state::AppState;
use crate::vprintln;
use code_graph::CodeGraph;
use std::path::Path;

use super::disambiguate::{is_homonym_risk_name, needs_homonym_disambiguation};
use super::helpers::{collect_seed_bridge_neighbors, normalize_goal};
use super::outputs::{edge_build_status_label, is_live_background_build, make_state_info};
use super::peer_callers;
use super::receipt::{attach_trace_receipt, attach_why_edges};
use super::render::{orchestrate_content_summary, ContentDetail};
use super::spine;
use super::OrchestrateOutput;

/// Early Exit Protocol: Trace/Find memo HIT before lobby tax (scope materialize / JIT / seed+BFS).
///
/// Returns `Some` only when:
/// - goal is TraceBlastRadius or FindImplementation
/// - non-empty `target_symbol`
/// - graph skeleton is loaded
/// - hot RAM (or disk-hydrated) memo matches current `graph.current_trace_epoch()`
///
/// Call from `context_engine` **before** `ensure_background_edge_build` / `scoped_block_refs`.
/// On miss, logs `EARLY EXIT miss: …` so pytorch-scale aborts are auditable (not silent).
pub fn try_trace_memo_early_exit(
    req: &ContextRequest,
    state: &AppState,
    root: &str,
    graph: &CodeGraph,
    total_time_ms: u64,
) -> Option<OrchestrateOutput> {
    // Bare /context with goal=TraceBlastRadius must Early-Exit too (not only MCP tool name).
    if !crate::server::context_engine::wants_orchestrate_path(req) {
        return None;
    }
    if graph.nodes.is_empty() {
        return None;
    }
    let raw_goal = req
        .goal
        .as_deref()
        .or(req.mode.as_deref())
        .unwrap_or("");
    let goal_str = normalize_goal(raw_goal);
    if goal_str != "TraceBlastRadius" && goal_str != "FindImplementation" {
        return None;
    }
    let symbol = match req
        .target_symbol
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => s,
        None => return None,
    };

    let memo_epoch = crate::server::trace_memo::graph_epoch(graph);
    let memo_key = crate::server::trace_memo::make_trace_key_from_req(
        root,
        &goal_str,
        symbol,
        memo_epoch,
        req,
        state.settings.analysis.trace_max_fan_out,
        state.settings.analysis.trace_max_visited_nodes,
        2,
    );
    let payload = match crate::server::trace_memo::lookup(Path::new(root), memo_key, memo_epoch)
    {
        Some(p) => p,
        None => {
            // Distinguish empty-cache vs epoch skew (payload present, wrong epoch).
            // lookup already epoch-filters; a second probe is not free — keep one line.
            vprintln!(
                "⚡ EARLY EXIT miss: no_payload_or_epoch key={:#x} epoch={:#x} symbol={:?} nodes={}",
                memo_key,
                memo_epoch,
                symbol,
                graph.nodes.len()
            );
            return None;
        }
    };

    // Seed integrity: never early-exit a tour whose ★ is a different Ident than the query
    // (poisoned pre-integrity memos: search_py → search_lib_dir).
    if !code_graph::seed_name_matches_query(symbol, &payload.target.name) {
        vprintln!(
            "⚡ EARLY EXIT reject: seed integrity ★={:?} ≠ query={:?} key={:#x}",
            payload.target.name,
            symbol,
            memo_key
        );
        return None;
    }
    // T.2: never serve a cached full Trace for short homonyms that still need alts-first.
    // (Memo may predate the gate or was stored with a lucky wrong ★ neighborhood.)
    if is_homonym_risk_name(symbol) {
        let locs: Vec<SymbolLocation> = payload
            .locations
            .iter()
            .map(crate::server::trace_memo::sym_from_location)
            .collect();
        if needs_homonym_disambiguation(symbol, &locs, None) {
            vprintln!(
                "⚡ EARLY EXIT reject: T.2 disambiguate required for {:?}",
                symbol
            );
            return None;
        }
    }

    let (bg_pct, bg_status) = {
        let live = is_live_background_build(state, root, Some(graph));
        edge_build_status_label(state, root, Some(graph), live)
    };
    let conf = build_status::confidence_rung(
        state,
        root,
        Some(graph),
        true,
        !payload.callers.is_empty() || !payload.callees.is_empty(),
    );
    let st = make_state_info(
        format!("{}% | {}", bg_pct, bg_status),
        "early_exit".to_string(),
        conf,
        bg_pct,
    );
    let (mut rep, mer) = crate::server::trace_memo::report_from_memo(
        &payload,
        st,
        "early_exit",
        total_time_ms,
        graph.nodes.len(),
    );
    // Soft I4: stamp focus telemetry on memo hydrate (key already isolates focus tours).
    let focus_names = crate::server::trace_pack::focus_names_from_parts(
        req.focus_symbol.as_deref(),
        req.focus_symbols.as_ref().map(|v| v.as_slice()),
    );
    crate::server::trace_pack::stamp_focus_telemetry(
        &mut rep.telemetry,
        &rep.callers,
        &focus_names,
    );
    crate::server::trace_pack::stamp_sample_window_telemetry(
        &mut rep.telemetry,
        req.sample_offset,
        req.sample_mode.as_deref(),
        req.exclude_symbols.as_ref().map(|v| v.as_slice()),
    );
    // Honesty: scrub peer∩hard on early-exit tours too.
    let _ = peer_callers::dedupe_peers_against_hard_callers(
        &mut rep.peer_callers,
        &rep.callers,
    );

    // Live reverse spine (cheap): fill even when memo predates caller_path / is empty.
    // Pass memo callers as hop-1 hints (loc-fallback when reverse CALL missing).
    if goal_str == "TraceBlastRadius" && rep.caller_path.is_empty() {
        rep.caller_path = spine::reverse_call_spine_for_seed(
            graph,
            Path::new(root),
            &payload.target.seed_id,
            &payload.target.name,
            &payload.target.file,
            &rep.callers,
        );
    }

    // Stale memos often store empty bridges. Live-augment from graph bridge maps only
    // (Export/Ipc already in warehouse). Never full-warehouse IPC disk re-read here.
    if goal_str == "TraceBlastRadius"
        && rep.bridge_callers.is_empty()
        && rep.bridge_callees.is_empty()
    {
        let pp = code_graph::ProjectPaths::new(std::path::Path::new(root));
        let is_noisy = |name: &str| crate::server::filters::is_trace_noise_name(name);
        let seed_id = code_graph::Id::from_string(payload.target.seed_id.clone());
        let target_opt = graph.get_block(seed_id).or_else(|| {
            // Fallback: name match preferred seed file
            graph.nodes.values().find(|b| {
                b.name == payload.target.name
                    && (payload.target.file.is_empty()
                        || b.file.to_string_lossy().ends_with(
                            payload
                                .target
                                .file
                                .rsplit('/')
                                .next()
                                .unwrap_or(""),
                        ))
            })
        });
        if let Some(target) = target_opt {
            let (br_in, br_out) =
                collect_seed_bridge_neighbors(graph, target, &pp, &is_noisy);
            // IPC full-warehouse disk re-read deliberately omitted on Early Exit.
            // Passing graph.nodes.values() into find_ipc_caller_ids_with_root was the
            // multi-minute single-core hang on large Complete warehouses (vite 60k):
            // slim sources → per-block file read + Regex::new per rule per block.
            // Phase-4 interconnect owns IPC once per session; empty bridges stay empty.
            // Do NOT fall through for dual-stack empty bridges — that re-ran full
            // interconnect under write lock on every probe (single-thread thrash).
            if !br_in.is_empty() || !br_out.is_empty() {
                rep.bridge_callers = br_in;
                rep.bridge_callees = br_out;
                if let Some(tel) = rep.telemetry.as_object_mut() {
                    tel.insert(
                        "bridge_callers".into(),
                        rep.bridge_callers.len().into(),
                    );
                    tel.insert(
                        "bridge_callees".into(),
                        rep.bridge_callees.len().into(),
                    );
                    tel.insert("bridge_live_augment".into(), true.into());
                    tel.insert(
                        "payload_blocks".into(),
                        (1 + rep.callers.len()
                            + rep.callees.len()
                            + rep.bridge_callers.len()
                            + rep.bridge_callees.len())
                        .into(),
                    );
                }
                vprintln!(
                    "⚡ EARLY EXIT bridge augment: +{} in +{} out for {:?}",
                    rep.bridge_callers.len(),
                    rep.bridge_callees.len(),
                    symbol
                );
            }
        }
    }

    // Memo never stores cites — fill top neighbors from disk (slim warehouses).
    crate::server::filters::fill_cites_from_disk(&mut rep.callers, std::path::Path::new(root), 3);
    crate::server::filters::fill_cites_from_disk(&mut rep.callees, std::path::Path::new(root), 3);
    // Seed definition may also be empty after slim strip.
    if let Some(ref mut t) = rep.target {
        let def_empty = t.definition.as_ref().is_none_or(|d| d.trim().is_empty());
        if def_empty {
            if let Some(s) =
                crate::server::filters::cite_snippet_from_disk(std::path::Path::new(root), &t.file, t.line)
            {
                t.definition = Some(s);
            }
        }
    }
    // T.1c why-edge on hydrate (memo does not store why).
    let seed_name = rep
        .target
        .as_ref()
        .map(|t| t.name.as_str())
        .unwrap_or(symbol);
    attach_why_edges(
        seed_name,
        &mut rep.callers,
        &mut rep.callees,
        &mut rep.bridge_callers,
        &mut rep.bridge_callees,
    );

    attach_trace_receipt(&mut rep);

    let detail = ContentDetail::from_req(req.detail.as_deref());
    let content = orchestrate_content_summary(Some(&rep), mer.as_deref(), detail);

    if goal_str == "TraceBlastRadius" {
        state
            .orchestrator_has_run
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    vprintln!(
        "⚡ Trace memo EARLY EXIT key={:#x} epoch={:#x} symbol={:?} (skip lobby+seed+BFS)",
        memo_key, memo_epoch, symbol
    );

    Some(OrchestrateOutput {
        content,
        mermaid: mer,
        structured: Some(rep),
    })
}
