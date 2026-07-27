//! Block selection via BFS or keyword-centrality scoring.
//!
//! This module was split using the Strangler Fig pattern (following the style
//! used for scanner/ and composer/).
//!
//! - mod.rs (this file): core data (Collection), scope filtering, plain BFS collect,
//!   and the public API surface. Re-exports from scoring submodule.
//! - scoring.rs: keyword extraction, per-block scoring, hub-aware neighbor selection,
//!   the heavy collect_with_scoring (scored BFS + hub special casing), and select_blocks.

use crate::snooper::normalize_path;
use crate::{BlockInfo, CodeGraph, Id};
use std::collections::{HashSet, VecDeque};

pub mod scoring;

// Re-export the heavy scoring APIs so that the collector module surface is unchanged
// for callers (snooper/mod.rs reexports, context, server, tests, etc.).
pub use scoring::{
    apply_heuristic_scores, apply_heuristic_scores_subset, collect_with_scoring,
    is_junk_symbol_name, keyword_text_match_score, path_keyword_score,
    rank_blocks_for_selection, rank_blocks_for_selection_subset, select_blocks,
    NeuralSelectionBlend, RankedCandidate,
};

fn translate_paths(
    paths: &Option<Vec<String>>,
    host_prefix: &str,
    container_prefix: &str,
    needs_translation: bool,
) -> Vec<String> {
    if let Some(v) = paths.as_ref() {
        if needs_translation {
            v.iter()
                .map(|p| {
                    let p = normalize_path(p);
                    if p.starts_with(host_prefix) {
                        normalize_path(&p.replacen(host_prefix, container_prefix, 1))
                    } else {
                        p
                    }
                })
                .collect()
        } else {
            v.iter().map(|p| normalize_path(p)).collect()
        }
    } else {
        vec![]
    }
}

pub fn file_matches_scope(
    file_str: &str,
    scope_paths: &Option<Vec<String>>,
    ignore_paths: &Option<Vec<String>>,
) -> bool {
    let (scopes, ignores) = resolved_scope_paths(scope_paths, ignore_paths);
    should_include_in_scope(file_str, &scopes, &ignores)
}

const SOURCE_FILE_EXTS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "svelte", "go", "c", "h", "cpp", "hpp", "cc", "cxx",
];

fn looks_like_file_scope(pat: &str) -> bool {
    let p = pat.trim_end_matches('/');
    if p.is_empty() || p.ends_with('/') {
        return false;
    }
    std::path::Path::new(p)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SOURCE_FILE_EXTS.contains(&ext))
}

/// Best-effort repo-relative form for matching (warehouse keys are usually relative).
///
/// Strips mount duals when configured; peels a single leading abs prefix only when the
/// remainder is already a plausible relative path (does **not** search for nested `src/`).
fn as_match_rel(path: &str) -> String {
    let mut s = normalize_path(path);
    s = s.trim_start_matches("./").to_string();
    let host = std::env::var("BUTLER_HOST_MOUNT").unwrap_or_default();
    let container = std::env::var("BUTLER_CONTAINER_MOUNT").unwrap_or_default();
    if !host.is_empty() && !container.is_empty() {
        let host = normalize_path(&host);
        let container = normalize_path(&container);
        if s.starts_with(&host) {
            s = s[host.len()..].trim_start_matches('/').to_string();
        } else if s.starts_with(&container) {
            s = s[container.len()..].trim_start_matches('/').to_string();
        }
    }
    // Absolute leftovers: drop only a leading slash so we never invent nested roots.
    // Live graphs should already be repo-relative via canonize_identity.
    if s.starts_with('/') {
        // Keep last path-looking relative suffix only when path embeds `/test_repos/<name>/`
        // or ends up still abs — then compare with starts_with will fail (safe miss).
        if let Some(idx) = s.find("/test_repos/") {
            let rest = &s[idx + "/test_repos/".len()..];
            if let Some(slash) = rest.find('/') {
                s = rest[slash + 1..].to_string();
            }
        } else {
            s = s.trim_start_matches('/').to_string();
        }
    }
    s
}

