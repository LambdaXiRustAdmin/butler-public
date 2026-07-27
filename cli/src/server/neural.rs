//! Butler Rank apply path (SmartButler GNN relevance).
//!
//! Implementation: `code_graph::gnn` (projection + CPU R-GCN). No Eve subprocess for scoring.
//! **Parked product:** optional addon — only runs when `agent.use_neural` is true (default false).
//! Plan: `plans/butler-rank-addon.md`. Do not delete this module for "cleanup"; leave the hooks.
//! Eve remains the Xi connector + training harness (not runtime scoring).

use crate::vprintln;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use code_graph::{
    apply_heuristic_scores_subset, retrieve_prompt_subgraph_with_l0, CodeGraph, Id,
};

use cli::config::ButlerSettings;

/// Store (pure) GNN neural scores into the graph's neural_score_cache (and block.score for the subgraph).
/// Nodes outside the subgraph receive neural score 0.

pub fn apply_scores_to_graph(
    graph: &mut CodeGraph,
    scores: &HashMap<String, f64>,
    subgraph_ids: &HashSet<Id>,
) {
    // Hot path: only touch the subgraph. Zeroing every node in a 200k graph is pure
    // single-core heat and made every Trace look like full-warehouse scoring.
    for id in subgraph_ids {
        let score = scores.get(id.as_str()).copied().unwrap_or(0.0);
        graph.neural_score_cache.insert(id.clone(), score);
        if let Some(block) = graph.nodes.get_mut(id) {
            block.score = score;
        }
    }
}

/// Step 9: In-process GNN scoring (moved from Eve).
/// Butler now runs the CPU forward natively using code_graph::gnn.
/// This eliminates the brittle subprocess and makes scoring part of SmartButler.
pub fn try_apply_neural_scores(
    graph_rw: &Arc<RwLock<CodeGraph>>,
    project_root: &str,
    settings: &ButlerSettings,
    prompt: &str,
) -> bool {
    let top_n = settings.agent.neural_subgraph_top_n;
    let hops = settings.agent.neural_subgraph_hops;
    let l0_modules = settings.agent.neural_l0_modules;

    let subgraph = {
        let guard = graph_rw.read().unwrap_or_else(|p| p.into_inner());
        // Structural membership → L0/L1 → hop expand. Empty = fail-closed (no hub spam).
        retrieve_prompt_subgraph_with_l0(&guard, prompt, top_n, hops, l0_modules)
    };

    if subgraph.node_ids.is_empty() {
        vprintln!("🧠 Neural skipped — structural query fail-closed (empty subgraph)");
        return false;
    }

    {
        let mut guard = graph_rw.write().unwrap_or_else(|p| p.into_inner());
        apply_heuristic_scores_subset(&mut guard, prompt, &subgraph.node_ids);
    }

    // Export write removed from scoring path (was for old eve sidecar).
    // Training/fat-graph paths use write_graph_export_for_nodes directly.

    // === Real in-process GNN (moved from Eve) ===
    let weights = code_graph::gnn::load_weights(project_root);
    let (gnn_raw, feats_shape) = {
        let guard = graph_rw.read().unwrap_or_else(|p| p.into_inner());
        let bundle = code_graph::gnn::build_scoring_input(&guard, &subgraph);
        let out = code_graph::gnn::cpu_gnn_forward(
            &weights,
            bundle.nodes.len(),
            &bundle.features,
            &bundle.edges,
        );
        let mut m = std::collections::HashMap::new();
        for (id, s) in bundle.nodes.into_iter().zip(out.into_iter()) {
            m.insert(id, s as f64);
        }
        (m, bundle.features.len())
    };

    // Store *pure* GNN scores. The caller (rank_blocks_for_selection / select_blocks)
    // applies the configured NeuralSelectionBlend (text + neural) using the caches.
    let mut scores = std::collections::HashMap::new();
    {
        for id in &subgraph.node_ids {
            let gnn = gnn_raw.get(id).copied().unwrap_or(0.0);
            scores.insert(id.as_str().to_string(), gnn);
        }
    }

    apply_diversity_penalty(&mut scores);
    strip_junk_scores(&mut scores);
    {
        let mut guard = graph_rw.write().unwrap_or_else(|p| p.into_inner());
        apply_scores_to_graph(&mut guard, &scores, &subgraph.node_ids);
    }
    vprintln!(
        "🧠 Neural (pure GNN) scores applied (in-process; {} nodes, feat={}B, wlen={})",
        scores.len(),
        feats_shape,
        weights.len()
    );
    true
}

/// Apply diversity penalty to GNN scores (penalize near-duplicates by file/name).
/// If a node shares exact file or has highly similar name to an already-selected top node,
/// its score is multiplied by 0.15 to encourage context diversity.

fn apply_diversity_penalty(scores: &mut HashMap<String, f64>) {
    if scores.is_empty() {
        return;
    }
    let mut ranked: Vec<(String, f64)> = scores.iter().map(|(k, v)| (k.clone(), *v)).collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut selected: Vec<String> = vec![];
    let mut adjusted = HashMap::new();
    for (id, score) in ranked {
        let mut penalty = 1.0;
        for prev in &selected {
            if same_file(&id, prev) || similar_name(&id, prev) {
                penalty *= 0.15;
                break;
            }
        }
        let final_score = score * penalty;
        adjusted.insert(id.clone(), final_score);
        selected.push(id);
    }
    *scores = adjusted;
}

fn same_file(id1: &str, id2: &str) -> bool {
    let f1 = id1.split(':').next().unwrap_or("");
    let f2 = id2.split(':').next().unwrap_or("");
    f1 == f2
}

fn similar_name(id1: &str, id2: &str) -> bool {
    let n1 = id1.split(':').nth(1).unwrap_or("").to_lowercase();
    let n2 = id2.split(':').nth(1).unwrap_or("").to_lowercase();
    if n1 == n2 {
        return true;
    }
    if n1.len() > 3 && n2.len() > 3 && (n1.contains(&n2) || n2.contains(&n1)) {
        return true;
    }
    false
}

/// Drop junk single-letter / shell names from the score map before apply.
fn strip_junk_scores(scores: &mut HashMap<String, f64>) {
    scores.retain(|id, _| {
        let name = id.split(':').nth(1).unwrap_or(id);
        !code_graph::is_junk_symbol_name(name)
    });
}
