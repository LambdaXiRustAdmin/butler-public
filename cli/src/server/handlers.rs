//! Thin Axum HTTP surface for Butler server endpoints.
//!
//! After split (Perfectionist Path): contains ONLY the endpoint wrappers.
//! All heavy logic (including the refactored run_context_logic orchestration) lives in context_engine.
//! No God Function here. Delegates directly to context_engine::run_context_logic.
//! Public signatures unchanged for full backward compat with router and MCP clients.

use crate::vprintln;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use std::env;

use crate::server::context_engine::{
    effective_prompt_for_request, get_fingerprint, get_nickname, run_context_logic,
    warm_project_root,
};
use crate::server::dto::*;
use crate::server::state::*;

// MCP toolbelt (shared with stdio bridge — ask-first, stable list).
use cli::butler_ask::product_mcp_tools_json;

/// Handles `GET /mcp/manifest` — returns the MCP tool manifest (thin wrapper).
///
/// Stable product belt: **who_calls** first, then butler_ask alias + orchestrate + help.
/// Expert suite only when `agent.expert_mode` (not mid-session unlock).
pub async fn mcp_manifest(State(state): State<AppState>) -> impl IntoResponse {
    let expert = state.settings.agent.expert_mode;
    let tools: Vec<McpTool> = product_mcp_tools_json(expert, vec![])
        .into_iter()
        .filter_map(|t| {
            Some(McpTool {
                name: t.get("name")?.as_str()?.to_string(),
                description: t
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
                method: "POST".to_string(),
                path: "/context".to_string(),
                input_schema: t.get("inputSchema").cloned().unwrap_or(serde_json::json!({})),
            })
        })
        .collect();

    let manifest = McpManifest {
        name: "butler".to_string(),
        description: "Butler — who_calls first (direct callers/callees). Internal: butler_ask."
            .to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        base_url: "/".to_string(),
        tools,
    };
    Json(manifest)
}

/// Handles `GET /mcp/health` — returns server health status (thin).
pub async fn mcp_health(State(state): State<AppState>) -> impl IntoResponse {
    use crate::server::build_status::is_live_build;
    use std::collections::HashMap;

    let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut edge_builds = HashMap::new();
    if let Ok(map) = state.edge_build_status.try_read() {
        for (project, telemetry) in map.iter() {
            let graph = state
                .graphs
                .try_read()
                .ok()
                .and_then(|g| g.get(project).cloned());
            let graph_snap = graph.as_ref().and_then(|rw| rw.try_read().ok());
            // Prefer percent_for_status when live so mid-batch files_processed shows
            // (inventory merge lag no longer freezes the meter at 0%).
            let percent = crate::server::build_status::percent_for_status(
                &state,
                project,
                graph_snap.as_deref(),
            );
            let phase = {
                let p = telemetry.phase_str();
                if p.is_empty() {
                    None
                } else {
                    Some(p)
                }
            };
            edge_builds.insert(
                project.clone(),
                EdgeBuildHealthEntry {
                    percent,
                    state: format!("{:?}", telemetry.state()),
                    live: is_live_build(&state, project, graph_snap.as_deref()),
                    phase,
                    blocked: telemetry.blocked_str(),
                    heartbeat_age_s: telemetry.heartbeat_age_secs(),
                },
            );
        }
    }

    // RAM-resident warehouses (hydrate done). Agents: use this for "is it loaded?",
    // not edge_builds (FullEdge only — often empty after trusted Complete hydrate).
    let mut loaded = HashMap::new();
    if let Ok(map) = state.graphs.try_read() {
        for (project, rw) in map.iter() {
            if let Ok(g) = rw.try_read() {
                if g.nodes.is_empty() {
                    continue;
                }
                loaded.insert(
                    project.clone(),
                    crate::server::dto::LoadedGraphHealth {
                        nodes: g.nodes.len(),
                        edges_complete: g.is_edge_build_complete(),
                        ready: true,
                    },
                );
            }
        }
    }

    let (fg_max, fg_active, fg_waiters) =
        code_graph::snooper::warehouse_police().fulledge_slot_status();
    let ring_path = crate::server::request_ring::log_path().map(|p| p.display().to_string());
    let mut recent = crate::server::request_ring::snapshot();
    // Health JSON: last 8 only (full trail in the ring file).
    if recent.len() > 8 {
        recent = recent.split_off(recent.len() - 8);
    }
    let health = McpHealth {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        fingerprint: get_fingerprint(root.to_string_lossy().as_ref()),
        edge_builds,
        loaded,
        fulledge_governor: Some(crate::server::dto::FullEdgeGovernorHealth {
            max: fg_max,
            active: fg_active,
            waiters: fg_waiters,
        }),
        request_log: ring_path,
        recent_requests: recent,
        agent_wait_hint: Some(serde_json::json!({
            "on_BUILDING": "retry_same_context_request_after_retry_after_ms",
            "edge_builds": "FullEdge_progress_only_not_hydrate",
            "loaded": "RAM_resident_warehouses_ready_for_Trace",
            "do_not": "wait_for_edge_builds_key_before_rewalking_hydrate",
        })),
    };
    Json(health)
}

