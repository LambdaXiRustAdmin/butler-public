//! Scoring and advanced collection logic for the collector.
//!
//! Extracted via Strangler Fig from the monolithic collector.rs.
//! Contains keyword extraction, per-block scoring (name/keyword/degree/recency/pub boost),
//! hub-aware neighbor selection during collection, the hybrid collect_with_scoring
//! (BFS + scoring + hub special-casing + working-set filter), and the pure
//! keyword-based select_blocks.
//!
//! The plain BFS collect and scope filter remain in the parent mod.rs as core
//! "collection" concerns. This keeps the heavy scoring/orchestration separate.

use crate::{BlockInfo, CodeGraph, Id};
use std::collections::{HashSet, VecDeque};

use super::{collect, filter_blocks_by_scope};

// Scoring helpers for context selection
pub(crate) fn score_block(
    block: &BlockInfo,
    prompt_lower: &str,
    keywords: &[&str],
    graph: &CodeGraph,
) -> f64 {
    let name_lower = block.name.to_lowercase();
    let _source_lower = block.source.to_lowercase();

    // Start with the structural importance already computed by compute_hubs (degree centrality).
    // This makes scoring additive: structural (hubs) + semantic (prompt/keywords).
    let mut score = block.score;

    // Direct match scoring
    if name_lower == prompt_lower {
        score += 15.0;
    } else if name_lower.contains(prompt_lower) {
        score += 8.0;
    }

    // Robust bidirectional keyword matching (snake_case ↔ camelCase)
    let search_regexes = crate::snooper::lang::rust::build_search_regexes(keywords);

    for re in &search_regexes {
        if re.is_match(&block.name) || re.is_match(&block.source) {
            score += 4.0;
            break;
        }
    }

    // Degree boosting (in-degree = callers, out-degree = children/calls)
    let in_degree = graph.reverse.get(&block.id).map_or(0, |v| v.len()) as f64;
    let out_degree = graph.edges.get(&block.id).map_or(0, |v| v.len()) as f64;
    score += (in_degree * 2.2) + (out_degree * 1.0);

    // Public API boost
    if block.source.contains("pub ") || block.name.contains("pub ") {
        score += 3.5;
    }

    // Recency boost
    if let Some(recency) = block.git_blame_recency {
        score += if recency < 86400 {
            3.5
        } else if recency < 604800 {
            1.8
        } else {
            0.6
        };
    }

    score
}

pub(crate) fn extract_keywords(prompt: &str) -> Vec<&str> {
    prompt
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .filter(|w| {
            let s = w.chars().next().unwrap_or(' ');
            s.is_uppercase()
                || w.contains("::")
                || w.contains('_')
                || [
                    "self", "Self", "Result", "Option", "Vec", "HashMap", "String", "fn", "struct",
                    "impl",
                ]
                .contains(w)
        })
        .filter(|w| {
            ![
                "the", "and", "for", "with", "use", "butler", "context", "analyze", "code",
            ]
            .contains(w)
        })
        .collect()
}

// Helper for hub top neighbors during collection (uses already scored unique_map)
// Returns borrowed references (zero clone of BlockInfo data).
pub(crate) fn get_top_scoring_neighbors_for_collection<'a>(
    id: &Id,
    graph: &CodeGraph,
    unique_map: &std::collections::HashMap<Id, &'a BlockInfo>,
    n: usize,
) -> Vec<&'a BlockInfo> {
    let mut neighbors: Vec<&'a BlockInfo> = graph
        .callers(id)
        .into_iter()
        .chain(graph.children(id))
        .filter_map(|nid| unique_map.get(&nid).copied())
        .collect();

    neighbors.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    neighbors.into_iter().take(n).collect()
}

/// Apply prompt-aware heuristic [`score_block`] values to every node in the graph.
///
/// Used by Butler before exporting to lambda-eve so `heuristic_score` in the JSON
/// contract reflects the same baseline that neural scores replace.
pub fn apply_heuristic_scores(graph: &mut CodeGraph, prompt: &str) {
    let ids: HashSet<Id> = graph.nodes.keys().cloned().collect();
    apply_heuristic_scores_subset(graph, prompt, &ids);
}