/// Exact file scope: `emcc.py` matches root or any path ending in `/emcc.py`.
fn file_scope_matches(file_str: &str, pat: &str) -> bool {
    let pat = pat.trim_end_matches('/');
    let file_str = as_match_rel(file_str);
    if file_str == pat {
        return true;
    }
    if file_str.ends_with(&format!("/{pat}")) {
        return true;
    }
    // Absolute ↔ relative: compare basenames when pat is a single path segment.
    if !pat.contains('/') {
        if let Some(name) = std::path::Path::new(&file_str)
            .file_name()
            .and_then(|n| n.to_str())
        {
            return name == pat;
        }
    }
    false
}

/// **Dir scope:** root-anchored prefix on repo-relative paths.
///
/// `src/` ⇒ only `<root>/src/**` — **not** `pyo3-ffi/src/**` or `**/src/**`.
/// File scopes still use [`file_scope_matches`].
pub fn dir_scope_matches_root_anchored(file_str: &str, pat: &str) -> bool {
    let pat = normalize_path(pat);
    let file_str = as_match_rel(file_str);

    if looks_like_file_scope(&pat) {
        return file_scope_matches(&file_str, &pat);
    }

    let marker = pat.trim_end_matches('/').to_string();
    if marker.is_empty() || marker == "." {
        return true;
    }
    let prefix = format!("{marker}/");
    // Root-anchored only — no contains("/src/"), no ends_with("/src").
    file_str == marker || file_str.starts_with(&prefix)
}

/// **Ignore** patterns: segment / path-contains match (nested `tests/` still ignored).
pub fn ignore_marker_matches(file_str: &str, pat: &str) -> bool {
    let pat = normalize_path(pat);
    let file_str = as_match_rel(file_str);

    if looks_like_file_scope(&pat) {
        return file_scope_matches(&file_str, &pat);
    }

    let marker = pat.trim_end_matches('/');
    if marker.is_empty() {
        return true;
    }
    let prefix = if pat.ends_with('/') {
        pat.clone()
    } else {
        format!("{marker}/")
    };
    file_str == marker
        || file_str.starts_with(&prefix)
        || file_str.ends_with(&format!("/{marker}"))
        || file_str.contains(&format!("/{marker}/"))
}

fn should_include_in_scope(file_str: &str, scopes: &[String], ignores: &[String]) -> bool {
    if !ignores.is_empty()
        && ignores
            .iter()
            .any(|pat| ignore_marker_matches(file_str, pat))
    {
        return false;
    }

    if scopes.is_empty() {
        true
    } else {
        scopes
            .iter()
            .any(|pat| dir_scope_matches_root_anchored(file_str, pat))
    }
}

