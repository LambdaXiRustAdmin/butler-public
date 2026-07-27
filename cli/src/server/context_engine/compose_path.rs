//! Compose path (P3.1 stage peel S6).
//!
//! Final serve read, lang void / empty graph, full module, zero-copy scope,
//! dispatch / default compose, honest partial banner, cache store.
//! Zero intentional behavior change.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::{http::StatusCode, Json};
use code_graph::snooper::context::OutputFormat;
use code_graph::{compose_context, BlockInfo, ContextOptions};

use crate::server::analysis::{detect_full_files, validate_file_path};
use crate::server::build_status;
use crate::server::dto::*;
use crate::server::mode_intent::is_architectural_summary_orchestrate;
use crate::server::score_audit::log_score_funnel_audit;
use crate::server::state::*;
use crate::vprintln;

use super::building::{building_graph_response_with_policy, cache_context_result};
use super::dispatch::dispatch_tool;
use super::selection_blend_from_settings;
use super::serve_prep::ServePrepReady;
use super::surgical::handle_surgical_mode;

fn count_tokens(s: &str) -> usize {
    code_graph::snooper::token_manager::count_tokens(s)
}

/// Final graph serve read through HTTP response (including cache insert).
pub(super) fn run_compose_path(
    state: &AppState,
    req: &ContextRequest,
    root: &str,
    graph_rw: &Arc<std::sync::RwLock<code_graph::CodeGraph>>,
    ipc_rules: &[code_graph::snooper::ipc_engine::IpcRule],
    force_surgical: bool,
    effective_prompt: &str,
    nl_guidance: &Option<String>,
    prep: &ServePrepReady,
    graph_time_ms: u64,
    is_cached: bool,
    node_count: usize,
    query_key: u64,
    edges_complete: bool,
    edge_percent: usize,
    overall_start: Instant,
) -> Result<(StatusCode, Json<ContextResponse>), String> {
    let ServePrepReady {
        effective_mode,
        is_orchestrate,
        symbol_surgical_trace,
        neural_prompt,
        use_neural_scores,
        symbol_trace_partial_ok,
    } = prep;
    let effective_mode = *effective_mode;
    let is_orchestrate = *is_orchestrate;
    let symbol_surgical_trace = *symbol_surgical_trace;
    let use_neural_scores = *use_neural_scores;
    let symbol_trace_partial_ok = *symbol_trace_partial_ok;

    let graph_guard = match build_status::read_graph_for_serve(state, root, graph_rw) {
        Some(g) => g,
        None => {
            if !symbol_trace_partial_ok {
                if let Some(msg) =
                    build_status::try_lock_contention_building(state, root, graph_rw)
                {
                    return building_graph_response_with_policy(
                        state,
                        root,
                        msg,
                        graph_time_ms,
                        is_cached,
                        overall_start,
                        req.confirm_long_wait.unwrap_or(false),
                    );
                }
            }
            return building_graph_response_with_policy(
                state,
                root,
                build_status::building_graph_message(build_status::percent_for_status(
                    state, root, None,
                )),
                graph_time_ms,
                is_cached,
                overall_start,
                req.confirm_long_wait.unwrap_or(false),
            );
        }
    };
    let graph: &code_graph::CodeGraph = &graph_guard;

    // Lang void: dominant unscanned product lang (java/…) vs crumb inventory.
    if let Some(void) = graph.warehouse_lang_void.as_ref() {
        let final_error = void.user_message(&root);
        return Ok((
            StatusCode::OK,
            Json(crate::server::filters::degenerate_context_response(
                final_error,
                Some(format!(
                    "lang_void:{}:unsup={}:sup={}",
                    void.dominant_ext, void.unsupported_files, void.supported_files
                )),
                None,
                0,
                false,
                overall_start.elapsed().as_millis() as u64,
            )),
        ));
    }

    if node_count == 0 {
        let base_error = format!(
            "=== Butler Error ===\nNo source files found in '{}'. This usually means the path does not contain a Butler-supported language (Rust, Python, TypeScript/JavaScript, Go, C/C++), or is not accessible.\n\nHow to fix (absolute external repos are fully supported):\n- Pass the **full absolute path** in either 'project' or 'root'.\n- Or use 'root' for the absolute directory while using any identifier in 'project'.\n- Java/Kotlin/etc. are not scanned yet — a Complete warehouse with only JS tooling crumbs is a **lang void**, not a product graph.",
            root
        );
        let final_error = if let Some(guidance) = &nl_guidance {
            format!("{}\n\n{}", guidance, base_error)
        } else {
            base_error
        };
        return Ok((
            StatusCode::OK,
            Json(crate::server::filters::degenerate_context_response(
                final_error,
                Some("empty graph - wrong root".to_string()),
                None,
                0,
                false,
                overall_start.elapsed().as_millis() as u64,
            )),
        ));
    }

    // Full module (kept behavior)
    if req.full_module {
        let blend = selection_blend_from_settings(&state.settings);
        let file_path =
            if !super::select_blocks(graph, effective_prompt, use_neural_scores, blend).is_empty() {
                super::select_blocks(graph, effective_prompt, use_neural_scores, blend)[0]
                    .file
                    .clone()
            } else {
                graph
                    .nodes
                    .values()
                    .find(|b| b.file.to_string_lossy().contains(&effective_prompt))
                    .map(|b| b.file.clone())
                    .unwrap_or_else(|| PathBuf::from("unknown.rs"))
            };
        if let Some(valid_path) = validate_file_path(&file_path, Path::new(&root)) {
            if let Ok(full_content) = std::fs::read_to_string(&valid_path) {
                let dump = format!(
                    "=== FULL MODULE DUMP: {} ===\n\n{}\n",
                    valid_path.display(),
                    full_content
                );
                return Ok((
                    StatusCode::OK,
                    Json(ContextResponse {
                        content: dump,
                        selected_count: 1,
                        warning: None,
                        token_count: None,
                        mode: None,
                        blocks_omitted: None,
                        graph_time_ms: Some(graph_time_ms),
                        cached: Some(is_cached),
                        total_time_ms: Some(overall_start.elapsed().as_millis() as u64),
                        mermaid: None,
                        structured: None,
                    }),
                ));
            }
        }
    }

    let is_first_call = effective_prompt.trim().is_empty() || effective_prompt.trim().len() < 3;

    // Architecture / Trace / Find: zero-copy scope (or O(hits) for symbol Trace).
    // Include wants_orchestrate_path so bare /context matches MCP (not select_blocks O(N)).
    let uses_zero_copy_scope = !force_surgical
        && (is_orchestrate
            || req.mcp_tool_name.as_deref() == Some("butler_map")
            || matches!(effective_mode, code_graph::ContextMode::Architecture)
            || (req.mcp_tool_name.as_deref() == Some("butler_search") && is_first_call));

    let capped_max_tokens = req.max_tokens.min(4000);
    let ctx_opts = ContextOptions {
        depth: req.depth,
        max_tokens: capped_max_tokens,
        compress_tests: req.compress_tests,
        format: if effective_prompt.contains("json") {
            OutputFormat::Json
        } else {
            OutputFormat::Markdown
        },
        mode: effective_mode,
        target_file: req.target_file.as_ref().map(PathBuf::from),
        target_line: req.target_line,
        importance_threshold: 0.0,
        scope_paths: req.scope_paths.clone(),
        ignore_paths: req.ignore_paths.clone(),
        use_neural_scores,
        project_root: Some(PathBuf::from(&root)),
    };

    if uses_zero_copy_scope {
        // Symbol Trace: O(name hits) only. Full scoped_block_refs walks every node when
        // first-use ignore_paths is set (gecko 4.8M → multi-second single-thread tax).
        let t_scope = std::time::Instant::now();
        let is_arch_goal = matches!(effective_mode, code_graph::ContextMode::Architecture)
            || is_architectural_summary_orchestrate(&req);

        let scoped_refs = if symbol_surgical_trace {
            let sym = req.target_symbol.as_deref().unwrap_or("").trim();
            let hits = code_graph::snooper::scoped_block_refs_for_symbol(
                graph,
                sym,
                &req.scope_paths,
                &req.ignore_paths,
            );
            if hits.is_empty() {
                // Exact-name miss → O(hits) empty, never O(warehouse). Mid-size repos
                // (xi/gin/bat ≤25k) used to full-scan "because small" and paid 100–400ms
                // worse than large repos that refused — threshold jail for agents.
                vprintln!(
                    "⚡ Surgical Trace: no name_index hits for {:?} (leaf={:?}); \
                     O(1) miss — no full-scope materialize (nodes={})",
                    sym,
                    code_graph::snooper::symbol_name_index_key(sym),
                    graph.nodes.len()
                );
                Vec::new()
            } else {
                let leaf = code_graph::snooper::symbol_name_index_key(sym);
                if leaf != sym && !hits.iter().any(|b| b.name == sym) {
                    vprintln!(
                        "⚡ Surgical Trace: resolved {:?} → leaf {:?} (n_hits={})",
                        sym,
                        leaf,
                        hits.len()
                    );
                }
                hits
            }
        } else if is_arch_goal {
            // Arch: file-level preflight (O(files)) then capped collect — never
            // materialize 3.4M blocks only to refuse.
            use code_graph::snooper::{
                count_files_in_scope, estimate_nodes_in_scope, scoped_block_refs_capped,
                DEFAULT_SCOPE_NODE_CAP,
            };
            let file_hits =
                count_files_in_scope(graph, &req.scope_paths, &req.ignore_paths);
            let est = estimate_nodes_in_scope(graph, file_hits);
            let blank = crate::server::filters::is_blank_scope(&req.scope_paths);
            let monster = graph.nodes.len() > DEFAULT_SCOPE_NODE_CAP;
            let too_big = (blank && monster) || est > DEFAULT_SCOPE_NODE_CAP;
            vprintln!(
                "⏱️  TRACE_PROFILE arch_preflight={:.1}ms file_hits={} est_nodes={} blank={} refuse={}",
                t_scope.elapsed().as_secs_f64() * 1000.0,
                file_hits,
                est,
                blank,
                too_big
            );
            if too_big {
                // Empty scoped → handle_orchestrate Arch hard-rail returns guidance.
                // Attach suggestions via empty scoped path (orchestrate tops from file_hashes).
                Vec::new()
            } else {
                let t_col = std::time::Instant::now();
                let (refs, capped) = scoped_block_refs_capped(
                    graph,
                    &req.scope_paths,
                    &req.ignore_paths,
                    DEFAULT_SCOPE_NODE_CAP,
                );
                let file_local = graph.file_node_index_is_warm()
                    && !crate::server::filters::is_blank_scope(&req.scope_paths);
                vprintln!(
                    "⏱️  TRACE_PROFILE arch_collect={:.1}ms n_scoped={} capped={} file_local={}",
                    t_col.elapsed().as_secs_f64() * 1000.0,
                    refs.len(),
                    capped,
                    file_local
                );
                if capped {
                    vprintln!(
                        "⚡ Arch collect hit cap {} — refuse (no partial skeleton)",
                        DEFAULT_SCOPE_NODE_CAP
                    );
                    Vec::new()
                } else {
                    refs
                }
            }
        } else {
            // Other non-surgical zero-copy (map/search): cap on monsters.
            if graph.nodes.len() > code_graph::snooper::DEFAULT_SCOPE_NODE_CAP {
                let (refs, capped) = code_graph::snooper::scoped_block_refs_capped(
                    graph,
                    &req.scope_paths,
                    &req.ignore_paths,
                    code_graph::snooper::DEFAULT_SCOPE_NODE_CAP,
                );
                if capped {
                    vprintln!(
                        "⚡ scope collect capped at {} (nodes={})",
                        code_graph::snooper::DEFAULT_SCOPE_NODE_CAP,
                        graph.nodes.len()
                    );
                }
                refs
            } else {
                code_graph::snooper::scoped_block_refs(
                    graph,
                    &req.scope_paths,
                    &req.ignore_paths,
                )
            }
        };
        if symbol_surgical_trace || scoped_refs.len() > 50_000 {
            vprintln!(
                "⏱️  TRACE_PROFILE scope_materialize={:.1}ms n_scoped={} symbol_path={}",
                t_scope.elapsed().as_secs_f64() * 1000.0,
                scoped_refs.len(),
                symbol_surgical_trace
            );
        }
        // Symbol Trace does not need ranking the entire scope (81k nodes → multi-second log).
        if !symbol_surgical_trace {
            log_score_funnel_audit(
                graph,
                if is_orchestrate {
                    &neural_prompt
                } else {
                    &effective_prompt
                },
                &scoped_refs,
                use_neural_scores,
                selection_blend_from_settings(&state.settings),
                20,
            );
        }
        if let Some(res) = dispatch_tool(
            req,
            state,
            root,
            graph_rw,
            graph,
            &scoped_refs,
            &ipc_rules,
            &effective_prompt,
            &ctx_opts,
            use_neural_scores,
            graph_time_ms,
            is_cached,
            overall_start,
        ) {
            cache_context_result(state, query_key, &res, edges_complete);
            return res;
        }
    }

    let selected: Vec<BlockInfo> = if force_surgical {
        handle_surgical_mode(req, graph)
    } else {
        super::select_blocks(
            graph,
            effective_prompt,
            use_neural_scores,
            selection_blend_from_settings(&state.settings),
        )
    };

    let selected =
        code_graph::snooper::filter_blocks_by_scope(&selected, &req.scope_paths, &req.ignore_paths);

    // Fail-closed: no strong graph membership → honest miss (no hub filler).
    // Incomplete warehouse → provisional miss (never no_structural_hits).
    // ArchitecturalSummary uses scope/hubs/skeleton — empty prompt is OK when
    // goal maps to Architecture (bare /context used to stay Balanced → miss).
    let arch_allows_empty_prompt = matches!(effective_mode, code_graph::ContextMode::Architecture)
        || is_architectural_summary_orchestrate(&req);
    if selected.is_empty() && !force_surgical && !arch_allows_empty_prompt {
        let q = code_graph::resolve_structural_query(graph, &effective_prompt);
        if !q.has_usable_hits() {
            let unmatched: Vec<String> = q.strong_unmatched.iter().take(8).cloned().collect();
            if !edges_complete {
                let pct = edge_percent.min(99);
                let msg = cli::butler_instructions::symbol_not_seen_yet_miss(
                    root,
                    &effective_prompt,
                    pct,
                    &unmatched,
                );
                vprintln!(
                    "🔎 symbol_not_seen_yet@{}% project={root} (provisional; not no_structural_hits)",
                    pct
                );
                return Ok((
                    StatusCode::OK,
                    Json(crate::server::filters::degenerate_context_response(
                        msg,
                        Some(build_status::provisional_miss_token(pct)),
                        Some("partial_miss".to_string()),
                        graph_time_ms,
                        is_cached,
                        overall_start.elapsed().as_millis() as u64,
                    )),
                ));
            }
            let msg = cli::butler_instructions::dense_structural_miss(
                root,
                &effective_prompt,
                &unmatched,
            );
            vprintln!("🔎 no_structural_hits project={root}");
            return Ok((
                StatusCode::OK,
                Json(crate::server::filters::degenerate_context_response(
                    msg,
                    Some("no_structural_hits".to_string()),
                    Some("miss".to_string()),
                    graph_time_ms,
                    is_cached,
                    overall_start.elapsed().as_millis() as u64,
                )),
            ));
        }
    }

    let selected_refs: Vec<&BlockInfo> = selected.iter().collect();
    log_score_funnel_audit(
        graph,
        &effective_prompt,
        &selected_refs,
        use_neural_scores,
        selection_blend_from_settings(&state.settings),
        20,
    );

    // Dispatch (early return on special tools — keyword search with prompt)
    if let Some(res) = dispatch_tool(
        req,
        state,
        root,
        graph_rw,
        graph,
        &selected_refs,
        &ipc_rules,
        &effective_prompt,
        &ctx_opts,
        use_neural_scores,
        graph_time_ms,
        is_cached,
        overall_start,
    ) {
        cache_context_result(state, query_key, &res, edges_complete);
        return res;
    }

    // Default compose (Sprint 9: hard payload cap before composition)
    let max_blocks = state.settings.analysis.max_context_blocks;
    let (selected, payload_omitted) =
        crate::server::filters::cap_blocks_by_score(selected, max_blocks);
    let work_start = Instant::now();
    let composed = compose_context(graph, selected.clone(), &ctx_opts, &effective_prompt);
    let work_time = work_start.elapsed();
    let mut content = composed.text;
    // Honest partial banner when warehouse still grinding.
    if !edges_complete {
        let exact = req
            .target_symbol
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| !graph.blocks_for_name(s).is_empty())
            .unwrap_or(false);
        let seed_edged = req
            .target_symbol
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                graph
                    .files_for_name(s)
                    .iter()
                    .any(|f| graph.file_has_edges(f))
            })
            .unwrap_or(false);
        let rung = build_status::confidence_rung(
            state,
            root,
            Some(graph),
            exact,
            seed_edged,
        );
        let banner = build_status::honest_partial_banner(
            edge_percent,
            rung,
            Some("serve-while-grind"),
        );
        if !banner.is_empty() {
            content = format!("{banner}\n\n{content}");
        }
    }
    vprintln!(
        "🧠 Selection + Composition completed in {:.2?} | selected={}",
        work_time,
        selected.len()
    );

    // Lightweight hits (kept, reference based)
    let mut token_est = count_tokens(&content);
    let lower_prompt = effective_prompt.to_lowercase();
    let keywords: Vec<&str> = lower_prompt
        .split_whitespace()
        .filter(|w| {
            w.len() > 2
                && ![
                    "the", "and", "for", "with", "use", "butler", "context", "analyze",
                ]
                .contains(w)
        })
        .collect();

    for block in &selected {
        let mut hits_set: HashSet<String> = HashSet::new();
        for kw in &keywords {
            if block.name.to_lowercase().contains(kw) || block.source.to_lowercase().contains(kw) {
                let hit = format!(
                    "line {} in {}",
                    block.start_line,
                    block.file.file_name().unwrap_or_default().to_string_lossy()
                );
                hits_set.insert(hit);
            }
        }
        if !hits_set.is_empty() {
            let hits: Vec<String> = hits_set.into_iter().take(3).collect();
            let extra = format!("Additional hits: {}\n\n", hits.join(", "));
            let extra_tokens = count_tokens(&extra);
            if token_est + extra_tokens <= capped_max_tokens {
                content.push_str(&extra);
                token_est += extra_tokens;
            }
        }
    }

    let full_files = detect_full_files(&effective_prompt, graph);
    for (path, raw_code) in full_files {
        let raw_section = format!("=== FULL RAW FILE: {} ===\n\n{}\n\n---\n\n", path, raw_code);
        let section_tokens = count_tokens(&raw_section);
        if token_est + section_tokens > capped_max_tokens {
            break;
        }
        content.push_str(&raw_section);
        token_est += section_tokens;
    }

    let final_content = if let Some(guidance) = &nl_guidance {
        format!("{}\n\n{}", guidance, content)
    } else {
        content
    };

    let mode_str = if edges_complete {
        format!("{:?}", composed.mode)
    } else {
        format!("{:?}+partial", composed.mode)
    };
    let response = ContextResponse {
        content: final_content,
        selected_count: selected.len(),
        warning: if edges_complete {
            None
        } else {
            Some(format!("honest_partial@{}%", edge_percent.min(99)))
        },
        token_count: Some(composed.token_count),
        mode: Some(mode_str),
        blocks_omitted: Some(payload_omitted + composed.blocks_omitted.len()),
        graph_time_ms: Some(graph_time_ms),
        cached: Some(is_cached),
        total_time_ms: Some(overall_start.elapsed().as_millis() as u64),
        mermaid: Some(format!(
            "graph TD;\n    Target[{}] --> Caller;\n    Target --> Callee;",
            "target"
        )),
        structured: None,
    };

    // Only cache complete-warehouse answers (honest partial never frozen).
    if edges_complete {
        if let Ok(mut qc) = state.query_cache.lock() {
            qc.insert(query_key, response.clone());
        }
    }

    Ok((StatusCode::OK, Json(response)))
}