/// Handles `GET /fingerprint` — returns server identity (thin).
pub async fn get_fingerprint_handler() -> impl IntoResponse {
    let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let fp = get_fingerprint(root.to_string_lossy().as_ref());
    let nick = get_nickname(root.to_string_lossy().as_ref());
    Json(serde_json::json!({
        "fingerprint": fp,
        "nickname": nick
    }))
}

/// Handles `POST /collisions` — multi-file same-name seeds from RAM `name_index`.
///
/// Body: `{ "project": "/path", "min_files"?: 2, "max"?: 200, "min_name_len"?: 2 }`.
/// Warehouse must already be loaded (warm/Trace first). Used by spectacular tier-2 mill.
pub async fn handle_collisions(
    State(state): State<AppState>,
    axum::extract::Json(payload): axum::extract::Json<serde_json::Value>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    let project = payload
        .get("project")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if project.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "project required"})),
        )
            .into_response();
    }
    let root = crate::server::paths::translate_client_path(&project);
    let root_key = root.to_string();
    let min_files = payload
        .get("min_files")
        .and_then(|v| v.as_u64())
        .unwrap_or(2) as usize;
    let max = payload
        .get("max")
        .and_then(|v| v.as_u64())
        .unwrap_or(200) as usize;
    let min_name_len = payload
        .get("min_name_len")
        .and_then(|v| v.as_u64())
        .unwrap_or(2) as usize;

    let map = match state.graphs.try_read() {
        Ok(m) => m,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "graphs lock busy"})),
            )
                .into_response();
        }
    };
    // Match warehouse key (host or container path).
    let mut hit_key: Option<String> = None;
    for k in map.keys() {
        if k == &root_key
            || k.ends_with(root_key.trim_start_matches('/'))
            || root_key.ends_with(k.trim_start_matches('/'))
            || k.contains(&root_key)
            || root_key.contains(k.as_str())
        {
            hit_key = Some(k.clone());
            break;
        }
    }
    // Also try translated container form
    if hit_key.is_none() {
        let cont = {
            let host = std::env::var("BUTLER_HOST_MOUNT").unwrap_or_default();
            let cont = std::env::var("BUTLER_CONTAINER_MOUNT").unwrap_or_default();
            if !host.is_empty() && root_key.starts_with(&host) {
                format!("{}{}", cont, &root_key[host.len()..])
            } else {
                root_key.clone()
            }
        };
        for k in map.keys() {
            if k == &cont || k.ends_with(cont.trim_start_matches('/')) {
                hit_key = Some(k.clone());
                break;
            }
        }
    }
    let Some(key) = hit_key else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "warehouse not loaded",
                "project": root_key,
                "hint": "warm or Trace first; see /mcp/health loaded",
            })),
        )
            .into_response();
    };
    let rw = match map.get(&key) {
        Some(g) => g.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "warehouse missing", "key": key})),
            )
                .into_response();
        }
    };
    drop(map);
    let g = match rw.try_read() {
        Ok(g) => g,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "graph read lock busy", "key": key})),
            )
                .into_response();
        }
    };
    if g.nodes.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "empty graph", "key": key})),
        )
            .into_response();
    }
    let collisions = g.multi_file_name_collisions(min_files, max.max(1), min_name_len.max(1));
    let names: Vec<serde_json::Value> = collisions
        .iter()
        .map(|(name, n_locs, n_files)| {
            serde_json::json!({
                "name": name,
                "locations": n_locs,
                "files": n_files,
            })
        })
        .collect();
    Json(serde_json::json!({
        "project": key,
        "nodes": g.nodes.len(),
        "name_index_keys": g.name_index.len(),
        "min_files": min_files,
        "max": max,
        "count": names.len(),
        "collisions": names,
    }))
    .into_response()
}

