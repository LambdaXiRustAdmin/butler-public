//! Serve prep (P3.1 stage peel S4).
//!
//! After front door: watcher, bg edge, Phase-1 / skeleton fast-fail, effective mode, neural.
//! Zero intentional behavior change.

use std::sync::Arc;
use std::time::Instant;

use axum::{http::StatusCode, Json};

use crate::server::build_status;
use crate::server::dto::*;
use crate::server::mode_intent::{
    compute_effective_mode, intent_from_request, is_architectural_summary_orchestrate,
    wants_orchestrate_path, ModeIntent,
};
use crate::server::neural::try_apply_neural_scores;
use crate::server::state::*;
use crate::vprintln;

use super::building::building_graph_response_with_policy;
use super::graph_admit::{ensure_background_edge_build, ensure_watcher};

/// Flags/values needed by surgical Phase-4 and compose after serve prep.
pub(super) struct ServePrepReady {
    pub effective_mode: code_graph::ContextMode,
    pub is_orchestrate: bool,
    pub symbol_surgical_trace: bool,
    pub neural_prompt: String,
    pub use_neural_scores: bool,
    pub symbol_trace_partial_ok: bool,
}

pub(super) enum ServePrepOutcome {
    Early(Result<(StatusCode, Json<ContextResponse>), String>),
    Ready(ServePrepReady),
}

fn orchestrate_neural_prompt(req: &ContextRequest, fallback: &str) -> String {
    if let Some(sym) = req
        .target_symbol
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return sym.to_string();
    }
    match intent_from_request(req) {
        ModeIntent::ArchitecturalSummary | ModeIntent::Architecture => {
            "architecture entry plugin system core".to_string()
        }
        ModeIntent::TraceBlastRadius
        | ModeIntent::FindImplementation
        | ModeIntent::Surgical
        | ModeIntent::Implementation => {
            if !fallback.trim().is_empty() {
                fallback.to_string()
            } else {
                "architecture".to_string()
            }
        }
        _ => {
            if !fallback.trim().is_empty() {
                fallback.to_string()
            } else {
                "architecture".to_string()
            }
        }
    }
}

/// Watcher + bg edge + Phase-1/fast-fail gates + effective mode + neural apply/skip.
pub(super) fn try_serve_prep(
    state: &AppState,
    req: &ContextRequest,
    root: &str,
    graph_rw: &Arc<std::sync::RwLock<code_graph::CodeGraph>>,
    force_surgical: bool,
    effective_prompt: &str,
    graph_time_ms: u64,
    is_cached: bool,
    overall_start: Instant,
) -> ServePrepOutcome {
    ensure_watcher(
        root,
        Arc::clone(graph_rw),
        state.settings.analysis.skip_directories.clone(),
    );

    // Sprint 5: ensure bg build is running or resuscitated (after Early Exit / QC).
    ensure_background_edge_build(state, root, graph_rw);

    if let Some(msg) = build_status::try_phase1_scan_building(
        state,
        root,
        build_status::try_read_graph(graph_rw).as_deref(),
    ) {
        return ServePrepOutcome::Early(building_graph_response_with_policy(
            state,
            root,
            msg,
            graph_time_ms,
            is_cached,
            overall_start,
            req.confirm_long_wait.unwrap_or(false),
        ));
    }

    // Serve skeleton while full edges grind. Only Phase-1 empty shell should hang-on.
    // Symbol Trace/Find/Arch all partial-serve; JIT fills symbol files below.
    let symbol_trace_partial_ok = {
        let intent = intent_from_request(req);
        req.target_symbol
            .as_deref()
            .map(str::trim)
            .is_some_and(|s| !s.is_empty())
            || is_architectural_summary_orchestrate(req)
            || intent.wants_orchestrate()
            || matches!(
                intent,
                ModeIntent::Surgical | ModeIntent::Implementation | ModeIntent::Architecture
            )
    };
    // block_on only if no skeleton-oriented intent *and* empty graph — fast_fail itself
    // serves whenever nodes are non-empty regardless of this flag.
    if let Some(msg) =
        build_status::try_building_fast_fail(state, root, graph_rw, !symbol_trace_partial_ok)
    {
        return ServePrepOutcome::Early(building_graph_response_with_policy(
            state,
            root,
            msg,
            graph_time_ms,
            is_cached,
            overall_start,
            req.confirm_long_wait.unwrap_or(false),
        ));
    }

    // Effective mode: MCP tool overrides → force_surgical → goal/mode (mode_intent Pack A).
    // Bare POST /context and orchestrate share the same synonym table so
    // goal=ArchitecturalSummary / "architect" map to Architecture (not silent Balanced).
    let effective_mode: code_graph::ContextMode = compute_effective_mode(req, force_surgical);

    // Orchestrate product path: MCP tool **or** bare POST /context with same goals
    // (wants_orchestrate_path). Do not require mcp_tool_name — curl/smokes omit it and
    // used to fall into select_blocks O(nodes) on monsters (gecko ~17s single-thread).
    let is_orchestrate = wants_orchestrate_path(req);
    // Trace/Find with an explicit symbol: structural name map + CALL edges are the product.
    // Full-warehouse neural re-score is pure latency (logs showed 80k–90k "final_selected").
    let has_target_symbol = req
        .target_symbol
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());
    let symbol_surgical_trace = is_orchestrate
        && matches!(
            effective_mode,
            code_graph::ContextMode::Surgical | code_graph::ContextMode::Implementation
        )
        && has_target_symbol;
    // Any exact-symbol Trace/Find: structural only. Neural on 1M-node graphs is a
    // single-core hog on the request worker and drowns multi-core edge collect.
    let skip_neural_for_symbol = has_target_symbol;
    let neural_prompt = if is_orchestrate {
        orchestrate_neural_prompt(req, effective_prompt)
    } else {
        effective_prompt.to_string()
    };
    // Butler solely responsible for graph_export.json (used for training fat graphs).
    // Neural (GNN) scoring is now in-process in code_graph::gnn (SmartButler).
    let use_neural_scores = state.settings.agent.use_neural
        && !skip_neural_for_symbol
        && try_apply_neural_scores(graph_rw, root, &state.settings, &neural_prompt);
    if skip_neural_for_symbol && state.settings.agent.use_neural {
        vprintln!(
            "⚡ Neural skipped for symbol Trace/Find (target_symbol present) — structural path only"
        );
    }

    ServePrepOutcome::Ready(ServePrepReady {
        effective_mode,
        is_orchestrate,
        symbol_surgical_trace,
        neural_prompt,
        use_neural_scores,
        symbol_trace_partial_ok,
    })
}
