//! The core module for the Butler code-graph subsystem — **Snooper**.
//!
//! Snooper is a multi-language code analysis engine that builds and queries a bidirectional
//! graph of code entities (functions, structs, traits, enums, modules, etc.) within a project.
//! It transforms raw source files into structured [`BlockInfo`] nodes connected by call/usage
//! edges, then provides context-aware composition for LLM consumption.
//!
//! # Architecture Overview
//!
//! The system follows a three-phase pipeline:
//!
//! 1. **Scanning** ([`scanner`]) — Walks the workspace directory, parses each source file with
//!    Tree-sitter, and extracts structural blocks (functions, structs, impls, traits, etc.).
//!    Blocks are stored as [`BlockInfo`] nodes in a [`CodeGraph`]. Edge building is deferred
//!    to preserve performance during initial scan.
//!
//! 2. **Collection** ([`collector`]) — Given seed blocks and a user prompt, performs a
//!    mode-aware BFS traversal of the graph to select relevant context. Supports two collection
//!    strategies: plain [`collect`] (BFS with depth limit) and [`collect_with_scoring`]
//!    (hybrid keyword + graph centrality scoring).
//!
//! 3. **Composition** ([`composer`]) — Transforms collected blocks into mode-specific,
//!    token-aware output tailored for LLM consumption. Supports five distinct modes:
//!    [`ContextMode::Balanced`], [`ContextMode::Surgical`], [`ContextMode::Implementation`],
//!    [`ContextMode::Architecture`], and [`ContextMode::Compressed`].
//!
//! # Design Decisions
//!
//! - **Lazy edge building**: Call/usage edges are computed on-demand via
//!   [`CodeGraph::ensure_call_graph`] rather than during the initial scan. This avoids
//!   expensive Tree-sitter query operations during workspace scanning, making the initial
//!   graph build significantly faster.
//!
//! - **Incremental updates** ([`scanner::load_graph`]): The scanner supports cache-based
//!   incremental re-parsing — only files with changed mtimes are re-parsed, and the graph
//!   is updated in-place rather than rebuilt from scratch.
//!
//! - **Hub detection** ([`CodeGraph::compute_hubs`]): Nodes in the top 5% by total degree
//!   (callers + callees) are flagged as "highly connected components". The composer handles
//!   these specially to avoid context explosion when traversing into library-wide hubs.
//!
//! - **Token-aware rendering** ([`token_manager`]): Output is budget-constrained using
//!   `cl100k` token counting (compatible with GPT-4). Blocks are rendered at varying detail
//!   levels (`Full`, `Signature`, `Minimal`, `Omitted`) based on mode and score.
//!
//! # Language Support
//!
//! Currently supports Rust and Python via the [`lang`] module. Each language module implements
//! Tree-sitter parsing, block extraction, call-edge collection (Rust only), and external crate
//! detection. New languages can be added by implementing the same parse pattern.
//!
//! # Module Structure
//!
//! - [`scanner`] — Workspace traversal, file parsing, caching, incremental updates
//! - [`collector`] — Block selection via BFS or keyword-centrality scoring
//! - [`composer`] — Mode-aware context composition with token budget management
//! - [`parser`] — Language-specific Tree-sitter parsing abstraction
//! - [`context`] — Public API entry point (`get_context`) and mode/configuration types
//! - [`token_manager`] — CL100K-BPE token counting for LLM output budgeting
//! - [`watcher`] — File-system watcher for automatic graph re-scanning
//! - [`lang`] — Language-specific parsers and edge collectors (Rust, Python)

#[allow(unused_imports)]
use std::collections::{HashMap, HashSet};

// ── Submodule declarations ─────────────────────────────────────────────

/// Workspace traversal, file parsing, caching, and incremental update logic.
pub mod scanner;

/// Block selection via BFS or keyword-centrality scoring.
pub mod collector;

/// Pure data model (Id, BlockInfo, CodeGraph + basic methods).
pub mod bg_progress;
pub mod model;
pub mod name_index;
pub mod edge_lifecycle;

/// Mode-aware context composition with token budget management.
pub mod composer;

/// Language-agnostic Tree-sitter parsing abstraction.
pub mod parser;

/// Configuration types and public API entry point (`get_context`).
pub mod context;

/// CL100K-BPE token counting for LLM output budgeting.
pub mod token_manager;

/// File-system watcher for automatic graph re-scanning on file changes.
pub mod watcher;
pub mod live_tree;

/// Language-specific parsers and edge collectors (Rust, Python).
pub mod lang;

/// FullEdge RAM tier + batch budget (B1 peel from builder).
mod edge_mem;

/// Per-lang CALL name maps (B2 peel from builder).
mod call_name_maps;

/// Per-file CALL/usage collect + source path I/O (B3 peel from builder).
mod edge_collect;

/// Heavy edge-building orchestration (cancellable rayon background builds, JIT, incremental).
pub mod builder;

/// Single mutation lane for full-edge + surgical JIT (EvePolice lesson).
pub mod warehouse_police;
/// Cross-stack bridges (protocol grammar) — typed edges, not Tree-sitter.
pub mod interconnect;

/// JSON export for lambda-eve neural sidecar (`graph_export.json`).
pub mod export;

/// Structure-first query tokens (identifier extract + graph membership; no NL dictionaries).
pub mod query_tokens;