/// When a root-anchored scope misses, suggest repo-relative pins that contain the
/// requested path segment (e.g. `src` → `cli/src/`, `code_graph/src/`).
///
/// Ranked by inventory density; always repo-relative; never host-absolute.
pub fn suggest_scope_repairs_for_token(
    inventory_paths: impl IntoIterator<Item = impl AsRef<str>>,
    requested: &str,
    max: usize,
) -> Vec<String> {
    if max == 0 {
        return vec![];
    }
    let token = normalize_path(requested)
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .last()
        .unwrap_or("")
        .to_string();
    if token.is_empty() || token == "." {
        return vec![];
    }
    let token_l = token.to_ascii_lowercase();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for p in inventory_paths {
        let rel = as_match_rel(p.as_ref());
        let parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        if let Some(idx) = parts
            .iter()
            .position(|s| s.eq_ignore_ascii_case(&token_l))
        {
            // Pin through the matching segment: django/core/, cli/src/
            let pin = format!("{}/", parts[..=idx].join("/"));
            *counts.entry(pin).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .take(max)
        .map(|(s, _)| s)
        .collect()
}

#[derive(Debug)]
pub struct Collection {
    pub blocks: Vec<BlockInfo>,
    pub selected_ids: HashSet<Id>,
}

pub fn resolved_scope_paths(
    scope_paths: &Option<Vec<String>>,
    ignore_paths: &Option<Vec<String>>,
) -> (Vec<String>, Vec<String>) {
    let host_prefix = std::env::var("BUTLER_HOST_MOUNT").unwrap_or_default();
    let container_prefix = std::env::var("BUTLER_CONTAINER_MOUNT").unwrap_or_default();
    let needs_translation = !host_prefix.is_empty() && !container_prefix.is_empty();
    (
        translate_paths(
            scope_paths,
            &host_prefix,
            &container_prefix,
            needs_translation,
        ),
        translate_paths(
            ignore_paths,
            &host_prefix,
            &container_prefix,
            needs_translation,
        ),
    )
}

/// Hard cap for non-surgical scope materialize (Arch / map). Never build multi‑M vecs.
pub const DEFAULT_SCOPE_NODE_CAP: usize = 80_000;

/// Count inventory **files** matching scope/ignore (O(files) via `file_hashes`).
///
/// Prefer this for Arch preflight — never walk 4.8M nodes just to refuse.
/// Returns 0 when `file_hashes` is empty (unknown inventory).
pub fn count_files_in_scope(
    graph: &CodeGraph,
    scope_paths: &Option<Vec<String>>,
    ignore_paths: &Option<Vec<String>>,
) -> usize {
    if graph.file_hashes.is_empty() {
        return 0;
    }
    let (scopes, ignores) = resolved_scope_paths(scope_paths, ignore_paths);
    graph
        .file_hashes
        .keys()
        .filter(|p| {
            let file_str = normalize_path(p);
            should_include_in_scope(&file_str, &scopes, &ignores)
        })
        .count()
}

/// Estimate node count under scope from file hits × average nodes/file.
///
/// When inventory is missing, returns `graph.nodes.len()` (worst case).
pub fn estimate_nodes_in_scope(graph: &CodeGraph, file_hits: usize) -> usize {
    if graph.nodes.is_empty() {
        return 0;
    }
    let n_files = graph.file_hashes.len();
    if n_files == 0 {
        return graph.nodes.len();
    }
    if file_hits == 0 {
        return 0;
    }
    let avg = ((graph.nodes.len() as u128) + (n_files as u128) - 1) / (n_files as u128);
    (file_hits as u128)
        .saturating_mul(avg)
        .min(graph.nodes.len() as u128) as usize
}

/// Zero-copy scope filter: returns references into the graph (no `BlockInfo` clones).
///
/// **Cost:** O(nodes) when scopes/ignores force a scan — fine for small graphs, multi-second
/// on 4.8M leviathans. Prefer [`scoped_block_refs_for_symbol`] for exact-name Trace.
/// Prefer [`scoped_block_refs_capped`] when a hard limit is required (Arch).
pub fn scoped_block_refs<'a>(
    graph: &'a CodeGraph,
    scope_paths: &Option<Vec<String>>,
    ignore_paths: &Option<Vec<String>>,
) -> Vec<&'a BlockInfo> {
    scoped_block_refs_capped(graph, scope_paths, ignore_paths, usize::MAX).0
}

/// Like [`scoped_block_refs`], but stops after `max_blocks` matches (`capped=true`).
///
/// Eager refuse rail: never finish a 3.4M collect just to measure length.
///
/// **File-local path:** when `file_node_index` is warm and there is a **positive** scope,
/// iterate only inventory files in scope then those files' nodes — O(files_in_scope +
/// nodes_in_scope), not O(warehouse). Falls back to full `nodes.values()` when the
/// index is cold or scope is blank (ignores-only / whole graph).
pub fn scoped_block_refs_capped<'a>(
    graph: &'a CodeGraph,
    scope_paths: &Option<Vec<String>>,
    ignore_paths: &Option<Vec<String>>,
    max_blocks: usize,
) -> (Vec<&'a BlockInfo>, bool) {
    let (scopes, ignores) = resolved_scope_paths(scope_paths, ignore_paths);
    let mut out: Vec<&BlockInfo> = Vec::new();
    let mut capped = false;

    // Stop once we have `max_blocks` matches; `capped` means "at least this many" (more may exist).
    let mut push = |b: &'a BlockInfo| -> bool {
        if out.len() >= max_blocks {
            capped = true;
            return false;
        }
        out.push(b);
        if out.len() >= max_blocks {
            capped = true;
        }
        true
    };

    if scopes.is_empty() && ignores.is_empty() {
        for b in graph.nodes.values() {
            if !push(b) {
                break;
            }
        }
        return (out, capped);
    }

    // File-first gather: positive scopes + warm path→ids index (Arch xpcom/base cliff).
    // Iterate **index keys** (node-path dialect) — O(files) path filters, not O(nodes).
    if !scopes.is_empty() && graph.file_node_index_is_warm() {
        for (path, ids) in &graph.file_node_index {
            if !should_include_in_scope(path, &scopes, &ignores) {
                continue;
            }
            for id in ids {
                let Some(b) = graph.nodes.get(id) else {
                    continue;
                };
                if !push(b) {
                    return (out, capped);
                }
            }
        }
        return (out, capped);
    }

    // Cold index / ignores-only: full node scan (representative; never partial HashMap stop).
    for b in graph.nodes.values() {
        let file_str = normalize_path(&b.file.to_string_lossy());
        if should_include_in_scope(&file_str, &scopes, &ignores) && !push(b) {
            break;
        }
    }
    (out, capped)
}

