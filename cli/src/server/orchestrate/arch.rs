//! ArchitecturalSummary pipeline (skeleton → hubs → bridges/clusters).
//!
//! Extracted from `handle_orchestrate` (M1b) — zero intentional behavior change.

use super::disambiguate::{sanitize_scope_prefix, suggested_scopes_from_paths};
use super::render::ContentDetail;
use super::{
    arch_scope_refused_output, bridge_infos, cluster_infos_from_scoped, collect_unique_hubs,
    hub_cap_for_summary, inject_response_telemetry, make_state_info, OrchestrateOutput,
};
use crate::server::build_status;
use crate::server::dto::*;
use crate::server::render::render_2level_file_tree;
use crate::server::sniffer::sniff_direct_dependencies;
use crate::server::state::AppState;
use crate::vprintln;
use code_graph::{BlockInfo, CodeGraph, NeuralSelectionBlend};
use std::path::Path;
use std::time::Instant;

/// Run ArchitecturalSummary. `Err` is a full early [`OrchestrateOutput`] (scope refuse).
pub(crate) fn run_architectural_summary(
    req: &ContextRequest,
    state: &AppState,
    root: &str,
    scoped: &[&BlockInfo],
    graph: &CodeGraph,
    use_neural_scores: bool,
    _blend: NeuralSelectionBlend,
    edge_build_label: &str,
    jit_note: &str,
    conf_full_or_inv: build_status::ConfidenceRung,
    bg_pct: usize,
    bg_status: &str,
    blocks_scanned: usize,
    total_time_ms: u64,
) -> Result<StructuredReport, OrchestrateOutput> {
    let root_path = Path::new(root);
    let pp = code_graph::ProjectPaths::new(root_path);
    let noise_cfg = {
        let mut settings = state.settings.clone();
        settings.merge_project_config(root_path);
        crate::server::filters::NoiseFilterConfig::from_analysis(&settings.analysis)
    };
    // ArchitecturalSummary (explicit). Populate skeleton + hubs for LLM structural data (Fix).
    let mut suggested = vec![];

    let total_project_blocks = graph.nodes.len();
    // Hard rail (post-collect backup): prefer context_engine file preflight so we never
    // pay O(nodes) just to refuse.
    const ARCH_SCOPED_HARD: usize = 80_000;
    if scoped.len() > ARCH_SCOPED_HARD {
        return Err(arch_scope_refused_output(
            graph,
            root_path,
            &pp,
            scoped.len(),
            total_project_blocks,
            edge_build_label,
            jit_note,
            conf_full_or_inv,
            bg_pct,
            "post-collect",
        ));
    }

    let is_tiny_project = total_project_blocks < 50;
    // Execution planner: compact / mid-size → Table of Contents (no bridges/clusters).
    // dense + small scope keeps richer path. Never early-stop HashMap collect (Gem C3).
    let detail = ContentDetail::from_req(req.detail.as_deref());
    let toc_mode = !matches!(detail, ContentDetail::Dense) || scoped.len() > 500;
    let t_arch = Instant::now();
    let mut arch_prof: Vec<(&str, f64)> = Vec::with_capacity(8);

    let max_blocks = state.settings.analysis.max_context_blocks;
    let direct_dependencies = if toc_mode {
        // Root manifests only; skip on TOC (already paid once if needed).
        String::new()
    } else {
        sniff_direct_dependencies(root_path).unwrap_or_default()
    };

    // --- Skeleton (files TOC) ---
    let t_sk = Instant::now();
    let mut all_files: Vec<String> =
        scoped.iter().map(|b| pp.to_display(&b.file)).collect();
    all_files.sort();
    all_files.dedup();
    let original_unique_count = all_files.len();

    let skeleton_files: Vec<String> = if toc_mode || is_tiny_project {
        // TOC: unique files only (path list) — no per-block noise rewalk for ranking.
        all_files
    } else {
        let mut entry_files: Vec<String> = scoped
            .iter()
            .copied()
            .filter(|b| crate::server::filters::is_entry_landmark(b, root_path))
            .map(|b| pp.to_display(&b.file))
            .collect();
        entry_files.sort();
        entry_files.dedup();
        let mut rest: Vec<String> = scoped
            .iter()
            .copied()
            .filter(|b| !crate::server::filters::is_noise(b, root_path, &noise_cfg))
            .map(|b| pp.to_display(&b.file))
            .collect();
        rest.sort();
        rest.dedup();
        let mut merged = entry_files;
        for f in rest {
            if !merged.iter().any(|e| e == &f) {
                merged.push(f);
            }
        }
        merged
    };
    let pruned_count = original_unique_count.saturating_sub(skeleton_files.len());

    // True file inventory under scope *before* any rollup (coverage honesty).
    let inventory_file_count = skeleton_files
        .iter()
        .filter(|p| !p.starts_with("[..."))
        .count();

    // Adaptive horizon: bat-sized scopes stay full; Gecko-scale maps roll up earlier.
    // Explicit scope_paths raise the budget (agent asked for that subtree).
    let explicit_scope = !crate::server::filters::is_blank_scope(&req.scope_paths);
    let skeleton_budget = arch_skeleton_full_budget(
        inventory_file_count,
        explicit_scope,
        total_project_blocks,
    );
    let (mut skeleton_files, skeleton_rolled_up, mut skeleton_summary) =
        maybe_collapse_arch_skeleton(skeleton_files, skeleton_budget);

    if pruned_count > 0 && !toc_mode {
        skeleton_files.push(format!(
            "[... Pruned {} test/doc/example files for brevity. Use scope_paths to view them directly.]",
            pruned_count
        ));
    }
    // Dense + blank tiny projects: 2-level FS tree for orientation.
    if !toc_mode
        && crate::server::filters::is_blank_scope(&req.scope_paths)
        && is_tiny_project
        && skeleton_summary.is_none()
    {
        skeleton_summary = Some(render_2level_file_tree(scoped));
    }
    arch_prof.push(("skeleton", t_sk.elapsed().as_secs_f64() * 1000.0));

    // --- Hubs ---
    let t_hub = Instant::now();
    let total_nodes = graph.nodes.len();
    const ARCH_DEGREE_POOL_SOFT: usize = 400;
    let hub_cap = hub_cap_for_summary(max_blocks, state.settings.analysis.hub_budget_pct)
        .max(8)
        .min(max_blocks.max(8));

    let hubs: Vec<Hub> = if toc_mode {
        // Table of Contents: top-N by directed in-degree (+ entry landmarks). No edge walk,
        // no source FFI scan, no neural. O(K log K) on scoped set only.
        // Drop test/sample helpers so map hubs look product-facing (agent trust).
        let mut scored: Vec<(&BlockInfo, i64)> = scoped
            .iter()
            .copied()
            .filter(|b| !crate::server::filters::is_noise(b, root_path, &noise_cfg))
            .filter(|b| !crate::server::filters::is_testish_seed_block(b))
            .filter(|b| !arch_map_hub_name_noise(&b.name))
            .map(|b| {
                let entry = crate::server::filters::is_entry_landmark(b, root_path);
                let indeg = crate::server::filters::directed_in_degree(graph, b);
                let outdeg = graph.edges.get(&b.id).map_or(0, |v| v.len());
                let prio = arch_map_hub_priority(b, root_path, indeg, outdeg, entry);
                (b, prio)
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.name.cmp(&b.0.name))
        });
        scored.truncate(hub_cap.max(ARCH_DEGREE_POOL_SOFT.min(64)));
        collect_unique_hubs_scored(
            scored.into_iter().map(|(b, prio)| (b, prio as f64)),
            graph,
            &pp,
            hub_cap,
        )
    } else {
        let is_ffi_bridge = |block: &BlockInfo| -> bool {
            let src = &block.source;
            !src.is_empty()
                && (src.contains("pybind11")
                    || src.contains("#[pymethods]")
                    || src.contains("#[pyclass]")
                    || src.contains("PYBIND11_MODULE")
                    || src.contains("EMSCRIPTEN_KEEPALIVE")
                    || src.contains("extern \"C\"")
                    || src.contains("JNIEXPORT")
                    || src.contains("export "))
        };
        let mut entry_hubs: Vec<&BlockInfo> = Vec::new();
        let mut degree_hubs: Vec<&BlockInfo> = Vec::new();
        for b in scoped.iter().copied() {
            if crate::server::filters::is_noise(b, root_path, &noise_cfg) {
                continue;
            }
            if crate::server::filters::is_testish_seed_block(b)
                || arch_map_hub_name_noise(&b.name)
            {
                continue;
            }
            let in_degree = graph.reverse.get(&b.id).map_or(0, |v| v.len());
            let out_degree = graph.edges.get(&b.id).map_or(0, |v| v.len());
            if crate::server::filters::is_hub_primitive(
                &b.name,
                in_degree,
                out_degree,
                total_nodes,
            ) && !crate::server::filters::is_entry_landmark(b, root_path)
            {
                continue;
            }
            if crate::server::filters::is_entry_landmark(b, root_path) {
                entry_hubs.push(b);
                continue;
            }
            let hub_eligible = graph.highly_connected_nodes.contains(&b.id)
                || (crate::server::filters::is_architectural_kind(&b.kind)
                    && in_degree + out_degree >= 2)
                || (use_neural_scores
                    && crate::server::filters::is_architectural_kind(&b.kind)
                    && in_degree + out_degree >= 1);
            if hub_eligible {
                degree_hubs.push(b);
            }
        }
        let arch_score = |block: &BlockInfo| -> f64 {
            let mut s = if crate::server::filters::is_vendored(&block.file) {
                block.score * 0.1
            } else {
                block.score
            };
            if is_ffi_bridge(block) {
                s *= 10.0;
            }
            let in_d = graph.reverse.get(&block.id).map_or(0, |v| v.len());
            let out_d = graph.edges.get(&block.id).map_or(0, |v| v.len());
            s *= crate::server::filters::structural_multiplier(
                &block.kind,
                in_d,
                out_d,
                total_nodes,
            );
            s *= crate::server::filters::entry_point_multiplier(block, root_path);
            let f = block.file.to_string_lossy().to_ascii_lowercase();
            if f.contains("/test/") || f.contains("/tests/") {
                s *= 0.15;
            }
            s
        };
        degree_hubs.sort_by(|a, b| {
            arch_score(b)
                .partial_cmp(&arch_score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if degree_hubs.len() > ARCH_DEGREE_POOL_SOFT {
            degree_hubs.truncate(ARCH_DEGREE_POOL_SOFT);
        }
        entry_hubs.sort_by(|a, b| {
            arch_score(b)
                .partial_cmp(&arch_score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut scoped_hubs: Vec<&BlockInfo> =
            Vec::with_capacity(entry_hubs.len() + degree_hubs.len());
        scoped_hubs.extend(entry_hubs);
        scoped_hubs.extend(degree_hubs);
        collect_unique_hubs(scoped_hubs, graph, &pp, hub_cap)
    };
    arch_prof.push(("hubs", t_hub.elapsed().as_secs_f64() * 1000.0));

    // Safety payload cap only when rollup still produced a huge *row* list (many dirs).
    let skeleton_row_n = skeleton_files
        .iter()
        .filter(|p| !p.starts_with("[..."))
        .count();
    let (skeleton_files, skeleton_payload_omitted) =
        if skeleton_row_n <= skeleton_budget.max(80) {
            (skeleton_files, 0usize)
        } else {
            let row_budget = max_blocks
                .saturating_sub(hubs.len())
                .max(32)
                .max(max_blocks)
                .max(80)
                .max(skeleton_budget);
            crate::server::filters::cap_string_payload(skeleton_files, row_budget)
        };

    // When rolled up, surface collapsed dirs as suggested_scopes so agents re-Arch
    // instead of list_dir.
    if skeleton_rolled_up && suggested.is_empty() {
        suggested = suggested_scopes_from_rollup_rows(
            root_path,
            skeleton_files.iter().map(|s| s.as_str()),
            6,
        );
    }
    if suggested.is_empty() && scoped.len() > 150 {
        suggested = suggested_scopes_from_paths(
            root_path,
            hubs.iter().map(|h| h.file.as_str()),
            3,
        );
    }

    let pruned_message = if pruned_count > 0 && !toc_mode {
        Some(format!(
            "[... Pruned {} test/doc/example files for brevity. Use scope_paths to view them directly.]",
            pruned_count
        ))
    } else {
        None
    };

    // Bridges / clusters:
    // - dense: full clusters + bridges
    // - TOC/compact (P3): skip clusters (latency), still surface a few dual-stack
    //   interconnect bridges so Arch compact shows Export/Ipc fabric (word-count/tauri).
    let t_br = Instant::now();
    let (clusters, bridges) = if toc_mode {
        let bridges = bridge_infos(graph, scoped, root_path, 8);
        (Vec::new(), bridges)
    } else {
        let clusters = cluster_infos_from_scoped(scoped);
        let bridges = bridge_infos(graph, scoped, root_path, 16);
        if suggested.is_empty() && !clusters.is_empty() {
            if let Some(top) = clusters.first() {
                suggested = suggested_scopes_from_paths(
                    root_path,
                    top.entries.iter().map(|s| s.as_str()),
                    3,
                );
                if suggested.is_empty() {
                    if let Some(sum) = code_graph::summarize_clusters(scoped.iter().copied())
                        .into_iter()
                        .next()
                    {
                        suggested = code_graph::suggested_scopes_for_cluster(&sum)
                            .into_iter()
                            .filter_map(|s| sanitize_scope_prefix(root_path, &s))
                            .collect();
                    }
                }
            }
        }
        (clusters, bridges)
    };
    arch_prof.push(("bridges_clusters", t_br.elapsed().as_secs_f64() * 1000.0));
    arch_prof.push(("total_arch", t_arch.elapsed().as_secs_f64() * 1000.0));
    {
        let parts: Vec<String> = arch_prof
            .iter()
            .map(|(k, ms)| format!("{k}={ms:.1}ms"))
            .collect();
        vprintln!(
            "⏱️  TRACE_PROFILE arch toc={} n_scoped={} | {}",
            toc_mode,
            scoped.len(),
            parts.join(" ")
        );
    }

    let payload_blocks = skeleton_files.len() + hubs.len();
    let skel_n = skeleton_files
        .iter()
        .filter(|p| !p.starts_with("[..."))
        .count();
    // Complete = every *file* under scope is listed as a basename row — never a rollup.
    let coverage_complete = arch_coverage_complete(
        inventory_file_count,
        skeleton_rolled_up,
        skeleton_payload_omitted,
    );
    let mut arch_telemetry = serde_json::json!({
        "type": "architectural",
        "toc_mode": toc_mode,
        "large": scoped.len() > 150,
        "block_count": scoped.len(),
        "payload_blocks": payload_blocks,
        "max_context_blocks": max_blocks,
        "hub_budget_pct": state.settings.analysis.hub_budget_pct,
        "payload_omitted": skeleton_payload_omitted,
        "skeleton_files": skel_n,
        "unique_files_under_scope": inventory_file_count,
        "skeleton_rolled_up": skeleton_rolled_up,
        "skeleton_full_max": skeleton_budget,
        "skeleton_budget_explicit_scope": explicit_scope,
        "coverage_complete": coverage_complete,
        "pruned_message": pruned_message,
        "direct_dependencies": direct_dependencies,
        "skeleton_summary": skeleton_summary
    });
    inject_response_telemetry(
        &mut arch_telemetry,
        blocks_scanned,
        total_time_ms,
        payload_blocks,
    );

    let next_action = if coverage_complete {
        Some(
            "format this map (tree+hubs); Trace a hub with scope_paths — do not list_dir/recursive-read unless coverage_complete=false"
                .into(),
        )
    } else if skeleton_rolled_up {
        Some(
            "skeleton rolled up — re-Arch with scope_paths on a listed directory for full basenames; prefer suggested_scopes over list_dir"
                .into(),
        )
    } else {
        Some(
            "skeleton truncated for size — narrow scope_paths or detail=dense; avoid full-tree file walk"
                .into(),
        )
    };

    return Ok(StructuredReport {
        state: make_state_info(
            format!("{}% | {}", bg_pct, bg_status),
            jit_note.to_string(),
            conf_full_or_inv,
            bg_pct,
        ),
        error: None,
        target: None,
        callers: vec![],
        callees: vec![],
        caller_path: vec![],
        peer_callers: vec![],
        bridge_callers: vec![],
        bridge_callees: vec![],
        blast_domain: None,
        seed_kind: None,
        receipt: None,
        next_action,
        telemetry: arch_telemetry,
        suggested_scopes: suggested,
        skeleton: Some(skeleton_files),
        hubs: Some(hubs),
        module_resolved_from: None,
        module_interior_candidates: None,
        locations: None,
        clusters: if clusters.is_empty() {
            None
        } else {
            Some(clusters)
        },
        bridges: if bridges.is_empty() {
            None
        } else {
            Some(bridges)
        },
        active_cluster: None,
    })
}

/// Absolute ceiling: even a scoped Gecko slice must not dump novel-length trees into content.
pub(crate) const ARCH_SKELETON_CEILING: usize = 300;
/// Always keep full basenames at or below this (bat-sized scopes).
pub(crate) const ARCH_SKELETON_FLOOR: usize = 48;

/// Adaptive full-basename budget for Arch maps.
///
/// - **Tiny inventory** → always full (bat).
/// - **Explicit `scope_paths`** → higher budget (agent asked for that subtree).
/// - **Blank / whole-repo feel on mega warehouses** → roll up sooner (Gecko ≠ 1k-line reply).
/// - Hard **ceiling** so no path dumps unbounded trees.
///
/// Not a single magic number: rollup stays honest via [`arch_coverage_complete`].
pub(crate) fn arch_skeleton_full_budget(
    inventory: usize,
    explicit_scope: bool,
    warehouse_nodes: usize,
) -> usize {
    if inventory <= ARCH_SKELETON_FLOOR {
        return ARCH_SKELETON_FLOOR;
    }
    let mega = warehouse_nodes >= 80_000;
    let large = warehouse_nodes >= 20_000;
    let base = match (explicit_scope, mega, large) {
        (true, true, _) => 180,  // scoped Gecko-scale
        (true, _, true) => 220,  // scoped large
        (true, _, _) => 260,     // scoped mid (rich/click/server packages)
        (false, true, _) => 56,  // blank mega → force dir map + re-scope
        (false, _, true) => 80,
        (false, _, _) => 120, // blank mid/small whole project
    };
    // Slack so mid packages aren't one file off a cliff (explicit scopes only).
    let with_slack = if explicit_scope {
        (base + base / 5).min(ARCH_SKELETON_CEILING)
    } else {
        base
    };
    // If inventory already fits under budget, no need to go higher.
    with_slack
        .max(ARCH_SKELETON_FLOOR)
        .min(ARCH_SKELETON_CEILING)
}

/// Rollup row shape: `"path/to/dir/ (N files)"`.
fn is_rollup_skeleton_row(p: &str) -> bool {
    let p = p.trim();
    if let Some(i) = p.rfind(" (") {
        let rest = &p[i + 2..];
        return rest.ends_with(" files)")
            && rest
                .trim_end_matches(" files)")
                .chars()
                .all(|c| c.is_ascii_digit());
    }
    false
}

/// `coverage_complete` only when the inventory is fully listed as basenames (no rollup/omit).
pub(crate) fn arch_coverage_complete(
    inventory_file_count: usize,
    rolled_up: bool,
    payload_omitted: usize,
) -> bool {
    !rolled_up && payload_omitted == 0 && inventory_file_count > 0
}

/// Collapse long file lists into directory counts + a few entry landmarks.
/// Returns `(rows, rolled_up, summary)`.
fn maybe_collapse_arch_skeleton(
    skeleton_files: Vec<String>,
    budget: usize,
) -> (Vec<String>, bool, Option<String>) {
    let n = skeleton_files
        .iter()
        .filter(|p| !p.starts_with("[..."))
        .count();
    if n <= budget {
        return (skeleton_files, false, None);
    }
    let mut dir_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut entry_keep: Vec<String> = Vec::new();
    for f in &skeleton_files {
        if f.starts_with("[...") {
            continue;
        }
        let base = std::path::Path::new(f)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        // Landmarks kept as basenames so rollup still has orientation pins.
        if matches!(
            base,
            "emcc.py"
                | "em++.py"
                | "main.rs"
                | "main.py"
                | "main.go"
                | "lib.rs"
                | "mod.rs"
                | "__main__.py"
                | "link.py"
                | "compile.py"
                | "building.py"
                | "emscripten.py"
                | "app.py"
                | "server.py"
                | "cli.py"
                | "index.ts"
                | "index.js"
        ) {
            entry_keep.push(f.clone());
            continue;
        }
        if let Some(parent) = std::path::Path::new(f).parent() {
            let mut dir = parent.to_string_lossy().to_string();
            if !dir.ends_with('/') {
                dir.push('/');
            }
            *dir_counts.entry(dir).or_insert(0) += 1;
        }
    }
    let mut collapsed: Vec<String> = dir_counts
        .iter()
        .map(|(d, c)| format!("{} ({} files)", d, c))
        .collect();
    collapsed.sort();
    let mut out = entry_keep;
    out.extend(collapsed);
    let summary = format!(
        "TOC: {} files under scope; kept entry landmarks + collapsed into {} directories (adaptive budget {}). Narrow scope_paths for full basenames.",
        n,
        dir_counts.len(),
        budget
    );
    (out, true, Some(summary))
}

/// Parse rollup rows into **repo-relative** scope prefixes (never host-absolute).
fn suggested_scopes_from_rollup_rows(
    root: &Path,
    rows: impl IntoIterator<Item = impl AsRef<str>>,
    max: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    for r in rows {
        let r = r.as_ref();
        if !is_rollup_skeleton_row(r) {
            continue;
        }
        if let Some(i) = r.rfind(" (") {
            let dir = r[..i].trim();
            let Some(scope) = sanitize_scope_prefix(root, dir) else {
                continue;
            };
            if !out.iter().any(|e| e == &scope) {
                out.push(scope);
            }
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Names that dominate degree rank but are not product map hubs (tests, samples, tiny helpers).
fn arch_map_hub_name_noise(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("test_")
        || n.starts_with("sample_")
        || n.ends_with("_test")
        || n.ends_with("_tests")
        || n == "test_block"
        || n == "sample_trace"
        || n.starts_with("fixture")
}

/// Soft path demotion for Arch hubs (structure, not product keyword lists).
///
/// Soft multipliers only — real CLIs in `__main__.py` can still win on high in-degree.
fn arch_map_hub_path_soft_delta(file: &str) -> i64 {
    let f = file.replace('\\', "/").to_ascii_lowercase();
    let mut d = 0i64;
    let base = f.rsplit('/').next().unwrap_or(f.as_str());
    // Demo / script entry files: high out-degree often, weak product authority.
    if base == "__main__.py" {
        d -= 8_000;
    }
    // Common non-product trees (language-agnostic path roles).
    if f.contains("/tests/")
        || f.contains("/test/")
        || f.contains("/examples/")
        || f.contains("/example/")
        || f.contains("/benchmarks/")
        || f.contains("/benches/")
        || f.contains("/docs/")
        || f.contains("/doc/")
    {
        d -= 10_000;
    }
    d
}

/// Product-facing hub rank for Arch maps — **authority (in-degree)** first, not undirected fan-out.
fn arch_map_hub_priority(
    b: &BlockInfo,
    root: &Path,
    indeg: usize,
    outdeg: usize,
    entry: bool,
) -> i64 {
    let mut s = indeg as i64;
    let n = b.name.as_str();
    let nl = n.to_ascii_lowercase();
    let file_s = b.file.to_string_lossy();
    let base = std::path::Path::new(file_s.as_ref())
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("");

    // Entry landmarks help orientation, but `__main__.py` is often a demo harness in libs.
    if entry {
        if base.eq_ignore_ascii_case("__main__.py") {
            s += 3_000;
        } else {
            s += 12_000;
        }
    }
    s += arch_map_hub_path_soft_delta(file_s.as_ref());

    // Demo signature: imports the world, few importers (high out, low in).
    if outdeg >= 8 && indeg <= 2 {
        s -= 5_000;
    }

    if crate::server::filters::is_architectural_kind(&b.kind) {
        s += 3_000;
    }
    // Light generic entry verbs (not Butler-specific symbol names).
    if nl.starts_with("handle_") || nl.starts_with("run_") {
        s += 4_000;
    }
    // Downrank tiny report helpers that dominate same-file CALL graphs
    if nl.starts_with("make_")
        || nl.starts_with("is_live_")
        || nl.ends_with("_line")
        || nl.ends_with("_of")
        || nl.contains("hop_split")
        || nl.contains("frame_line")
        || nl.contains("not_found_message")
        || nl.contains("error_structured")
        || nl.contains("lang_cluster")
    {
        s -= 8_000;
    }
    // Prefer defs that look like real API surface
    let k = b.kind.to_ascii_lowercase();
    if k.contains("function") || k.contains("method") {
        s += 500;
    }
    if k.contains("class") || k.contains("struct") || k.contains("type") {
        s += 1_500;
    }
    let _ = root;
    s
}

/// Like [`super::collect_unique_hubs`] but preserves explicit map scores for content ranking.
fn collect_unique_hubs_scored<'a>(
    ranked: impl IntoIterator<Item = (&'a BlockInfo, f64)>,
    graph: &CodeGraph,
    pp: &code_graph::ProjectPaths,
    max_hubs: usize,
) -> Vec<Hub> {
    use crate::server::paths::format_project_path;
    use super::lang_cluster_of;
    use std::collections::HashSet;
    let mut hubs = Vec::with_capacity(max_hubs);
    let mut seen_names: HashSet<&str> = HashSet::new();
    for (h, prio) in ranked {
        if !seen_names.insert(h.name.as_str()) {
            continue;
        }
        let (lang, cluster) = lang_cluster_of(h);
        let graph_score = graph
            .nodes
            .get(&h.id)
            .map(|real_block| real_block.score)
            .unwrap_or(0.0);
        hubs.push(Hub {
            name: h.name.clone(),
            file: format_project_path(pp.root(), &h.file),
            // Prefer map priority so compact hub list ranks product entrypoints first.
            score: prio.max(graph_score),
            lang: Some(lang),
            cluster: Some(cluster),
        });
        if hubs.len() >= max_hubs {
            break;
        }
    }
    hubs
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph::Id;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn blk(file: &str, name: &str, kind: &str) -> BlockInfo {
        BlockInfo {
            id: Id::new(file, kind, name),
            name: name.into(),
            file: PathBuf::from(file),
            kind: kind.into(),
            lang: "python".into(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 0,
            parent_id: None,
            children: vec![],
            content_hash: "t".into(),
            sig_hash: "s".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            score: 1.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        }
    }

    #[test]
    fn skeleton_keeps_full_list_at_mid_size() {
        let files: Vec<String> = (0..100).map(|i| format!("pkg/mod_{i}.py")).collect();
        let budget = arch_skeleton_full_budget(100, true, 15_000);
        let (out, rolled, summary) = maybe_collapse_arch_skeleton(files.clone(), budget);
        assert!(
            !rolled,
            "100 scoped mid files must not roll up (budget {budget})"
        );
        assert!(summary.is_none());
        assert_eq!(out.len(), 100);
        assert!(arch_coverage_complete(100, rolled, 0));
    }

    #[test]
    fn skeleton_rolls_up_above_budget_and_marks_incomplete() {
        let budget = 80;
        let n = budget + 40;
        let files: Vec<String> = (0..n)
            .map(|i| format!("pkg/mod_{i}.py"))
            .collect();
        let (out, rolled, summary) = maybe_collapse_arch_skeleton(files, budget);
        assert!(rolled);
        assert!(summary.is_some());
        assert!(
            out.iter().any(|r| is_rollup_skeleton_row(r)),
            "want dir rollup rows: {out:?}"
        );
        assert!(!arch_coverage_complete(n, rolled, 0));
        assert!(arch_coverage_complete(n, false, 0)); // only if not rolled
    }

    #[test]
    fn adaptive_budget_blank_mega_tighter_than_scoped_mid() {
        let blank_mega = arch_skeleton_full_budget(5_000, false, 120_000);
        let scoped_mid = arch_skeleton_full_budget(100, true, 12_000);
        let blank_small = arch_skeleton_full_budget(40, false, 2_000);
        assert!(
            blank_mega < scoped_mid,
            "Gecko blank ({blank_mega}) must roll up sooner than scoped mid ({scoped_mid})"
        );
        assert!(blank_mega <= 80, "{blank_mega}");
        assert!(scoped_mid >= 100, "rich-scale scoped inventory must fit: {scoped_mid}");
        assert!(blank_small >= ARCH_SKELETON_FLOOR);
        assert!(scoped_mid <= ARCH_SKELETON_CEILING);
    }

    #[test]
    fn rollup_row_detect_and_suggested_scopes() {
        assert!(is_rollup_skeleton_row("rich/rich/ (76 files)"));
        assert!(!is_rollup_skeleton_row("rich/console.py"));
        let root = Path::new("/projects/test_repos/rich");
        let scopes = suggested_scopes_from_rollup_rows(
            root,
            [
                "rich/rich/__main__.py",
                "rich/rich/ (76 files)",
                "rich/_unicode_data/ (23 files)",
                "/home/user/projects/test_repos/rich/pkg/ (9 files)",
            ]
            .into_iter(),
            6,
        );
        assert!(
            scopes.iter().all(|s| !s.starts_with('/') && !s.starts_with("home/")),
            "must be repo-relative: {scopes:?}"
        );
        assert!(
            scopes.iter().any(|s| s.contains("rich") || s.contains("_unicode")),
            "{scopes:?}"
        );
    }

    #[test]
    fn hub_priority_prefers_authority_over_main_demo() {
        let root = Path::new("/proj");
        let demo = blk("rich/__main__.py", "ColorBox", "class_definition");
        let product = blk("rich/console.py", "Console", "class_definition");
        // Demo: entry landmark-ish, high out low in; product: high in, not entry.
        let demo_s = arch_map_hub_priority(&demo, root, 1, 40, true);
        let prod_s = arch_map_hub_priority(&product, root, 80, 10, false);
        assert!(
            prod_s > demo_s,
            "Console authority should beat __main__ demo: prod={prod_s} demo={demo_s}"
        );
    }

    #[test]
    fn hub_path_soft_demotes_tests_tree() {
        let d = arch_map_hub_path_soft_delta("pkg/tests/test_foo.py");
        assert!(d < 0, "{d}");
        let p = arch_map_hub_path_soft_delta("pkg/core.py");
        assert_eq!(p, 0);
    }
}

