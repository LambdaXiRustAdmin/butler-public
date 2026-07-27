// code_graph/src/snooper/context.rs
pub use crate::snooper::collector::{collect, collect_with_scoring, Collection};
// render_markdown is deprecated in favor of the composer module.
// pub use crate::snooper::renderer::render_markdown;
pub use crate::snooper::token_manager::{count_tokens, should_include_full_code};

use crate::snooper::normalize_path as norm_path;
use crate::{BlockInfo, CodeGraph};
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct ContextOptions {
    pub depth: usize,
    pub max_tokens: usize,
    pub compress_tests: bool,
    pub format: OutputFormat, // kept for server compatibility (legacy)

    /// Desired output shape for the LLM (new in Phase 1)
    pub mode: ContextMode,

    /// Optional surgical targeting (makes target_file + target_line actually work)
    pub target_file: Option<std::path::PathBuf>,
    pub target_line: Option<usize>,

    /// Drop blocks whose final score is below this (0.0 = no filtering)
    pub importance_threshold: f32,

    /// Working Set scoping support (prefix filters). Populated from MCP tool calls.
    pub scope_paths: Option<Vec<String>>,
    pub ignore_paths: Option<Vec<String>>,

    /// When true, block scores were set by lambda-eve neural sidecar — skip `score_block` heuristics.
    pub use_neural_scores: bool,

    /// Project root for hydrating empty `BlockInfo.source` from disk at compose time
    /// (slim progressive cache never stores source text).
    pub project_root: Option<std::path::PathBuf>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    #[default]
    Markdown,
    Json,
}

/// Controls the "desired output" shape for the LLM context.
/// This directly addresses noisy / hard-to-manage inputs by letting callers
/// request different consumption modes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ContextMode {
    /// Current balanced behavior (good mix of full code + summaries)
    #[default]
    Balanced,
    /// Focus on implementation bodies of the selected seeds + direct children
    Implementation,
    /// Signatures, relationships, high-level structure (good for architecture questions)
    Architecture,
    /// Surgical tracing: heavily prioritize the exact file+line + its callers/callees
    Surgical,
    /// Aggressive summarization / compression for very large contexts
    Compressed,
    /// Ultra-compact mode for tiny/local models (roughly 7B-class and below).
    /// Produces just the name + signature + top 2 direct neighbors.
    Mini,
}