/// Leaf token for C++/Rust qualified names (`mozilla::Mutex` → `Mutex`, `a::b::c` → `c`).
///
/// Name index keys bare identifiers, not `ns::Type`. Callers must not full-scope materialize
/// on leviathans just because the qualified form misses.
pub fn symbol_name_index_key(symbol: &str) -> &str {
    let sym = symbol.trim();
    if sym.is_empty() {
        return sym;
    }
    // Prefer C++ / Rust path separator; also accept single trailing segment after `.` only when
    // it looks like a type path (Foo.Bar) — not file extensions (skip if last has no alnum start).
    if let Some(leaf) = sym.rsplit("::").next() {
        let leaf = leaf.trim();
        if !leaf.is_empty() && leaf != sym {
            return leaf;
        }
    }
    sym
}

/// Monster Trace path: O(name hits) only — never walk 4.8M nodes to apply ignore_paths.
///
/// Seed selection uses `name_index` / `blocks_for_name`; full-scope materialize was the
/// multi-second single-thread tax after every warm poke on Complete warehouses.
///
/// Qualified names: try full string then **leaf** (`mozilla::Mutex` → `Mutex`) so surgical
/// Trace stays O(hits) instead of falling back to full monorepo scope.
pub fn scoped_block_refs_for_symbol<'a>(
    graph: &'a CodeGraph,
    symbol: &str,
    scope_paths: &Option<Vec<String>>,
    ignore_paths: &Option<Vec<String>>,
) -> Vec<&'a BlockInfo> {
    let sym = symbol.trim();
    if sym.is_empty() {
        return scoped_block_refs(graph, scope_paths, ignore_paths);
    }
    let (scopes, ignores) = resolved_scope_paths(scope_paths, ignore_paths);
    let mut hits = graph.blocks_for_name(sym);
    if hits.is_empty() {
        let leaf = symbol_name_index_key(sym);
        if leaf != sym {
            hits = graph.blocks_for_name(leaf);
        }
    }
    if hits.is_empty() {
        // No exact name — caller may fall back to full scope / fuzzy (not on monsters).
        return Vec::new();
    }
    if scopes.is_empty() && ignores.is_empty() {
        return hits;
    }
    hits.into_iter()
        .filter(|b| {
            let file_str = normalize_path(&b.file.to_string_lossy());
            should_include_in_scope(&file_str, &scopes, &ignores)
        })
        .collect()
}

/// Early Working Set filter with proper path translation for Docker/host mismatch.
/// Uses the same normalization logic as the rest of the server (replace backslashes +
/// starts_with / contains fallback). This makes relative scopes like ["code_graph/src/snooper/"]
/// work correctly even when the graph stores full container paths.
pub fn filter_blocks_by_scope(
    blocks: &[BlockInfo],
    scope_paths: &Option<Vec<String>>,
    ignore_paths: &Option<Vec<String>>,
) -> Vec<BlockInfo> {
    let (scopes, ignores) = resolved_scope_paths(scope_paths, ignore_paths);

    if scopes.is_empty() && ignores.is_empty() {
        return blocks.to_vec();
    }

    blocks
        .iter()
        .filter(|b| {
            let file_str = normalize_path(&b.file.to_string_lossy());
            should_include_in_scope(&file_str, &scopes, &ignores)
        })
        .cloned()
        .collect()
}