/// Handles `POST /warm` — register project root(s) for async graph load + watchers.
///
/// Body: `{ "root": "/path" }` or `{ "roots": ["/a", "/b"] }`.
/// Does not block on FullEdge completion; use `butler warm --full` for offline full edges,
/// or poll `/mcp/health` / query with BUILDING contract until ready.
pub async fn handle_warm(
    State(state): State<AppState>,
    Json(req): Json<WarmRequest>,
) -> impl IntoResponse {
    let mut roots = req.roots;
    if let Some(r) = req.root {
        if !r.trim().is_empty() {
            roots.push(r);
        }
    }
    roots.retain(|r| !r.trim().is_empty());
    roots.sort();
    roots.dedup();
    if roots.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(WarmResponse {
                ok: false,
                warmed: vec![],
                message: "provide root or roots".into(),
            }),
        );
    }
    let warmed = roots.clone();
    let result = tokio::task::spawn_blocking(move || {
        for r in &roots {
            warm_project_root(&state, r);
        }
        roots
    })
    .await;
    match result {
        Ok(done) => (
            StatusCode::OK,
            Json(WarmResponse {
                ok: true,
                warmed: done,
                message: format!(
                    "registered {} root(s) for async load + watchers",
                    warmed.len()
                ),
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(WarmResponse {
                ok: false,
                warmed: vec![],
                message: format!("warm task failed: {e}"),
            }),
        ),
    }
}

/// Async handler for the `POST /context` endpoint (thin wrapper).
///
/// Spawns the (now clean, extracted) run_context_logic on a blocking thread pool.
/// All heavy orchestration, selection, composition, and dispatch lives in context_engine.
/// Signature and error behavior identical.
///
/// The QUERY_SEMAPHORE (defined in context_engine) is acquired in a tight scope around
/// the spawn_blocking so that at most 4 heavy requests run concurrently. This protects
/// against greedy LLM pile-ups. The heavy ensure_call_graph + Phase 4 work inside
/// run_context_logic runs under this permit + the blocking task (Rayon cannot starve async IO).
/// Permit is dropped immediately after the blocking work completes (before response is sent).
pub async fn handle_context(
    State(state): State<AppState>,
    Json(req): Json<ContextRequest>,
) -> impl IntoResponse {
    let prompt_preview: String = effective_prompt_for_request(&req)
        .chars()
        .take(80)
        .collect();
    // Ring log fields (req moves into spawn_blocking).
    let ring_project = req
        .project
        .clone()
        .unwrap_or_else(|| req.root.clone());
    let ring_goal = req.goal.clone();
    let ring_symbol = req.target_symbol.clone();
    let ring_tool = req.mcp_tool_name.clone();
    let start = std::time::Instant::now();

    // Tight acquire + drop: permit held only for the duration of the heavy blocking work.
    // Dropped before we construct and return the response to the MCP client.
    let result = {
        let _permit = crate::server::context_engine::QUERY_SEMAPHORE
            .acquire()
            .await
            .unwrap();
        // The entire run_context_logic (incl. graph load, any JIT/Phase4 ensure_call_graph,
        // selection, and for orchestrate the structured report) is executed here under the permit.
        // This + the outer blocking pool ensures Rayon edge work doesn't block Tokio workers.
        tokio::task::spawn_blocking(move || run_context_logic(state, req)).await
    };

    let duration = start.elapsed();
    let duration_ms = duration.as_millis() as u64;
    vprintln!(
        "⏱️  /context completed in {:.2?} | prompt=\"{}\"",
        duration, prompt_preview
    );

    match result {
        Ok(Ok(resp)) => {
            // Extract mode/warning without consuming response (tuple clone of meta).
            let (mode, warning) = match &resp {
                (_, Json(body)) => (body.mode.clone(), body.warning.clone()),
            };
            crate::server::request_ring::record_context(
                duration_ms,
                &ring_project,
                ring_goal.as_deref(),
                ring_symbol.as_deref(),
                ring_tool.as_deref(),
                mode.as_deref(),
                warning.as_deref(),
                true,
            );
            resp
        }
        Ok(Err(e)) => {
            crate::server::request_ring::record_context(
                duration_ms,
                &ring_project,
                ring_goal.as_deref(),
                ring_symbol.as_deref(),
                ring_tool.as_deref(),
                Some("error"),
                Some(e.as_str()),
                false,
            );
            (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ContextResponse {
                content: format!("Butler error: {}", e),
                selected_count: 0,
                warning: Some(e),
                token_count: None,
                mode: None,
                blocks_omitted: None,
                graph_time_ms: Some(0),
                cached: Some(false),
                total_time_ms: Some(0),
                mermaid: None,
                structured: None,
            }),
        )
        }
        Err(_) => {
            crate::server::request_ring::record_context(
                duration_ms,
                &ring_project,
                ring_goal.as_deref(),
                ring_symbol.as_deref(),
                ring_tool.as_deref(),
                Some("panic"),
                Some("panic caught"),
                false,
            );
            (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ContextResponse {
                content: "Internal server error — check Docker logs".to_string(),
                selected_count: 0,
                warning: Some("panic caught".to_string()),
                token_count: None,
                mode: None,
                blocks_omitted: None,
                graph_time_ms: Some(0),
                cached: Some(false),
                total_time_ms: Some(0),
                mermaid: None,
                structured: None,
            }),
        )
        }
    }
}
