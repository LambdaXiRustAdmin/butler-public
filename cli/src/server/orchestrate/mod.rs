//! High-level orchestration for `butler_orchestrate` / `butler_ask` Trace·Arch·Find.
//!
//! Split (M1 + M1b + **P1a–c**): receipt · disambiguate · render · neighborhood · seed · arch
//! · peer_callers · spine · **trace_pack_helpers** · **outputs** · **helpers** · **memo_early**
//! · **trace_path** · **detail_tests** (cfg test only)
//! — zero intentional behavior change. `handle_orchestrate` remains the switch + Trace glue.

mod receipt;
mod disambiguate;
mod render;
mod neighborhood;
mod peer_callers;
mod spine;
mod seed;
mod arch;
mod trace_pack_helpers;
mod outputs;
mod helpers;
mod memo_early;
mod trace_path;

// P1b: names in parent scope so render/arch/detail_tests/trace_path keep `super::` paths.
#[allow(unused_imports)]
use trace_pack_helpers::{
    build_symbol_locations, cap_trace_payload_focus, collect_unique_hubs, empty_callers_line,
    enclosing_callable, loc_fallback_unique_fn_def, next_action_mega_hub, report_incomplete,
    short_path, truncate_def, HUB_FANIN_NEXT,
};

// P1c: BUILDING / scope-repair / error outputs.
use outputs::{
    arch_scope_refused_output, building_graph_output_for_symbol_confirm, edge_build_status_label,
    error_orchestrate_output, error_structured_report, is_live_background_build, make_state_info,
    no_blocks_in_scope_message, scope_empty_blocks_repair_output, scope_not_found_repair_output,
    symbol_not_found_message,
};
pub(crate) use outputs::scope_working_set_truly_too_big;

// O1: helpers in parent scope so render/arch/detail_tests keep `super::foo` paths.
// Helpers in parent scope for sibling `super::` paths + handle_orchestrate.
#[allow(unused_imports)]
pub(crate) use helpers::{
    bridge_infos, call_side_bit, cluster_infos_from_scoped, collect_seed_bridge_neighbors,
    edge_census_from_report, format_loc_lang, format_loc_lang_hop, hop_split,
    hub_cap_for_summary, lang_cluster_of, normalize_goal, scope_frame_line,
    scope_frame_line_with_peers,
};

// O2: Early Exit entry (public surface unchanged).
pub use memo_early::try_trace_memo_early_exit;

#[allow(unused_imports)]
pub(crate) use receipt::{
    attach_trace_receipt, attach_why_edges, next_action_disambiguate,
    next_action_missing_target_symbol, next_action_symbol_miss, set_next_action,
};
#[allow(unused_imports)]
pub(crate) use disambiguate::{
    collision_alt_file_count, is_homonym_risk_name, needs_homonym_disambiguation,
    pin_locations_for_disambiguate, sanitize_scope_prefix, serious_alt_file_count,
    suggested_scopes_from_locations, suggested_scopes_from_paths,
};
pub use render::{orchestrate_content_summary, ContentDetail};


use crate::vprintln;
use crate::server::build_status;
use crate::server::dto::*;
use crate::server::state::AppState;
use code_graph::{BlockInfo, CodeGraph, NeuralSelectionBlend};
use std::path::Path;
use std::sync::{Arc, RwLock};


pub fn inject_response_telemetry(
    telemetry: &mut serde_json::Value,
    blocks_scanned: usize,
    total_time_ms: u64,
    payload_blocks: usize,
) {
    let tokens_saved = blocks_scanned
        .saturating_sub(payload_blocks)
        .saturating_mul(150);
    if let Some(obj) = telemetry.as_object_mut() {
        obj.insert("blocks_scanned".into(), blocks_scanned.into());
        obj.insert("total_time_ms".into(), total_time_ms.into());
        obj.insert("tokens_saved_estimate".into(), tokens_saved.into());
    }
}


// extracted to submodule; was lines 87-294]

// extracted to submodule; was lines 296-477]
/// Result of `handle_orchestrate` — human summary in `content`, native report in `structured`.
pub struct OrchestrateOutput {
    pub content: String,
    pub mermaid: Option<String>,
    pub structured: Option<StructuredReport>,
}

pub fn structured_report_to_value(st: &StructuredReport) -> serde_json::Value {
    serde_json::to_value(st).unwrap_or_else(|_| serde_json::json!({}))
}

fn project_noise_config(
    state: &AppState,
    root: &Path,
) -> crate::server::filters::NoiseFilterConfig {
    let mut settings = state.settings.clone();
    settings.merge_project_config(root);
    crate::server::filters::NoiseFilterConfig::from_analysis(&settings.analysis)
}