// Private owned variant used in the hot path (collect_with_scoring) to avoid
// an extra clone round-trip when we already own the candidate list.
fn filter_blocks_by_scope_owned(
    mut blocks: Vec<BlockInfo>,
    scope_paths: &Option<Vec<String>>,
    ignore_paths: &Option<Vec<String>>,
) -> Vec<BlockInfo> {
    let (scopes, ignores) = resolved_scope_paths(scope_paths, ignore_paths);

    if scopes.is_empty() && ignores.is_empty() {
        return blocks;
    }

    blocks.retain(|b| {
        let file_str = normalize_path(&b.file.to_string_lossy());
        should_include_in_scope(&file_str, &scopes, &ignores)
    });

    blocks
}

pub fn collect(
    graph: &CodeGraph,
    seed_blocks: impl IntoIterator<Item = BlockInfo>,
    options: &crate::ContextOptions,
) -> Collection {
    // NOTE (long-term refactor): When JIT surgical edge build is active (see CodeGraph::ensure_call_graph
    // with target_files + run_background_full_edge_build), the edges for the relevant files (selected
    // from the eager skeleton using the target_symbol) are guaranteed to be present before collection
    // runs. This keeps TraceBlastRadius / FindImplementation fast (~1-2s) even while the multi-core
    // cancellable background full build is in flight for the rest of a large repo (e.g. Bevy).

    // Optimized hot-path BFS:
    // - Queue and seen use cheap Id (small clones only).
    // - We never store full BlockInfo in the queue.
    // - BlockInfo is cloned only once, at the moment we decide to include it in the result.
    // - Seeds are moved into the result where possible (no extra clone for the seed set itself).

    let mut queue: VecDeque<Id> = VecDeque::with_capacity(64);
    let mut seen: HashSet<Id> = HashSet::with_capacity(128);
    let mut blocks: Vec<BlockInfo> = Vec::with_capacity(64);

    for block in seed_blocks {
        let id = block.id.clone();
        if seen.insert(id.clone()) {
            queue.push_back(id);
            blocks.push(block); // move the owned seed BlockInfo (no extra clone)
        }
    }

    // Mode-aware collection radius (reduces noise dramatically in Surgical mode)
    let multiplier = match options.mode {
        crate::snooper::context::ContextMode::Surgical => 5,
        crate::snooper::context::ContextMode::Compressed => 8,
        _ => 20,
    };
    let max_blocks = options.depth * multiplier;
    let mut collected = 1usize;

    while let Some(current_id) = queue.pop_front() {
        if collected >= max_blocks {
            break;
        }

        // Children (callees)
        for child_id in graph.children(&current_id) {
            if seen.insert(child_id.clone()) {
                if let Some(child) = graph.get_block(child_id.clone()) {
                    // Clone the heavy BlockInfo *only* when we actually include it.
                    blocks.push(child.clone());
                    queue.push_back(child_id); // move the (already cloned for seen) Id from the children() vec
                    collected += 1;
                }
            }
        }

        // Callers
        for caller_id in graph.callers(&current_id) {
            if seen.insert(caller_id.clone()) {
                if let Some(caller) = graph.get_block(caller_id.clone()) {
                    blocks.push(caller.clone());
                    queue.push_back(caller_id);
                    collected += 1;
                }
            }
        }
    }

    Collection {
        blocks,
        selected_ids: seen,
    }
}

