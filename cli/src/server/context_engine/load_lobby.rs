//! Load lobby (P3.1 stage peel S2).
//!
//! In-progress hang-on, async graph admit (never block /context on load_graph),
//! pressure-defer retry, first-use layout defaults, graph-ready telemetry.
//!
//! Zero intentional behavior change.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{http::StatusCode, Json};
use code_graph::graph_cache_exists;

use crate::server::build_status::{self, get_telemetry, sync_telemetry_if_graph_ready};
use crate::server::dto::*;
use crate::server::mode_intent::wants_orchestrate_path;
use crate::server::state::*;
use crate::vprintln;

use super::building::building_graph_response_with_policy;
use super::graph_admit::{
    maybe_retry_pressure_deferred_load, spawn_async_graph_load, touch_graph_lru,
};
use super::resolve::apply_first_use_layout_defaults;

/// Graph handle + timing after a successful lobby (ready to front-door).
pub(super) struct LoadLobbyReady {
    pub graph_rw: Arc<std::sync::RwLock<code_graph::CodeGraph>>,
    pub is_cached: bool,
    pub graph_time_ms: u64,
    /// Node count (or edge_files_est fallback) stamped at lobby exit — used for empty-graph gate.
    pub node_count: usize,
}

pub(super) enum LoadLobbyOutcome {
    /// Still building / first-miss BUILDING — return immediately.
    Early(Result<(StatusCode, Json<ContextResponse>), String>),
    Ready(LoadLobbyReady),
}

fn graph_has_nodes(state: &AppState, root: &str) -> bool {
    state
        .graphs
        .blocking_read()
        .get(root)
        .and_then(|g| g.try_read().ok())
        .is_some_and(|g| !g.nodes.is_empty())
}

/// Wait until nodes appear **or** `budget` elapses, whichever first.
fn wait_for_nodes(state: &AppState, root: &str, budget: Duration) -> bool {
    if graph_has_nodes(state, root) {
        return true;
    }
    if budget.is_zero() {
        return false;
    }
    let start = Instant::now();
    let slice = Duration::from_millis(25);
    while start.elapsed() < budget {
        let left = budget.saturating_sub(start.elapsed());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(slice.min(left));
        if graph_has_nodes(state, root) {
            return true;
        }
    }
    graph_has_nodes(state, root)
}

/// In-progress guard → double-checked graph cache admit → pressure retry → layout defaults.
pub(super) fn try_load_lobby(
    state: &AppState,
    req: &mut ContextRequest,
    root: &str,
    overall_start: Instant,
) -> LoadLobbyOutcome {
    // In-progress: wait until done **or** grace (whichever first). Fast hydrates
    // return the answer; only still-empty after grace emits BUILDING/hydrating.
    // Do not wait here if nothing is loading (first miss spawns below).
    {
        if !graph_has_nodes(state, root) && {
            let ip = state.in_progress.blocking_read();
            ip.contains_key(root)
        } {
            let grace = Duration::from_millis(build_status::hydrate_answer_grace_ms());
            if !wait_for_nodes(state, root, grace) {
                let in_progress = state.in_progress.blocking_read();
                if let Some(progress) = in_progress.get(root) {
                    // Progressive L1 may have published nodes under a different key/race —
                    // prefer usable BUILDING with TOC when any graph is readable.
                    let toc = state
                        .graphs
                        .blocking_read()
                        .get(root)
                        .and_then(|g| g.try_read().ok())
                        .map(|g| build_status::cheap_toc_dirs(&g, 12))
                        .unwrap_or_default();
                    let confirm = req.confirm_long_wait.unwrap_or(false);
                    let (msg, wait_json) = build_status::phase1_progress_message_with_toc_confirm(
                        root, progress, &toc, confirm,
                    );
                    let soft = msg.contains("BUILDING_SOFT_WALL");
                    return LoadLobbyOutcome::Early(Ok((
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
                                0,
                                false,
                                overall_start.elapsed().as_millis() as u64,
                                Some(wait_json),
                            ),
                        ),
                    )));
                }
            }
        }
    }

    // Graph load (double checked). P0: never block /context on load_graph — always async.
    let graph_load_start = Instant::now();
    let graph_rw: Arc<std::sync::RwLock<code_graph::CodeGraph>> = {
        {
            let cache = state.graphs.blocking_read();
            if let Some(g) = cache.get(root) {
                Arc::clone(g)
            } else {
                drop(cache);
                let mut cache = state.graphs.blocking_write();
                if let Some(g) = cache.get(root) {
                    Arc::clone(g)
                } else {
                    let g_rw = Arc::new(std::sync::RwLock::new(code_graph::CodeGraph::new()));
                    cache.insert(root.to_string(), Arc::clone(&g_rw));
                    drop(cache);
                    spawn_async_graph_load(state, root, Arc::clone(&g_rw));
                    touch_graph_lru(state, root);
                    let grace = Duration::from_millis(build_status::hydrate_answer_grace_ms());
                    if wait_for_nodes(state, root, grace) {
                        g_rw
                    } else {
                        let msg = if graph_cache_exists(root) {
                            build_status::hydrating_graph_message(0)
                        } else {
                            build_status::building_graph_message(0)
                        };
                        return LoadLobbyOutcome::Early(building_graph_response_with_policy(
                            state,
                            root,
                            msg,
                            0,
                            false,
                            overall_start,
                            req.confirm_long_wait.unwrap_or(false),
                        ));
                    }
                }
            }
        }
    };
    // In-memory hit (async load already completed, or warm root). First miss always returned above.
    let is_cached = true;
    touch_graph_lru(state, root);
    // Empty shell + pressure defer → retry admission when host freer.
    maybe_retry_pressure_deferred_load(state, root, &graph_rw);

    let graph_load_time = graph_load_start.elapsed();
    let graph_time_ms = graph_load_time.as_millis() as u64;
    // Prefer real node count; never lie with edge files_total as "blocks" (gecko 30704 vs 4.8M).
    let (node_count, node_count_src) = if let Some(gg) = build_status::try_read_graph(&graph_rw) {
        (gg.nodes.len(), "nodes")
    } else {
        (
            get_telemetry(state, root)
                .map(|t| t.files_total.load(std::sync::atomic::Ordering::Relaxed))
                .unwrap_or(0),
            "edge_files_est",
        )
    };
    if let Some(gg) = build_status::try_read_graph(&graph_rw) {
        sync_telemetry_if_graph_ready(state, root, &gg);
        apply_first_use_layout_defaults(req, &gg, Path::new(root));
    } else {
        // Writer held (edge merge / maps). Clear zombie live if heartbeat died.
        if let Some(t) = get_telemetry(state, root) {
            t.clear_if_heartbeat_stale(180);
        }
        let is_trace = wants_orchestrate_path(req)
            && req
                .target_symbol
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty());
        if is_trace {
            vprintln!(
                "⚡ EARLY EXIT abort: try_read_busy root={} (writer holds CodeGraph — will retry after lobby)",
                root
            );
        }
    }

    vprintln!(
        "✅ Graph ready — {} {} (project: {}) | load_time={:.2?}",
        node_count, node_count_src, root, graph_load_time
    );

    LoadLobbyOutcome::Ready(LoadLobbyReady {
        graph_rw,
        is_cached,
        graph_time_ms,
        node_count,
    })
}