pub fn handle_orchestrate(
    req: &ContextRequest,
    state: &AppState,
    root: &str,
    graph_rw: &Arc<RwLock<CodeGraph>>,
    scoped: &[&BlockInfo],
    graph: &CodeGraph,
    ipc_rules: &[code_graph::snooper::ipc_engine::IpcRule],
    use_neural_scores: bool,
    blend: NeuralSelectionBlend,
    total_time_ms: u64,
) -> OrchestrateOutput {
    let blocks_scanned = graph.nodes.len();
    let root_path = Path::new(root);
    // Foundation: single path dialect for this request (repo-relative warehouse → display).
    let pp = code_graph::ProjectPaths::new(root_path);

    // HIT 13: Hoist the config merge (possible disk I/O) to top; pass by ref to avoid repeated calls in Trace/Arch/etc branches.
    let noise_cfg = project_noise_config(state, root_path);

    let raw_goal = req
        .goal
        .clone()
        .or_else(|| req.mode.clone())
        .unwrap_or_default();
    let goal_str = normalize_goal(&raw_goal);

    let jit_note = "None".to_string();

    let (bg_pct, bg_status) = {
        let gg = build_status::try_read_graph(graph_rw);
        let live = is_live_background_build(state, root, gg.as_deref());
        edge_build_status_label(state, root, gg.as_deref(), live)
    };
    let edge_build_label = format!("{bg_pct}% | {bg_status}");
    let edges_complete = {
        let gg = build_status::try_read_graph(graph_rw);
        build_status::is_edge_build_complete(state, root, gg.as_deref())
    };
    let conf_full_or_inv = if edges_complete {
        build_status::ConfidenceRung::EdgesFull
    } else {
        build_status::ConfidenceRung::Inventory
    };

    // Stream-merge may hold write lock briefly — never hollow Building; serve/JIT below.
    let _ = build_status::try_lock_contention_building(state, root, graph_rw);

    // False Complete on unsupported product langs (e.g. spring-boot Java → JS crumbs).
    if let Some(void) = graph.warehouse_lang_void.as_ref() {
        let msg = void.user_message(root);
        vprintln!(
            "⚠️  lang_void refuse root={} .{} unsup={} sup={}",
            root,
            void.dominant_ext,
            void.unsupported_files,
            void.supported_files
        );
        let mut st = error_structured_report(
            &msg,
            &edge_build_label,
            &jit_note,
            conf_full_or_inv,
            bg_pct,
        );
        st.telemetry["lang_void"] = true.into();
        st.telemetry["dominant_ext"] = void.dominant_ext.clone().into();
        st.telemetry["unsupported_files"] = void.unsupported_files.into();
        st.telemetry["supported_files"] = void.supported_files.into();
        set_next_action(
            &mut st,
            "pick a supported-language root (rs/py/ts/go/c/…) or wait for lang drawer support",
        );
        return OrchestrateOutput {
            content: msg,
            mermaid: None,
            structured: Some(st),
        };
    }

    if scoped.is_empty() {
        let blank = crate::server::filters::is_blank_scope(&req.scope_paths);
        // Non-blank scope + empty working set: distinguish miss / empty blocks / fat refuse.
        // Never paint warehouse-wide "too broad" when the scope only hit a handful of files
        // (bevy root `src/lib.rs`: file_hits=1, n_scoped=0 → was wrongly "141k too broad").
        if !blank {
            let file_hits = code_graph::snooper::count_files_in_scope(
                graph,
                &req.scope_paths,
                &req.ignore_paths,
            );
            let est = code_graph::snooper::estimate_nodes_in_scope(graph, file_hits);
            let truly_too_big =
                scope_working_set_truly_too_big(file_hits, est, graph.nodes.len());

            if file_hits == 0 {
                return scope_not_found_repair_output(
                    graph,
                    root_path,
                    &pp,
                    &req.scope_paths,
                    &edge_build_label,
                    &jit_note,
                    conf_full_or_inv,
                    bg_pct,
                );
            }
            if !truly_too_big {
                // Tiny/mid hit but zero parse blocks (or collect empty) — repair, not refuse.
                return scope_empty_blocks_repair_output(
                    graph,
                    root_path,
                    &pp,
                    &req.scope_paths,
                    file_hits,
                    &edge_build_label,
                    &jit_note,
                    conf_full_or_inv,
                    bg_pct,
                );
            }
            // Truly fat: Arch refuse with suggested pins; Trace keeps generic empty message.
            if goal_str == "ArchitecturalSummary" {
                return arch_scope_refused_output(
                    graph,
                    root_path,
                    &pp,
                    0,
                    graph.nodes.len(),
                    &edge_build_label,
                    &jit_note,
                    conf_full_or_inv,
                    bg_pct,
                    "preflight (scope too large or capped)",
                );
            }
            let error = no_blocks_in_scope_message(&req.scope_paths);
            return error_orchestrate_output(
                &error,
                &edge_build_label,
                &jit_note,
                conf_full_or_inv,
                bg_pct,
            );
        }
        // Blank scope Arch preflight empties leviathans — surface top-level pins.
        if goal_str == "ArchitecturalSummary" {
            return arch_scope_refused_output(
                graph,
                root_path,
                &pp,
                0,
                graph.nodes.len(),
                &edge_build_label,
                &jit_note,
                conf_full_or_inv,
                bg_pct,
                "preflight (file inventory / blank monorepo)",
            );
        }
        let error = no_blocks_in_scope_message(&req.scope_paths);
        return error_orchestrate_output(
            &error,
            &edge_build_label,
            &jit_note,
            conf_full_or_inv,
            bg_pct,
        );
    }

    // Skeleton in RAM → always continue into Trace/Find/Arch (JIT fills missing edges).
    // Full-repo edge grind is background; do not black out product on live worker.
    // Important: try_read fails under brief FullEdge write merge — that must NOT look like
    // cold Phase-1 "Building Graph". Brief-block serve read, then telemetry inventory hint.
    {
        let skeleton_ready = build_status::skeleton_present_for_serve(state, root, graph_rw);
        if !skeleton_ready {
            let gg = build_status::try_read_graph(graph_rw);
            let live = is_live_background_build(state, root, gg.as_deref());
            if live {
                // Empty shell + live worker — usable BUILDING pack (TOC when progressive nodes exist).
                let sym = req
                    .target_symbol
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let confirm = req.confirm_long_wait.unwrap_or(false);
                return building_graph_output_for_symbol_confirm(
                    state,
                    root,
                    gg.as_deref(),
                    true,
                    sym,
                    confirm,
                );
            }
        }
    }

    let mut trace_mermaid: Option<String> = None;
    let mut structured: Option<StructuredReport> = None;

    match goal_str.as_str() {
        "TraceBlastRadius" | "FindImplementation" => {
            let (st, mer) = trace_path::run_trace_find(
                req,
                state,
                root,
                root_path,
                graph,
                scoped,
                &noise_cfg,
                use_neural_scores,
                blend,
                &goal_str,
                &edge_build_label,
                &jit_note,
                bg_pct,
                &bg_status,
                edges_complete,
                blocks_scanned,
                total_time_ms,
                &pp,
                ipc_rules,
            );
            structured = st;
            trace_mermaid = mer;
        }
        "ArchitecturalSummary" => {
            match arch::run_architectural_summary(
                req,
                state,
                root,
                scoped,
                graph,
                use_neural_scores,
                blend,
                &edge_build_label,
                &jit_note,
                conf_full_or_inv,
                bg_pct,
                &bg_status,
                blocks_scanned,
                total_time_ms,
            ) {
                Ok(rep) => structured = Some(rep),
                Err(out) => return out,
            }
        }
        _other => {
            // Strict: unrecognized or missing goal (after case-insens normalize) -> exact required error.
        }
    }

    if structured.is_none() {
        let symbol: Option<&str> = req
            .target_symbol
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let p = req.prompt.trim();
                if p.is_empty() {
                    None
                } else {
                    Some(p)
                }
            });
        let (error, next) = match goal_str.as_str() {
            "TraceBlastRadius" | "FindImplementation" => {
                if let Some(sym) = symbol {
                    (
                        symbol_not_found_message(sym, &req.scope_paths, edges_complete, bg_pct),
                        next_action_symbol_miss(sym, edges_complete, bg_pct),
                    )
                } else {
                    (
                        format!(
                            "Goal '{}' requires a non-empty target_symbol. next: {}",
                            goal_str,
                            next_action_missing_target_symbol()
                        ),
                        next_action_missing_target_symbol(),
                    )
                }
            }
            _ if goal_str.trim().is_empty() => (
                "Missing or unrecognized goal. Use TraceBlastRadius, FindImplementation, or ArchitecturalSummary. next: set goal to one of those three".to_string(),
                "set goal to TraceBlastRadius, FindImplementation, or ArchitecturalSummary".into(),
            ),
            _ => (
                format!(
                    "Unrecognized goal '{}'. Use TraceBlastRadius, FindImplementation, or ArchitecturalSummary. next: set a recognized goal",
                    goal_str
                ),
                "set goal to TraceBlastRadius, FindImplementation, or ArchitecturalSummary".into(),
            ),
        };
        let mut miss = error_structured_report(
            &error,
            &edge_build_label,
            &jit_note,
            conf_full_or_inv,
            bg_pct,
        );
        set_next_action(&mut miss, next);
        if let Some(sym) = symbol {
            if is_homonym_risk_name(sym) {
                if let Some(obj) = miss.telemetry.as_object_mut() {
                    obj.insert("homonym_risk".into(), true.into());
                }
            }
        }
        structured = Some(miss);
    }

    let unlock_for_trace =
        goal_str == "TraceBlastRadius" && structured.as_ref().map_or(false, |s| s.target.is_some());
    if unlock_for_trace {
        state
            .orchestrator_has_run
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    if let Some(ref mut st) = structured {
        attach_trace_receipt(st);
    }

    let detail = ContentDetail::from_req(req.detail.as_deref());
    let content = orchestrate_content_summary(
        structured.as_ref(),
        trace_mermaid.as_deref(),
        detail,
    );

    OrchestrateOutput {
        content,
        mermaid: trace_mermaid,
        structured,
    }
}


#[cfg(test)]
mod detail_tests;