/// Apply prompt-aware heuristic scores only to `ids` (subgraph cascade micro-path).
pub fn apply_heuristic_scores_subset(graph: &mut CodeGraph, prompt: &str, ids: &HashSet<Id>) {
    let prompt_lower = prompt.to_lowercase();
    let keywords = extract_keywords(&prompt_lower);
    for id in ids {
        if let Some(block) = graph.nodes.get(id).cloned() {
            let score = score_block(&block, &prompt_lower, &keywords, graph);
            graph.heuristic_score_cache.insert(id.clone(), score);
            if let Some(b) = graph.nodes.get_mut(id) {
                b.score = score;
            }
        }
    }
}

/// Configurable blend for neural selection funnel (text match vs GNN score).
#[derive(Debug, Clone, Copy)]
pub struct NeuralSelectionBlend {
    pub text_weight: f64,
    pub neural_weight: f64,
}

impl Default for NeuralSelectionBlend {
    fn default() -> Self {
        Self {
            text_weight: 0.1,
            neural_weight: 0.9,
        }
    }
}

/// One row in the selection funnel (used by `select_blocks` + audit logging).
#[derive(Debug, Clone)]
pub struct RankedCandidate {
    pub name: String,
    pub id: Id,
    pub text_match: f64,
    pub heuristic: f64,
    pub neural: f64,
    pub blended: f64,
}

pub use crate::snooper::query_tokens::is_junk_symbol_name;

/// Name-only text match via structural hierarchy (exact ≫ suffix ≫ segment).
/// `keywords` kept for call-site compat; preferred path is [`structural_text_score`].
pub fn keyword_text_match_score(block: &BlockInfo, keywords: &[&str]) -> f64 {
    if is_junk_symbol_name(&block.name) || keywords.is_empty() {
        return 0.0;
    }
    let q = crate::snooper::query_tokens::StructuralQuery {
        graph_hits: keywords.iter().map(|s| (*s).to_string()).collect(),
        strong_unmatched: vec![],
        exact_name_hits: vec![],
        prompt_lower: String::new(),
    };
    crate::snooper::query_tokens::structural_name_score(block, &q)
}

/// Path co-score (module / file segments).
pub fn path_keyword_score(block: &BlockInfo, keywords: &[&str]) -> f64 {
    if keywords.is_empty() {
        return 0.0;
    }
    let q = crate::snooper::query_tokens::StructuralQuery {
        graph_hits: keywords.iter().map(|s| (*s).to_string()).collect(),
        strong_unmatched: vec![],
        exact_name_hits: vec![],
        prompt_lower: String::new(),
    };
    crate::snooper::query_tokens::structural_path_score(block, &q)
}

/// Full structural text score for a block against a live graph prompt resolution.
pub fn structural_text_score(
    block: &BlockInfo,
    query: &crate::snooper::query_tokens::StructuralQuery,
) -> f64 {
    crate::snooper::query_tokens::structural_block_score(block, query)
}