// Note: collect_with_scoring and select_blocks are re-exported from the scoring submodule
// (see pub use at top of this file). Their implementations live in scoring.rs to keep
// the keyword + graph-centrality "heavy" logic isolated.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockInfo;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn make_simple_block(name: &str, source: &str) -> BlockInfo {
        let id = crate::Id::new("test.rs", "function_item", &format!("hash_{}", name));
        BlockInfo {
            id: id.clone(),
            name: name.to_string(),
            file: PathBuf::from("test.rs"),
            kind: "function_item".to_string(),
            lang: "rust".to_string(),
            start_line: 1,
            end_line: 3,
            start_byte: 0,
            end_byte: source.len(),
            parent_id: None,
            children: vec![],
            content_hash: format!("hash_{}", name),
            sig_hash: format!("sig_{}", name),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: source.to_string(),
            score: 0.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        }
    }

    #[test]
    fn test_extract_keywords_filters_noise() {
        let kws = scoring::extract_keywords(
            "How does the authentication middleware work with User struct?",
        );
        // Should keep meaningful tokens, drop stop words
        assert!(kws
            .iter()
            .any(|&w| w.contains("auth") || w.contains("User") || w.contains("struct")));
        assert!(!kws.iter().any(|&w| w == "the" || w == "with" || w == "how"));
    }

    #[test]
    fn test_score_block_exact_name_match() {
        let mut graph = CodeGraph::new();
        let block = make_simple_block("authenticate_user", "fn authenticate_user() {}");
        graph.add_block(block.clone());

        let score = scoring::score_block(&block, "authenticate_user", &[], &graph);
        assert!(score >= 15.0, "Exact name match should give high score");
    }

    #[test]
    fn test_score_block_degree_boost() {
        let mut graph = CodeGraph::new();
        let main = make_simple_block("main", "fn main() { foo(); }");
        let foo = make_simple_block("foo", "fn foo() {}");

        graph.add_block(main.clone());
        graph.add_block(foo.clone());
        graph.add_edge(main.id.clone(), foo.id.clone()); // main calls foo

        let score = scoring::score_block(&foo, "something", &[], &graph);
        // foo has 1 caller → should get degree boost
        assert!(score > 2.0);
    }

    #[test]
    fn test_collect_with_scoring_sorts_by_score() {
        let mut graph = CodeGraph::new();

        let auth = make_simple_block("auth", "pub fn authenticate() {}");
        let utils = make_simple_block("utils", "fn helper() {}");

        graph.add_block(auth.clone());
        graph.add_block(utils.clone());

        let seeds = vec![auth.clone(), utils.clone()];
        let opts = crate::ContextOptions {
            depth: 1,
            max_tokens: 4000,
            compress_tests: false,
            format: crate::snooper::context::OutputFormat::Markdown,
            ..Default::default()
        };

        let collection = collect_with_scoring(&graph, seeds, &opts, "authenticate");

        // "auth" should come before "utils" because of keyword match + pub boost
        if collection.blocks.len() >= 2 {
            assert_eq!(collection.blocks[0].name, "auth");
        }
    }

    #[test]
    fn scope_src_is_root_anchored_not_nested() {
        // Root-anchored: only <root>/src/** — not crates/*/src or tools/*/src.
        assert!(dir_scope_matches_root_anchored("src/main.rs", "src/"));
        assert!(dir_scope_matches_root_anchored("src/lib.rs", "src"));
        assert!(!dir_scope_matches_root_anchored(
            "crates/bevy_app/src/app.rs",
            "src/"
        ));
        assert!(!dir_scope_matches_root_anchored(
            "pyo3-ffi/src/lib.rs",
            "src/"
        ));
        assert!(!dir_scope_matches_root_anchored(
            "tools/export-content/src/app.rs",
            "src/"
        ));
        // Explicit nested pin still works.
        assert!(dir_scope_matches_root_anchored(
            "crates/bevy_app/src/app.rs",
            "crates/"
        ));
        assert!(dir_scope_matches_root_anchored(
            "cli/src/server/mod.rs",
            "cli/src/"
        ));
    }

    #[test]
    fn scope_ignore_still_matches_nested_tests() {
        // Ignores keep segment match — nested tests/ still dropped.
        let scopes = vec!["src/".to_string()];
        let ignores = vec!["tests/".to_string()];
        assert!(should_include_in_scope("src/lib.rs", &scopes, &ignores));
        assert!(!should_include_in_scope(
            "src/tests/foo.rs",
            &scopes,
            &ignores
        ));
        assert!(!should_include_in_scope(
            "crates/foo/tests/bar.rs",
            &scopes,
            &ignores
        ));
        // tools/ ignore still works nested
        let ignores = vec!["tools/".to_string()];
        assert!(!should_include_in_scope(
            "tools/export-content/src/app.rs",
            &[],
            &ignores
        ));
    }

    #[test]
    fn decision_table_pyo3_bat_django_self() {
        // bat src/ → root package only
        assert!(dir_scope_matches_root_anchored("src/main.rs", "src/"));
        // pyo3: root src/ only, not ffi
        assert!(dir_scope_matches_root_anchored("src/err/mod.rs", "src/"));
        assert!(!dir_scope_matches_root_anchored(
            "pyo3-ffi/src/lib.rs",
            "src/"
        ));
        // self-repo: bare src/ misses nested
        assert!(!dir_scope_matches_root_anchored("cli/src/lib.rs", "src/"));
        assert!(!dir_scope_matches_root_anchored(
            "code_graph/src/lib.rs",
            "src/"
        ));
        // django core/ miss; django/core/ hits
        assert!(!dir_scope_matches_root_anchored(
            "django/core/handlers/base.py",
            "core/"
        ));
        assert!(dir_scope_matches_root_anchored(
            "django/core/handlers/base.py",
            "django/core/"
        ));
    }

    #[test]
    fn suggest_repairs_for_src_token() {
        let inv = [
            "cli/src/server/mod.rs",
            "cli/src/main.rs",
            "code_graph/src/lib.rs",
            "src/bin/tool.rs",
            "README.md",
        ];
        let pins = suggest_scope_repairs_for_token(inv.iter().copied(), "src/", 6);
        assert!(
            pins.iter().any(|p| p == "cli/src/" || p == "src/"),
            "pins={pins:?}"
        );
        assert!(pins.iter().any(|p| p == "code_graph/src/"), "pins={pins:?}");
        assert!(pins.iter().all(|p| !p.starts_with('/') && !p.contains("/home/")));
    }
}