/// Prompt subgraph retrieval for neural sidecar cascade.
pub mod subgraph;

/// Polyglot cross-language linker (type/function usage across Rust/Python/TS/Go/C++).
pub mod linker;

/// Config-driven IPC rules engine (cross-language call bridging).
pub mod ipc_engine;

/// Cross-platform path normalization (converts `\` to `/` for consistent string matching,
/// edge storage, caching, and scope filtering across Windows/WSL/Unix).
pub mod utils;

/// Project root anchor: repo-relative warehouse keys + abs/display translation.
pub mod project_paths;

/// Where `{root}/.butler` may be created (refuse nested src/examples/tests).
pub mod butler_cache_policy;

/// L0 path inventory → repo shape + progressive parse plan (generalist).
pub mod repo_shape;

/// Bundled-vendor / infra directory segment policy (scan hard-prune + demote).
pub mod path_policy;

/// Warehouse language honesty (unsupported product lang vs scanned crumbs).
pub mod warehouse_lang;

/// Language clusters + cross-cluster bridges (views over one warehouse).
pub mod clusters;

pub use project_paths::ProjectPaths;
pub use butler_cache_policy::{
    assert_butler_cache_writable, butler_cache_write_forbidden_reason, ensure_project_butler_cache_dir,
    ensure_project_butler_dir, probe_butler_cache_dir_writable,
    ALLOW_NESTED_ENV as BUTLER_ALLOW_NESTED_CACHE_ENV,
};
pub use path_policy::{
    bundled_vendor_dir_segments_owned, bundled_vendor_skip_patterns, is_bundled_vendor_dir_segment,
    is_infra_prune_dir_segment, BUNDLED_VENDOR_DIR_SEGMENTS, INFRA_PRUNE_DIR_SEGMENTS,
};
pub use warehouse_lang::{
    assess_lang_void, census_code_extensions, refresh_lang_void, CodeExtCensus, WarehouseLangVoid,
    SUPPORTED_SOURCE_EXTS, UNSUPPORTED_PRODUCT_EXTS,
};
pub use repo_shape::{classify_from_abs_paths, classify_from_rel_paths, path_priority_for_plan, ParsePlan, RepoShapeKind};
pub use clusters::{
    cluster_for_block, cluster_from_lang, find_bridges, normalize_lang_label, summarize_clusters,
    suggested_scopes_for_cluster, BridgeEdge, ClusterId, ClusterSummary,
};
pub use utils::normalize_path;

// ── Clean Public API ───────────────────────────────────────────────────

/// Re-exported block selection and collection functions from [`collector`].
pub use collector::{
    apply_heuristic_scores, apply_heuristic_scores_subset, collect, count_files_in_scope,
    dir_scope_matches_root_anchored, estimate_nodes_in_scope, filter_blocks_by_scope,
    is_junk_symbol_name, rank_blocks_for_selection, rank_blocks_for_selection_subset,
    scoped_block_refs, scoped_block_refs_capped, scoped_block_refs_for_symbol, select_blocks,
    suggest_scope_repairs_for_token, symbol_name_index_key, Collection, NeuralSelectionBlend,
    RankedCandidate, DEFAULT_SCOPE_NODE_CAP,
};

/// Structure-first query resolution (no natural-language dictionaries).
pub use query_tokens::{
    extract_raw_tokens, is_code_shaped, normalize_seed_query, resolve_structural_query,
    seed_name_matches_query, structural_block_score, StructuralQuery,
};

/// Re-exported token counting utilities from [`token_manager`].
pub use token_manager::{count_tokens, should_include_full_code};

/// Re-exported language-agnostic file parser from [`parser`].
pub use parser::{parse_file, parse_single_file, ParsedFile};

/// C/C++ family semantics used by Trace / product layers (decl↔def is structural, not a call).
pub use lang::c_family::{
    dialect_for_file as c_family_dialect_for_file, impl_preference_score as c_impl_preference_score,
    is_c_family_block, is_decl_def_implements_pair as is_c_decl_def_implements_pair,
    looks_static_c_block, CFamilyDialect,
};

/// Re-exported workspace scanning and caching functions from [`scanner`].
pub use scanner::{
    get_skip_patterns, graph_cache_exists, load_graph, save_graph, save_graph_async, scan_workspace,
    scan_workspace_with_waves, should_scan_path, CACHE_SCHEMA_VERSION, EDGE_SEMANTICS_VERSION,
    GRAPH_SCHEMA_VERSION,
};

/// Re-exported context configuration and retrieval from [`context`].
pub use context::{get_context, ContextOptions};

/// Re-exported composition functions and result types from [`composer`].
pub use composer::{compose, compose_context, ComposedContext, ContextMetadata};

/// Re-exported file-system watcher from [`watcher`].
pub use watcher::{start_watcher, start_watcher_cancellable};

/// Re-exported background edge build coordinator from [`builder`].
pub use builder::run_background_full_edge_build;
pub use builder::run_background_full_edge_build_policed;

/// Process-wide warehouse traffic cop.
pub use warehouse_police::{warehouse_police, WarehousePolice};

/// Re-export core types (model data + bg FullEdge telemetry).
pub use bg_progress::{BackgroundEdgeBuildState, BgBuildProgress};
pub use model::{BlockInfo, CodeGraph, Id, NameLocation};
pub use interconnect::BridgeKind;