/// Rank a candidate subset (zero-copy scope audit / orchestrate preamble).
pub fn rank_blocks_for_selection_subset(
    graph: &CodeGraph,
    candidates: &[&BlockInfo],
    prompt: &str,
    use_neural_scores: bool,
    blend: NeuralSelectionBlend,
) -> Vec<RankedCandidate> {
    let query = crate::snooper::query_tokens::resolve_structural_query(graph, prompt);
    if !query.has_usable_hits() || candidates.is_empty() {
        return vec![];
    }

    let prompt_lower = prompt.to_lowercase();
    let extract_kw = extract_keywords(&prompt_lower);
    let exact_floor = crate::snooper::query_tokens::EXACT_NAME_SCORE * 0.5;

    if use_neural_scores {
        let raw: Vec<(&BlockInfo, f64, f64, f64)> = candidates
            .iter()
            .filter(|b| !is_junk_symbol_name(&b.name))
            .map(|b| {
                let text = structural_text_score(b, &query);
                let neural = graph
                    .neural_score_cache
                    .get(&b.id)
                    .copied()
                    .unwrap_or(b.score);
                let heuristic = graph
                    .heuristic_score_cache
                    .get(&b.id)
                    .copied()
                    .unwrap_or_else(|| score_block(b, &prompt_lower, &extract_kw, graph));
                (*b, text, heuristic, neural)
            })
            .collect();

        let max_text = raw
            .iter()
            .map(|(_, t, _, _)| *t)
            .fold(0.0f64, f64::max)
            .max(1.0);
        let max_neural = raw
            .iter()
            .map(|(_, _, _, n)| *n)
            .fold(0.0f64, f64::max)
            .max(1e-9);

        let mut ranked: Vec<RankedCandidate> = raw
            .into_iter()
            .map(|(b, text, heuristic, neural)| {
                let text_norm = text / max_text;
                let neural_norm = neural / max_neural;
                // Exact / full-symbol text must dominate GNN blend (suffix shells stay down).
                let blended = if text >= exact_floor {
                    1_000.0 + text_norm * 10.0 + neural_norm
                } else {
                    blend.text_weight * text_norm + blend.neural_weight * neural_norm
                };
                RankedCandidate {
                    name: b.name.clone(),
                    id: b.id.clone(),
                    text_match: text,
                    heuristic,
                    neural,
                    blended,
                }
            })
            .collect();

        ranked.sort_by(|a, b| {
            b.blended
                .partial_cmp(&a.blended)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.text_match.partial_cmp(&a.text_match).unwrap_or(std::cmp::Ordering::Equal))
        });
        ranked
    } else {
        let mut ranked: Vec<RankedCandidate> = candidates
            .iter()
            .filter(|b| !is_junk_symbol_name(&b.name))
            .filter_map(|b| {
                let text = structural_text_score(b, &query);
                if text <= 0.0 {
                    return None;
                }
                let heuristic = score_block(b, &prompt_lower, &extract_kw, graph);
                let blended = text * (1.0 + heuristic);
                Some(RankedCandidate {
                    name: b.name.clone(),
                    id: b.id.clone(),
                    text_match: text,
                    heuristic,
                    neural: 0.0,
                    blended,
                })
            })
            .collect();

        ranked.sort_by(|a, b| {
            b.blended
                .partial_cmp(&a.blended)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        ranked
    }
}

/// Rank graph nodes for selection.
///
/// **Neural:** only GNN subgraph members (`neural_score_cache` present / non-zero) plus
/// structural name/path hits — never a full-graph blend over 25k nodes.
/// **Heuristic:** structural hits only (junk filtered). Membership-filtered tokens only.
pub fn rank_blocks_for_selection(
    graph: &CodeGraph,
    prompt: &str,
    use_neural_scores: bool,
    blend: NeuralSelectionBlend,
) -> Vec<RankedCandidate> {
    let query = crate::snooper::query_tokens::resolve_structural_query(graph, prompt);
    if !query.has_usable_hits() {
        return vec![];
    }

    let candidates: Vec<&BlockInfo> = if use_neural_scores {
        graph
            .nodes
            .values()
            .filter(|b| {
                if is_junk_symbol_name(&b.name) {
                    return false;
                }
                let neural = graph.neural_score_cache.get(&b.id).copied().unwrap_or(0.0);
                // Prefer structural text > 1 (exclude overshadowed residual 1.0 unless neural)
                let text = structural_text_score(b, &query);
                neural.abs() > 1e-12 || text > 1.5
            })
            .collect()
    } else {
        graph
            .nodes
            .values()
            .filter(|b| {
                !is_junk_symbol_name(&b.name) && structural_text_score(b, &query) > 1.5
            })
            .collect()
    };

    if candidates.is_empty() {
        return vec![];
    }

    rank_blocks_for_selection_subset(graph, &candidates, prompt, use_neural_scores, blend)
}

pub fn collect_with_scoring(
    graph: &CodeGraph,
    seed_blocks: Vec<BlockInfo>,
    options: &crate::ContextOptions,
    prompt: &str,
) -> super::Collection {
    let prompt_lower = prompt.to_lowercase();
    let keywords = extract_keywords(&prompt_lower);

    // Demoscene: move the (tiny) seed vec to collect (no pre-clone of BlockInfo data).
    // Only cheap Ids cloned for later seeding of the final BFS.
    let seed_ids: Vec<Id> = seed_blocks.iter().map(|b| b.id.clone()).collect();
    let raw_collection = collect(graph, seed_blocks, options);

    // Apply Working Set filter early (before scoring and the expensive second BFS).
    // This uses the improved filter_blocks_by_scope which handles Docker path translation
    // (relative client paths vs full container paths stored in the graph).
    let filtered_blocks = filter_blocks_by_scope(
        &raw_collection.blocks,
        &options.scope_paths,
        &options.ignore_paths,
    );

    // Compute scores for each block (we already own the filtered candidates after the
    // radius collection + scope filter; we mutate scores in place on our local copies).
    let mut scored_blocks = filtered_blocks;
    for b in &mut scored_blocks {
        if options.use_neural_scores {
            // Scores already injected by Butler's lambda-eve sidecar (graph node `.score`).
            continue;
        }
        b.score = score_block(b, &prompt_lower, &keywords, graph);
    }

    // Sort by score descending to prioritize high-score blocks
    scored_blocks.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

    // Strict deduplication: keep only first occurrence of each id (by highest score)
    let mut seen_ids = HashSet::new();
    let mut unique_blocks: Vec<BlockInfo> = Vec::with_capacity(scored_blocks.len());

    for block in scored_blocks {
        if seen_ids.insert(block.id.clone()) {
            unique_blocks.push(block);
        }
    }

    // Build a lookup map from id to *reference* (zero-copy of the heavy BlockInfo data).
    // The referenced data lives in `unique_blocks` which outlives the second BFS.
    let unique_map: std::collections::HashMap<Id, &BlockInfo> = unique_blocks
        .iter()
        .map(|block| (block.id.clone(), block))
        .collect();

    // Re-collect with BFS from seeds to maintain graph structure, but use scored blocks.
    // Queue/seen use cheap Ids only. We clone a BlockInfo only when we decide to
    // include it in the final result (include-time clone).
    let mut queue = VecDeque::with_capacity(64);
    let mut final_seen = HashSet::with_capacity(128);
    let mut final_blocks = Vec::with_capacity(64);

    // Add seed blocks first (they always appear). Use pre-collected cheap Ids (no pre-clone of full BlockInfo vec for the collect call).
    // Fallback uses graph (authoritative) or unique_map.
    for id in seed_ids {
        if final_seen.insert(id.clone()) {
            queue.push_back(id.clone());
            let block_to_add = unique_map
                .get(&id)
                .copied()
                .unwrap_or_else(|| graph.nodes.get(&id).expect("seed should exist in graph"));
            final_blocks.push(block_to_add.clone());
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

    // Compute hub threshold once for the expansion
    // TODO: Make 5% configurable
    let hub_threshold = crate::snooper::composer::highly_connected_threshold(graph); // reuse from composer for now

    while let Some(current_id) = queue.pop_front() {
        if collected >= max_blocks {
            break;
        }

        // Process children
        for child_id in graph.children(&current_id) {
            if final_seen.contains(&child_id) {
                continue;
            }

            // get_block takes Id by value, so we clone the (cheap) Id for the lookup.
            // We keep the original child_id for the queue if we decide to enqueue.
            let block_ref: Option<&BlockInfo> = unique_map
                .get(&child_id)
                .copied()
                .or_else(|| graph.get_block(child_id.clone()));

            if let Some(block) = block_ref {
                let is_hub = {
                    let in_deg = graph.reverse.get(&block.id).map_or(0, |v| v.len());
                    let out_deg = graph.edges.get(&block.id).map_or(0, |v| v.len());
                    (in_deg + out_deg) >= hub_threshold
                };

                if is_hub {
                    // Include the hub
                    final_seen.insert(child_id.clone());
                    final_blocks.push(block.clone()); // clone only at include time
                    collected += 1;

                    // Add top 5 highest scoring direct neighbors of the hub (as "leaves")
                    // We don't enqueue the hub, so no further expansion from it.
                    let hub_neighbors =
                        get_top_scoring_neighbors_for_collection(&block.id, graph, &unique_map, 5);
                    for nb in hub_neighbors {
                        if final_seen.insert(nb.id.clone()) {
                            final_blocks.push(nb.clone()); // clone only at include time
                            collected += 1;
                        }
                    }
                } else {
                    // Normal node: include and enqueue for further expansion
                    if final_seen.insert(child_id.clone()) {
                        queue.push_back(child_id);
                        final_blocks.push(block.clone()); // clone only at include time
                        collected += 1;
                    }
                }
            }
        }

        // Process callers (same logic)
        for caller_id in graph.callers(&current_id) {
            if final_seen.contains(&caller_id) {
                continue;
            }

            let block_ref: Option<&BlockInfo> = unique_map
                .get(&caller_id)
                .copied()
                .or_else(|| graph.get_block(caller_id.clone()));

            if let Some(block) = block_ref {
                let is_hub = {
                    let in_deg = graph.reverse.get(&block.id).map_or(0, |v| v.len());
                    let out_deg = graph.edges.get(&block.id).map_or(0, |v| v.len());
                    (in_deg + out_deg) >= hub_threshold
                };

                if is_hub {
                    final_seen.insert(caller_id.clone());
                    final_blocks.push(block.clone());
                    collected += 1;

                    let hub_neighbors =
                        get_top_scoring_neighbors_for_collection(&block.id, graph, &unique_map, 5);
                    for nb in hub_neighbors {
                        if final_seen.insert(nb.id.clone()) {
                            final_blocks.push(nb.clone());
                            collected += 1;
                        }
                    }
                } else {
                    if final_seen.insert(caller_id.clone()) {
                        queue.push_back(caller_id);
                        final_blocks.push(block.clone());
                        collected += 1;
                    }
                }
            }
        }
    }

    // Final safety filter: ensure the returned blocks respect the Working Set even after
    // the second BFS + hub neighbor expansion (handles any edge cases in path normalization).
    //
    // We use the private owned variant so that we move the blocks we already cloned at
    // include-time and only clone the final passers once more (the unavoidable cost for
    // the returned Collection).
    let final_blocks = super::filter_blocks_by_scope_owned(
        final_blocks,
        &options.scope_paths,
        &options.ignore_paths,
    );
    let final_ids: HashSet<Id> = final_blocks.iter().map(|b| b.id.clone()).collect();

    super::Collection {
        blocks: final_blocks,
        selected_ids: final_ids,
    }
}

/// Hybrid structural-token + (optional) neural blend selector.
///
/// Tokens are graph-membership filtered (no NL dictionaries). Exact name beats
/// bare suffix. Dedupes by `(name, file)` so take-8 is distinct symbols.
pub fn select_blocks(
    graph: &CodeGraph,
    prompt: &str,
    use_neural_scores: bool,
    blend: NeuralSelectionBlend,
) -> Vec<BlockInfo> {
    let ranked = rank_blocks_for_selection(graph, prompt, use_neural_scores, blend);
    let mut seen_keys: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(8);
    for r in ranked {
        if is_junk_symbol_name(&r.name) || r.blended <= 0.0 {
            continue;
        }
        let Some(b) = graph.nodes.get(&r.id) else {
            continue;
        };
        let key = format!(
            "{}|{}",
            b.name.to_lowercase(),
            b.file.to_string_lossy().to_lowercase()
        );
        if !seen_keys.insert(key) {
            continue;
        }
        let mut cloned = b.clone();
        cloned.score = r.blended;
        out.push(cloned);
        if out.len() >= 8 {
            break;
        }
    }
    out
}