// ── Main context function (scoring + graph aware) ──
pub fn get_context(
    graph: &CodeGraph,
    file: impl AsRef<Path>,
    start_line: usize,
    end_line: usize,
    opts: ContextOptions,
    prompt: &str,
) -> String {
    let file = file.as_ref();
    let norm_file = normalize_path(file);

    // === Surgical targeting support (hardened) ===
    // Demoscene: build with &BlockInfo refs (zero clone during filters), clone ONLY the final tiny set of seeds.
    let mut seeds: Vec<BlockInfo> = if let (Some(target_file), Some(target_line)) =
        (&opts.target_file, opts.target_line)
    {
        let norm_target = normalize_path(target_file);

        let mut surgical: Vec<&BlockInfo> = graph
            .nodes
            .values()
            .filter(|b| normalize_path(&b.file) == norm_target)
            .filter(|b| b.start_line <= target_line && b.end_line >= target_line)
            .collect();

        if surgical.is_empty() {
            // Graceful fallback: try a small window around the requested line
            surgical = graph
                .nodes
                .values()
                .filter(|b| normalize_path(&b.file) == norm_target)
                .filter(|b| (b.start_line as i64 - target_line as i64).abs() <= 20)
                .collect();

            if surgical.is_empty() {
                // Hardened diagnostic...
                let available: Vec<String> = graph
                    .nodes
                    .values()
                    .filter(|b| normalize_path(&b.file) == norm_target)
                    .map(|b| format!("- {} (lines {}-{})", b.name, b.start_line, b.end_line))
                    .collect();

                return format!(
                    "No block found at line {} in {:?}\n\n**Do not guess line numbers.**\n\nAvailable blocks in this file:\n{}\n\n**Strongly recommended:** Use Workflow B — provide only the function/struct name in the `prompt` field and omit both `target_file` and `target_line`.",
                    target_line, target_file, available.join("\n")
                );
            }
        }
        surgical.into_iter().cloned().collect()
    } else {
        // === Primary entry point for agent-friendly use: smart name/symbol lookup ===
        if let Some(best) = resolve_best_symbol(graph, prompt) {
            println!(
                "🔍 [get_context] Keyword/symbol resolution used (no target_line) — prompt=\"{}\" → {} ({}:{})",
                prompt, best.name, best.file.display(), best.start_line
            );
            vec![best.clone()]
        } else {
            // Legacy location hint fallback
            graph
                .nodes
                .values()
                .filter(|b| {
                    normalize_path(&b.file) == norm_file
                        && b.start_line <= end_line
                        && b.end_line >= start_line
                })
                .cloned()
                .collect()
        }
    };

    if seeds.is_empty() {
        return format!("No block found for the requested location in {:?}\n", file);
    }

    // Boost resolved targets (Surgical or Mini benefit from strong prioritization)
    if matches!(opts.mode, ContextMode::Surgical | ContextMode::Mini) {
        for block in &mut seeds {
            block.score += 100.0;
        }
    }

    // Move seeds (no clone) -- collect_with_scoring / collect now accept by move / IntoIterator.
    let collection = collect_with_scoring(graph, seeds, &opts, prompt);

    // Delegate final assembly to the new Composer
    // This is the start of the composer refactor
    crate::snooper::composer::compose(graph, collection.blocks, &opts, prompt)
}

fn normalize_path(p: &Path) -> String {
    // Delegate to the shared snooper utility for the core \ -> / sanitization.
    // Trims were dropped: graph stores (and ingress supplies) full normalized absolute
    // or relative-with-/ paths for consistent matching in filters/surgical/legacy paths.
    norm_path(&p.to_string_lossy())
}

/// Fast, low-allocation symbol / keyword resolver used when the caller does not
/// supply an exact `target_line`.
///
/// Matching priority:
///   1. Exact name match (case-sensitive → case-insensitive)
///   2. Substring match on `block.name` (case-insensitive)
///
/// Tie-breaking: highest (score + degree), where degree = |callers| + |callees|.
///
/// Returns a reference into the live graph (the caller clones only the winner).
fn resolve_best_symbol<'a>(graph: &'a CodeGraph, prompt: &str) -> Option<&'a BlockInfo> {
    let p = prompt.trim();
    if p.is_empty() {
        return None;
    }

    // 1. Exact match (zero allocation)
    if let Some(b) = graph.nodes.values().find(|b| b.name == p) {
        return Some(b);
    }

    let p_lower = p.to_lowercase();

    // Exact match, case-insensitive
    if let Some(b) = graph
        .nodes
        .values()
        .find(|b| b.name.to_lowercase() == p_lower)
    {
        return Some(b);
    }

    // 2. Substring matches + best ranking
    let mut best: Option<(&'a BlockInfo, i32)> = None;

    for block in graph.nodes.values() {
        let name_l = block.name.to_lowercase();
        if !name_l.contains(&p_lower) {
            continue;
        }

        let mut rank = 8i32;
        if name_l == p_lower {
            rank += 40;
        } else if name_l.starts_with(&p_lower) {
            rank += 15;
        }

        // Degree from the already-built bidirectional graph
        let deg = graph.reverse.get(&block.id).map_or(0, |v| v.len())
            + graph.edges.get(&block.id).map_or(0, |v| v.len());

        let total = rank * 10 + (block.score as i32) + (deg as i32);

        if best.is_none_or(|(_, s)| total > s) {
            best = Some((block, total));
        }
    }

    best.map(|(b, _)| b)
}
