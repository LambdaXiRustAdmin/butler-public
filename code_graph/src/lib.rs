//! Re-exports the Snooper code-graph engine and its public API (CodeGraph, context, composer, etc.).

pub mod gnn;
pub mod snooper;
/// Host memory pressure (admission + scan/edge thread caps).
pub mod sys_pressure;

pub use snooper::composer::{compose, compose_context, ComposedContext, ContextMetadata};
pub use snooper::context::{get_context, ContextMode, ContextOptions, OutputFormat};
pub use snooper::export::{
    build_graph_export, build_graph_export_for_nodes, write_graph_export,
    write_graph_export_for_nodes, GraphExport,
};
pub use snooper::parser::ParseError;
pub use snooper::subgraph::{
    retrieve_prompt_subgraph, retrieve_prompt_subgraph_with_l0, PromptSubgraph,
};
pub use snooper::{
    apply_heuristic_scores, apply_heuristic_scores_subset, extract_raw_tokens, graph_cache_exists,
    is_code_shaped, is_junk_symbol_name, load_graph, normalize_seed_query, parse_file,
    rank_blocks_for_selection, rank_blocks_for_selection_subset, resolve_structural_query,
    save_graph, save_graph_async, scan_workspace, scan_workspace_with_waves, scoped_block_refs,
    seed_name_matches_query, select_blocks, start_watcher, structural_block_score, BgBuildProgress,
    BlockInfo, CodeGraph, Id, NameLocation, NeuralSelectionBlend, ParsePlan, ProjectPaths,
    RankedCandidate, RepoShapeKind, StructuralQuery, classify_from_abs_paths,
    classify_from_rel_paths, path_priority_for_plan,
    cluster_for_block, cluster_from_lang, find_bridges, normalize_lang_label, summarize_clusters,
    suggested_scopes_for_cluster, BridgeEdge, ClusterId, ClusterSummary,
    CACHE_SCHEMA_VERSION, EDGE_SEMANTICS_VERSION, GRAPH_SCHEMA_VERSION, BridgeKind,
    // Nested-cache guard (no .butler under src/examples/tests)
    assert_butler_cache_writable, butler_cache_write_forbidden_reason, ensure_project_butler_cache_dir,
    ensure_project_butler_dir, BUTLER_ALLOW_NESTED_CACHE_ENV,
    // Path policy: bundled-vendor segment skip list (scan hard-prune + demote)
    bundled_vendor_dir_segments_owned, bundled_vendor_skip_patterns, is_bundled_vendor_dir_segment,
    is_infra_prune_dir_segment, BUNDLED_VENDOR_DIR_SEGMENTS, INFRA_PRUNE_DIR_SEGMENTS,
    // Warehouse lang honesty (java-dominant etc. → refuse false product graph)
    assess_lang_void, census_code_extensions, refresh_lang_void, CodeExtCensus, WarehouseLangVoid,
    // C/C++ product semantics (decl↔def structural edges — not call edges)
    c_family_dialect_for_file, c_impl_preference_score, is_c_decl_def_implements_pair,
    is_c_family_block, looks_static_c_block, CFamilyDialect,
};

pub use sys_pressure::{
    admit_warehouse_open, estimate_cache_bytes, may_retry_deferred_open, scan_thread_cap, snapshot,
    AdmitDecision, PressureSnapshot, PressureTier,
};

/// Locate an optional on-disk fixture under `test_repos/<rel>` (canary / dogfood tests).
/// Never hardcodes a username. Order:
/// 1. `BUTLER_TEST_REPOS` env (base directory of checkouts)
/// 2. `/projects/test_repos` (Docker stack convention)
/// 3. `$HOME/projects/test_repos` (typical host layout)
#[doc(hidden)]
pub fn resolve_optional_test_repo(rel: &str) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut candidates = Vec::new();
    if let Ok(base) = std::env::var("BUTLER_TEST_REPOS") {
        if !base.is_empty() {
            candidates.push(PathBuf::from(base).join(rel));
        }
    }
    candidates.push(PathBuf::from("/projects/test_repos").join(rel));
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            candidates.push(PathBuf::from(home).join("projects/test_repos").join(rel));
        }
    }
    candidates.into_iter().find(|p| p.exists())
}
