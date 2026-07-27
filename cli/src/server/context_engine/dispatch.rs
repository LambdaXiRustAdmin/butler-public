//! Tool dispatch for context composition (P3 peel).
//! Zero intentional behavior change.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use axum::{http::StatusCode, Json};
use code_graph::{BlockInfo, ContextOptions};

use crate::server::dto::*;
use crate::server::mode_intent::wants_orchestrate_path;
use crate::server::orchestrate::*;
use crate::server::render::{format_search_results_markdown, render_skeleton};
use crate::server::state::*;

use super::selection_blend_from_settings;

pub(super) fn dispatch_tool(
    req: &ContextRequest,
    state: &AppState,
    root: &str,
    graph_rw: &Arc<std::sync::RwLock<code_graph::CodeGraph>>,
    graph: &code_graph::CodeGraph,
    scoped: &[&BlockInfo],
    ipc_rules: &[code_graph::snooper::ipc_engine::IpcRule],
    effective_prompt: &str,
    _ctx_opts: &ContextOptions,
    use_neural_scores: bool,
    graph_time_ms: u64,
    is_cached: bool,
    overall_start: Instant,
) -> Option<Result<(StatusCode, Json<ContextResponse>), String>> {
    if wants_orchestrate_path(req) {
        let selected_len = scoped.len();
        let orchestrate_out = handle_orchestrate(
            req,
            state,
            root,
            graph_rw,
            scoped,
            graph,
            ipc_rules,
            use_neural_scores,
            selection_blend_from_settings(&state.settings),
            overall_start.elapsed().as_millis() as u64,
        );
        let token_count = (orchestrate_out.content.len() / 4).max(1);
        let selected_count = orchestrate_out
            .structured
            .as_ref()
            .map(|st| {
                if st.target.is_some() {
                    1 + st.callers.len() + st.callees.len()
                } else {
                    let sk = st.skeleton.as_ref().map(|s| s.len()).unwrap_or(0);
                    let hubs = st.hubs.as_ref().map(|h| h.len()).unwrap_or(0);
                    sk + hubs
                }
            })
            .unwrap_or(selected_len);
        let structured_json = orchestrate_out
            .structured
            .as_ref()
            .map(crate::server::orchestrate::structured_report_to_value);
        return Some(Ok((
            StatusCode::OK,
            Json(ContextResponse {
                content: orchestrate_out.content,
                selected_count,
                warning: None,
                token_count: Some(token_count),
                mode: Some("orchestrate".to_string()),
                blocks_omitted: None,
                graph_time_ms: Some(graph_time_ms),
                cached: Some(is_cached),
                total_time_ms: Some(overall_start.elapsed().as_millis() as u64),
                mermaid: orchestrate_out.mermaid,
                structured: structured_json,
            }),
        )));
    }

    if req.mcp_tool_name.as_deref() == Some("butler_map")
        || (req.mcp_tool_name.as_deref() == Some("butler_search")
            && (effective_prompt.trim().is_empty() || effective_prompt.trim().len() < 3))
    {
        let mut proj_settings = state.settings.clone();
        proj_settings.merge_project_config(Path::new(root));
        let noise_cfg =
            crate::server::filters::NoiseFilterConfig::from_analysis(&proj_settings.analysis);
        let max_blocks = proj_settings.analysis.max_context_blocks;
        let (capped, omitted) = crate::server::filters::cap_block_refs(scoped.to_vec(), max_blocks);
        let skeleton = render_skeleton(&capped, graph, Path::new(root), &noise_cfg);
        let token_count = code_graph::snooper::token_manager::count_tokens(&skeleton);
        return Some(Ok((StatusCode::OK, Json(ContextResponse {
            content: skeleton,
            selected_count: capped.len(),
            warning: None,
            token_count: Some(token_count),
            mode: Some("skeleton".to_string()),
            blocks_omitted: if omitted > 0 { Some(omitted) } else { None },
            graph_time_ms: Some(graph_time_ms),
            cached: Some(is_cached),
            total_time_ms: Some(overall_start.elapsed().as_millis() as u64),
            mermaid: Some("graph TD;\n    Project[Project Scope] --> Files[Files & Symbols];\n    Files --> Drill[Click tree to drill scope];".to_string()),
            structured: None,
        }))));
    }

    if req.mcp_tool_name.as_deref() == Some("butler_search") {
        let max_r = req
            .max_results
            .min(state.settings.analysis.max_context_blocks);
        let (capped, omitted) = crate::server::filters::cap_block_refs(scoped.to_vec(), max_r);
        let top: Vec<BlockInfo> = capped.into_iter().map(|b| (*b).clone()).collect();
        let md = format_search_results_markdown(&top, Some(graph));
        let delivered_tokens = code_graph::snooper::token_manager::count_tokens(&md);
        return Some(Ok((
            StatusCode::OK,
            Json(ContextResponse {
                content: md,
                selected_count: top.len(),
                warning: None,
                token_count: Some(delivered_tokens),
                mode: Some("search".to_string()),
                blocks_omitted: if omitted > 0 { Some(omitted) } else { None },
                graph_time_ms: Some(graph_time_ms),
                cached: Some(is_cached),
                total_time_ms: Some(overall_start.elapsed().as_millis() as u64),
                mermaid: None,
                structured: None,
            }),
        )));
    }

    None
}