#[cfg(test)]
mod leaf_name_tests {
    use super::symbol_name_index_key;

    #[test]
    fn symbol_name_index_key_splits_cpp() {
        assert_eq!(symbol_name_index_key("mozilla::Mutex"), "Mutex");
        assert_eq!(symbol_name_index_key("a::b::Foo"), "Foo");
        assert_eq!(symbol_name_index_key("Mutex"), "Mutex");
        assert_eq!(symbol_name_index_key("  nsCOMPtr  "), "nsCOMPtr");
    }
}


#[cfg(test)]
mod scope_preflight_tests {
    use super::*;
    use crate::snooper::model::{BlockInfo, CodeGraph, Id};
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn blk(file: &str, name: &str, uniq: usize) -> BlockInfo {
        let hash = format!("{:08x}{:08x}", uniq as u32, uniq as u32 + 1);
        BlockInfo {
            id: Id::new(file, "function_item", &hash),
            name: name.into(),
            file: PathBuf::from(file),
            kind: "function_item".into(),
            lang: "cpp".into(),
            start_line: 1,
            end_line: 2,
            start_byte: 0,
            end_byte: 1,
            parent_id: None,
            children: vec![],
            content_hash: hash,
            sig_hash: "s".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            score: 0.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        }
    }

    #[test]
    fn count_files_in_scope_preflight() {
        let mut g = CodeGraph::new();
        for (i, f) in ["xpcom/a.h", "xpcom/b.h", "js/c.cpp"].iter().enumerate() {
            let b = blk(f, "x", i);
            g.file_hashes.insert((*f).into(), 1);
            g.nodes.insert(b.id.clone(), b);
        }
        let scopes = Some(vec!["xpcom/".into()]);
        assert_eq!(count_files_in_scope(&g, &scopes, &None), 2);
        let est = estimate_nodes_in_scope(&g, 2);
        assert!(est >= 2 && est <= 3, "est={est}");
    }

    #[test]
    fn scoped_block_refs_capped_stops_early() {
        let mut g = CodeGraph::new();
        for i in 0..20 {
            let f = format!("src/f{i}.rs");
            let b = blk(&f, "n", i);
            g.file_hashes.insert(f.clone(), 1);
            g.nodes.insert(b.id.clone(), b);
        }
        let (refs, capped) = scoped_block_refs_capped(&g, &None, &None, 5);
        assert!(capped);
        assert_eq!(refs.len(), 5);
    }

    #[test]
    fn file_local_collect_uses_path_index() {
        let mut g = CodeGraph::new();
        for (i, f) in ["xpcom/base/a.h", "xpcom/base/b.h", "js/x.cpp"].iter().enumerate() {
            let b = blk(f, "n", i);
            g.file_hashes.insert((*f).into(), 1);
            g.nodes.insert(b.id.clone(), b);
        }
        g.rebuild_name_index();
        assert!(g.file_node_index_is_warm());
        let scopes = Some(vec!["xpcom/base/".into()]);
        let (refs, capped) = scoped_block_refs_capped(&g, &scopes, &None, 80_000);
        assert!(!capped);
        assert_eq!(refs.len(), 2, "only xpcom/base nodes");
        assert!(refs.iter().all(|b| b.file.to_string_lossy().contains("xpcom")));
    }
}
