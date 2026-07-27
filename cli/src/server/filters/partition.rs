//! Trace core/utility partition + degenerate /context responses.
use super::cite::caller_callee_from_block_at_hop;
use super::entry::structural_multiplier;
use crate::server::dto::{CallerCallee, ContextResponse};
use code_graph::{CodeGraph, Id};
use std::path::Path;

/// Extracted isomorphic partitioning (core vs utility by structural_multiplier) for trace
/// callers/callees. Eliminates the duplicated loop body over by_depth lists.
/// Returns (core_callers, core_callees, utility_omitted_count).
pub fn partition_trace_cores(
    callers_by_depth: &[Vec<Id>],
    callees_by_depth: &[Vec<Id>],
    graph: &CodeGraph,
    project_root: &Path,
) -> (Vec<CallerCallee>, Vec<CallerCallee>, usize) {
    let pp = code_graph::ProjectPaths::new(project_root);
    let total_nodes = graph.nodes.len();
    // Collect cores (mult>=1) and soft (mult>=0.5) so Trace can fail-open when
    // polyglot / shell edges only hit non-function kinds.
    let mut core_callers = vec![];
    let mut soft_callers = vec![];
    let mut utility_callers = 0usize;
    for (depth_idx, level) in callers_by_depth.iter().enumerate() {
        let hop = (depth_idx + 1).min(255) as u8;
        for id in level {
            let Some(b) = graph.get_block(id.clone()) else {
                continue;
            };
            let in_degree = graph.reverse.get(&b.id).map_or(0, |v| v.len());
            let out_degree = graph.edges.get(&b.id).map_or(0, |v| v.len());
            let mult = structural_multiplier(&b.kind, in_degree, out_degree, total_nodes);
            let cc = caller_callee_from_block_at_hop(b, &pp, hop);
            if mult >= 1.0 {
                core_callers.push(cc);
            } else if mult >= 0.5 {
                soft_callers.push(cc);
                utility_callers += 1;
            } else {
                utility_callers += 1;
            }
        }
    }
    let mut core_callees = vec![];
    let mut soft_callees = vec![];
    let mut utility_callees = 0usize;
    for (depth_idx, level) in callees_by_depth.iter().enumerate() {
        let hop = (depth_idx + 1).min(255) as u8;
        for id in level {
            let Some(b) = graph.get_block(id.clone()) else {
                continue;
            };
            let in_degree = graph.reverse.get(&b.id).map_or(0, |v| v.len());
            let out_degree = graph.edges.get(&b.id).map_or(0, |v| v.len());
            let mult = structural_multiplier(&b.kind, in_degree, out_degree, total_nodes);
            let cc = caller_callee_from_block_at_hop(b, &pp, hop);
            if mult >= 1.0 {
                core_callees.push(cc);
            } else if mult >= 0.5 {
                soft_callees.push(cc);
                utility_callees += 1;
            } else {
                utility_callees += 1;
            }
        }
    }
    // Fail-open interconnects: empty core → promote soft neighbors (still drops noise mult 0.1).
    if core_callers.is_empty() && !soft_callers.is_empty() {
        core_callers = soft_callers;
        utility_callers = utility_callers.saturating_sub(core_callers.len());
    }
    if core_callees.is_empty() && !soft_callees.is_empty() {
        core_callees = soft_callees;
        utility_callees = utility_callees.saturating_sub(core_callees.len());
    }
    (
        core_callers,
        core_callees,
        utility_callers + utility_callees,
    )
}

/// Degenerate ContextResponse for instructions, errors, building, discovery, etc.
/// selected_count=0, no token/mermaid/structured/omitted; common telemetry fields filled.
/// Extracted from ~10+ near-identical literals across context_engine.rs (P0 HIT-4).
pub fn degenerate_context_response(
    content: String,
    warning: Option<String>,
    mode: Option<String>,
    graph_time_ms: u64,
    cached: bool,
    total_time_ms: u64,
) -> ContextResponse {
    degenerate_context_response_structured(
        content,
        warning,
        mode,
        graph_time_ms,
        cached,
        total_time_ms,
        None,
    )
}

/// Like [`degenerate_context_response`] with optional structured payload (e.g. wait_policy).
pub fn degenerate_context_response_structured(
    content: String,
    warning: Option<String>,
    mode: Option<String>,
    graph_time_ms: u64,
    cached: bool,
    total_time_ms: u64,
    structured: Option<serde_json::Value>,
) -> ContextResponse {
    ContextResponse {
        content,
        selected_count: 0,
        warning,
        token_count: None,
        mode,
        blocks_omitted: None,
        graph_time_ms: Some(graph_time_ms),
        cached: Some(cached),
        total_time_ms: Some(total_time_ms),
        mermaid: None,
        structured,
    }
}
