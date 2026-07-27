//! BUILDING responses + wait policy + query cache store (P3 peel).
//! Zero intentional behavior change.

use std::time::Instant;

use axum::{http::StatusCode, Json};

use crate::server::build_status;
use crate::server::dto::*;
use crate::server::state::*;

/// Append adaptive wait_policy when the message lacks one (legacy BUILDING meters).
pub(super) fn with_wait_policy(
    state: &AppState,
    root: &str,
    mut msg: String,
    confirm_long_wait: bool,
) -> (String, serde_json::Value) {
    let elapsed = build_status::elapsed_build_secs(state, root);
    let soft_wall = build_status::soft_wall_secs();
    let (wait_txt, wait_json) =
        build_status::wait_policy_block(elapsed, soft_wall, confirm_long_wait, None);
    let soft_block = elapsed >= soft_wall && !confirm_long_wait;
    if soft_block {
        if msg.contains("status: BUILDING") && !msg.contains("BUILDING_SOFT_WALL") {
            msg = msg.replace("status: BUILDING", "status: BUILDING_SOFT_WALL");
        }
    }
    if !msg.contains("Wait policy") {
        msg.push_str("\n\n");
        msg.push_str(&wait_txt);
    }
    (msg, wait_json)
}

pub(super) fn building_graph_response_with_policy(
    state: &AppState,
    root: &str,
    msg: String,
    graph_time_ms: u64,
    is_cached: bool,
    overall_start: Instant,
    confirm_long_wait: bool,
) -> Result<(StatusCode, Json<ContextResponse>), String> {
    let (msg, wait_json) = with_wait_policy(state, root, msg, confirm_long_wait);
    building_graph_response_structured(
        msg,
        graph_time_ms,
        is_cached,
        overall_start,
        Some(wait_json),
    )
}

pub(super) fn building_graph_response_structured(
    msg: String,
    graph_time_ms: u64,
    is_cached: bool,
    overall_start: Instant,
    structured: Option<serde_json::Value>,
) -> Result<(StatusCode, Json<ContextResponse>), String> {
    // Ensure BUILDING contract even when callers only pass the old meter.
    let soft = msg.contains("BUILDING_SOFT_WALL");
    let msg = if msg.contains("status: BUILDING") || soft {
        msg
    } else {
        format!(
            "{msg}\n\n=== Usable while building ===\nstatus: BUILDING\naction: retry shortly; pass scope_paths when toc appears. Not a hang.\n"
        )
    };
    Ok((
        StatusCode::OK,
        Json(
            crate::server::filters::degenerate_context_response_structured(
                msg,
                Some("graph_building".to_string()),
                Some(if soft {
                    "building_soft_wall".to_string()
                } else {
                    "building".to_string()
                }),
                graph_time_ms,
                is_cached,
                overall_start.elapsed().as_millis() as u64,
                structured,
            ),
        ),
    ))
}

/// Store successful (non-building) context responses in the query cache.
///
/// Honest-partial / provisional-miss answers are **not** cached: rewalk must see
/// fresher warehouse state. Percent is also in the key (belt-and-suspenders).
pub(super) fn cache_context_result(
    state: &AppState,
    query_key: u64,
    res: &Result<(StatusCode, Json<ContextResponse>), String>,
    edges_complete: bool,
) {
    let Ok((_, Json(resp))) = res else {
        return;
    };
    if !edges_complete {
        // Never freeze a 15% pack — agent rewalk after FullEdge must not get stale.
        return;
    }
    let warn = resp.warning.as_deref().unwrap_or("");
    if warn == "building"
        || warn == "graph_building"
        || warn.starts_with("symbol_not_seen_yet@")
    {
        return;
    }
    if resp.content.contains("=== Building Graph")
        || resp.content.contains("=== Honest partial")
        || resp.content.contains("symbol_not_seen_yet@")
    {
        return;
    }
    if let Ok(mut qc) = state.query_cache.lock() {
        qc.insert(query_key, resp.clone());
    }
}

