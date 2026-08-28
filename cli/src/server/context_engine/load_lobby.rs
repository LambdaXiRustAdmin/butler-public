//! Load lobby (P3.1 stage peel S2).
//!
//! In-progress hang-on, async graph admit (never block /context on load_graph),
//! pressure-defer retry, first-use layout defaults, graph-ready telemetry.
//!
//! Start-grace: wait until hydrate/BUILDING/starting is done **or** budget, whichever first.

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

/// Sight of the RAM graph. `try_read` fail is **not** empty — a warm repo
/// under a brief write lock (incremental edges) used to look like 0 nodes
/// and flash BUILDING after the start-grace.
enum GraphSight {
    Nodes,
    Empty,
    LockBusy,
    Missing,
}

fn graph_sight(state: &AppState, root: &str) -> GraphSight {
    let Some(g) = state.graphs.blocking_read().get(root).cloned() else {
        return GraphSight::Missing;
    };
    let sight = match g.try_read() {
        Ok(gg) if gg.nodes.is_empty() => GraphSight::Empty,
        Ok(_) => GraphSight::Nodes,
        Err(_) => GraphSight::LockBusy,
    };
    sight
}

fn graph_has_nodes(state: &AppState, root: &str) -> bool {
    matches!(graph_sight(state, root), GraphSight::Nodes)
}

/// True when a hydrate / Phase-1 / cold scan thread is still registered.
/// Lock busy → assume in flight (do not treat as done).
fn load_in_flight(state: &AppState, root: &str) -> bool {
    match state.in_progress.try_read() {
        Ok(m) => m.contains_key(root),
        Err(_) => true,
    }
}

fn sight_is_ready(sight: GraphSight, in_flight: bool) -> bool {
    match sight {
        GraphSight::Nodes => true,
        // Warm graph, writer held, no scan thread — serve path brief-blocks.
        GraphSight::LockBusy if !in_flight => true,
        _ => false,
    }
}

/// Wait until nodes appear, the load finishes, **or** `budget` elapses — whichever first.
/// Lock-busy on a resident graph is ready, not BUILDING.
fn wait_for_nodes(state: &AppState, root: &str, budget: Duration) -> bool {
    let in_flight = load_in_flight(state, root);
    if sight_is_ready(graph_sight(state, root), in_flight) {
        return true;
    }
    if budget.is_zero() || !in_flight {
        return sight_is_ready(graph_sight(state, root), load_in_flight(state, root));
    }
    let start = Instant::now();
    let slice = Duration::from_millis(25);
    while start.elapsed() < budget {
        let left = budget.saturating_sub(start.elapsed());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(slice.min(left));
        let in_flight = load_in_flight(state, root);
        if sight_is_ready(graph_sight(state, root), in_flight) {
            return true;
        }
        if !in_flight {
            break;
        }
    }
    sight_is_ready(graph_sight(state, root), load_in_flight(state, root))
}

/// In-progress guard → double-checked graph cache admit → pressure retry → layout defaults.
pub(super) fn try_load_lobby(
    state: &AppState,
    req: &mut ContextRequest,
    root: &str,
    overall_start: Instant,
) -> LoadLobbyOutcome {
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
                    g_rw
                }
            }
        }
    };

    // One start-grace for hydrate, BUILDING, and empty-shell "starting": wait until
    // nodes appear **or** budget, whichever first. A write lock on a resident graph
    // is not empty — do not BUILDING rust-rage because try_read failed.
    if !graph_has_nodes(state, root) {
        let grace = Duration::from_millis(build_status::hydrate_answer_grace_ms());
        if !wait_for_nodes(state, root, grace)
            && !matches!(graph_sight(state, root), GraphSight::LockBusy)
        {
            let in_progress = state.in_progress.blocking_read();
            if let Some(progress) = in_progress.get(root) {
                let confirm = req.confirm_long_wait.unwrap_or(false);
                if graph_cache_exists(root) {
                    drop(in_progress);
                    return LoadLobbyOutcome::Early(building_graph_response_with_policy(
                        state,
                        root,
                        build_status::hydrating_graph_message(0),
                        0,
                        false,
                        overall_start,
                        confirm,
                    ));
                }
                let toc = state
                    .graphs
                    .blocking_read()
                    .get(root)
                    .and_then(|g| g.try_read().ok())
                    .map(|g| build_status::cheap_toc_dirs(&g, 12))
                    .unwrap_or_default();
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
            // Load finished empty (or never started): not BUILDING — compose empty-graph.
        }
    }
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
        node_count,
        node_count_src,
        root,
        graph_load_time
    );

    LoadLobbyOutcome::Ready(LoadLobbyReady {
        graph_rw,
        is_cached,
        graph_time_ms,
        node_count,
    })
}
