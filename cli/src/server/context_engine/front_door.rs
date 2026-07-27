//! Front door: Trace memo Early Exit + query-cache HIT (P3.1 stage peel).
//!
//! Must run **before** WarehousePolice / watcher / inventory rewalk.
//! (pytorch multi-hit paid ~5s in `is_edge_build_complete` before memo.)
//!
//! Zero intentional behavior change.

use std::sync::Arc;
use std::time::Instant;

use axum::{http::StatusCode, Json};

use crate::server::build_status;
use crate::server::dto::*;
use crate::server::query_cache;
use crate::server::state::*;
use crate::vprintln;

use super::building::cache_context_result;

/// Values needed after a front-door miss (compose path still keys/caches on these).
pub(super) struct FrontDoorContinue {
    pub query_key: u64,
    pub edges_complete: bool,
    pub edge_percent: usize,
}

pub(super) enum FrontDoorOutcome {
    /// Early Exit memo or query-cache HIT — return immediately.
    Hit(Result<(StatusCode, Json<ContextResponse>), String>),
    /// No short-circuit; continue request after watcher / bg / JIT.
    Continue(FrontDoorContinue),
}

/// Early Exit memo + query-cache HIT before police lane + scoped world.
pub(super) fn try_front_door(
    state: &AppState,
    req: &ContextRequest,
    root: &str,
    graph_rw: &Arc<std::sync::RwLock<code_graph::CodeGraph>>,
    effective_prompt: &str,
    graph_time_ms: u64,
    overall_start: Instant,
) -> FrontDoorOutcome {
    let graph_snapshot = build_status::try_read_graph(graph_rw);
    let graph_version = graph_snapshot.as_ref().map(|g| g.version).unwrap_or(0);
    let edges_complete =
        build_status::is_edge_build_complete(state, root, graph_snapshot.as_deref());
    let edge_percent = build_status::percent_for_status(state, root, graph_snapshot.as_deref());
    let want_neural = state.settings.agent.use_neural;
    let query_key = query_cache::make_query_key(
        root,
        graph_version,
        req,
        effective_prompt,
        want_neural,
        edge_percent,
        edges_complete,
    );

    // Early Exit Protocol: Trace/Find path-memo before police lane + scoped world.
    if let Some(ref gg) = graph_snapshot {
        if let Some(early) = crate::server::orchestrate::try_trace_memo_early_exit(
            req,
            state,
            root,
            gg,
            overall_start.elapsed().as_millis() as u64,
        ) {
            let selected_count = early
                .structured
                .as_ref()
                .map(|st| {
                    if st.target.is_some() {
                        1 + st.callers.len() + st.callees.len()
                    } else {
                        0
                    }
                })
                .unwrap_or(0);
            let token_count = (early.content.len() / 4).max(1);
            let structured_json = early
                .structured
                .as_ref()
                .map(crate::server::orchestrate::structured_report_to_value);
            let res = Ok((
                StatusCode::OK,
                Json(ContextResponse {
                    content: early.content,
                    selected_count,
                    warning: None,
                    token_count: Some(token_count),
                    mode: Some("orchestrate".to_string()),
                    blocks_omitted: None,
                    graph_time_ms: Some(graph_time_ms),
                    cached: Some(true),
                    total_time_ms: Some(overall_start.elapsed().as_millis() as u64),
                    mermaid: early.mermaid,
                    structured: structured_json,
                }),
            ));
            cache_context_result(state, query_key, &res, edges_complete);
            return FrontDoorOutcome::Hit(res);
        }
    }
    drop(graph_snapshot);

    if let Ok(mut qc) = state.query_cache.lock() {
        if let Some(mut hit) = qc.get(query_key) {
            hit.cached = Some(true);
            hit.graph_time_ms = Some(graph_time_ms);
            hit.total_time_ms = Some(overall_start.elapsed().as_millis() as u64);
            vprintln!(
                "⚡ Query cache HIT (key={:#x}, graph_v={})",
                query_key, graph_version
            );
            return FrontDoorOutcome::Hit(Ok((StatusCode::OK, Json(hit))));
        }
    }

    FrontDoorOutcome::Continue(FrontDoorContinue {
        query_key,
        edges_complete,
        edge_percent,
    })
}
