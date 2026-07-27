//! Prompt subgraph retrieval for the neural scoring cascade.
//!
//! Funnel:
//! 1. Resolve prompt → graph-membership tokens (no NL dictionaries)
//! 2. L0 — module path hits from those tokens
//! 3. L1 — structural name/path score (exact ≫ suffix ≫ segment)
//! 4. Hop expand → tiny GNN cluster
//!
//! Never full-graph source scan.

use std::collections::{HashMap, HashSet};

use super::model::{CodeGraph, Id};
use super::query_tokens::{
    is_junk_symbol_name, resolve_structural_query, structural_block_score, StructuralQuery,
};

/// Subgraph node set plus per-node retriever text_match scores for prompt-conditioned GNN features.
#[derive(Debug, Clone)]
pub struct PromptSubgraph {
    pub node_ids: HashSet<Id>,
    pub text_match_scores: HashMap<Id, f64>,
}

const L1_SCORE_CAP: usize = 4096;

/// Name+path structural score (kept for tests / external callers).
pub fn retriever_text_score(
    block: &super::model::BlockInfo,
    prompt_lower: &str,
    keywords: &[&str],
) -> f64 {
    let q = StructuralQuery {
        graph_hits: keywords.iter().map(|s| (*s).to_string()).collect(),
        strong_unmatched: vec![],
        exact_name_hits: vec![],
        prompt_lower: prompt_lower.to_string(),
    };
    structural_block_score(block, &q)
}

fn expand_hops(graph: &CodeGraph, seeds: &HashSet<Id>, hops: usize) -> HashSet<Id> {
    let mut result = seeds.clone();
    let mut frontier = seeds.clone();
    for _ in 0..hops {
        let mut next = HashSet::new();
        for id in &frontier {
            for neighbor in graph.children(id).into_iter().chain(graph.callers(id)) {
                if result.insert(neighbor.clone()) {
                    next.insert(neighbor);
                }
            }
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }
    result.retain(|id| {
        graph
            .nodes
            .get(id)
            .map(|b| !is_junk_symbol_name(&b.name))
            .unwrap_or(false)
    });
    result
}

fn module_keyword_score(module_path: &str, tokens: &[&str]) -> f64 {
    if tokens.is_empty() {
        return 0.0;
    }
    let lower = module_path.to_lowercase();
    let mut score = 0.0;
    for t in tokens {
        let k = t.to_lowercase();
        if k.len() < 3 {
            continue;
        }
        if lower.contains(&k) {
            score += 10.0 + k.len() as f64;
            if lower
                .rsplit('/')
                .next()
                .is_some_and(|seg| seg == k || seg.contains(&k))
            {
                score += 8.0;
            }
        }
    }
    score
}

fn l0_module_prefixes(
    graph: &CodeGraph,
    tokens: &[&str],
    l0_modules: usize,
) -> Option<HashSet<String>> {
    if l0_modules == 0 || tokens.is_empty() {
        return None;
    }

    let mut ranked: Vec<(String, f64)> = if graph.module_hashes.is_empty() {
        let mut seen = HashSet::new();
        graph
            .nodes
            .values()
            .filter_map(|b| {
                let key = CodeGraph::module_key_for_path(&b.file.to_string_lossy());
                if seen.insert(key.clone()) {
                    let s = module_keyword_score(&key, tokens);
                    if s > 0.0 {
                        Some((key, s))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect()
    } else {
        graph
            .module_hashes
            .keys()
            .filter_map(|m| {
                let s = module_keyword_score(m, tokens);
                if s > 0.0 {
                    Some((m.clone(), s))
                } else {
                    None
                }
            })
            .collect()
    };

    if ranked.is_empty() {
        return None;
    }

    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let take = l0_modules.min(ranked.len());
    Some(ranked.into_iter().take(take).map(|(m, _)| m).collect())
}

fn block_in_modules(block: &super::model::BlockInfo, modules: &HashSet<String>) -> bool {
    let key = CodeGraph::module_key_for_path(&block.file.to_string_lossy());
    if modules.contains(&key) {
        return true;
    }
    let path = block.file.to_string_lossy().replace('\\', "/");
    modules.iter().any(|m| {
        path == *m
            || path.starts_with(&format!("{m}/"))
            || key.starts_with(&format!("{m}/"))
            || key == *m
    })
}

pub fn retrieve_prompt_subgraph(
    graph: &CodeGraph,
    prompt: &str,
    top_n: usize,
    hops: usize,
) -> PromptSubgraph {
    retrieve_prompt_subgraph_with_l0(graph, prompt, top_n, hops, 32)
}

/// Structural tokens → L0 modules → L1 name ranking → hop expand.
pub fn retrieve_prompt_subgraph_with_l0(
    graph: &CodeGraph,
    prompt: &str,
    top_n: usize,
    hops: usize,
    l0_modules: usize,
) -> PromptSubgraph {
    let query = resolve_structural_query(graph, prompt);
    let tokens = query.ranking_tokens();

    if query.has_usable_hits() {
        println!(
            "🔎 Structural query: {} strong graph-hit token(s) {:?}",
            tokens.len(),
            tokens.iter().take(8).collect::<Vec<_>>()
        );
    } else if !query.strong_unmatched.is_empty() {
        println!(
            "🔎 Structural query: FAIL-CLOSED (no graph hits; strong unmatched {:?})",
            query
                .strong_unmatched
                .iter()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
        );
        return PromptSubgraph {
            node_ids: HashSet::new(),
            text_match_scores: HashMap::new(),
        };
    } else {
        println!("🔎 Structural query: FAIL-CLOSED (prose only / no strong membership)");
        return PromptSubgraph {
            node_ids: HashSet::new(),
            text_match_scores: HashMap::new(),
        };
    }

    let l0 = l0_module_prefixes(graph, &tokens, l0_modules);
    if let Some(ref mods) = l0 {
        println!(
            "🔎 L0 funnel: {} modules from structural tokens",
            mods.len()
        );
    }

    let mut scored: Vec<(Id, f64)> = Vec::new();
    for b in graph.nodes.values() {
        if is_junk_symbol_name(&b.name) {
            continue;
        }
        if let Some(ref mods) = l0 {
            if !block_in_modules(b, mods) {
                continue;
            }
        }
        let s = structural_block_score(b, &query);
        if s > 0.0 {
            scored.push((b.id.clone(), s));
        }
    }

    if scored.is_empty() && l0.is_some() {
        println!("🔎 L0 empty — widening to full-graph structural scores");
        for b in graph.nodes.values() {
            if is_junk_symbol_name(&b.name) {
                continue;
            }
            let s = structural_block_score(b, &query);
            if s > 0.0 {
                scored.push((b.id.clone(), s));
            }
        }
    }

    // No hub/sample fallback: empty structural scores ⇒ empty subgraph (fail-closed).
    if scored.is_empty() {
        println!("🔎 L1 structural scores empty — FAIL-CLOSED (no seeds)");
        return PromptSubgraph {
            node_ids: HashSet::new(),
            text_match_scores: HashMap::new(),
        };
    }

    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.as_str().cmp(b.0.as_str()))
    });
    if scored.len() > L1_SCORE_CAP {
        scored.truncate(L1_SCORE_CAP);
    }

    let text_match_scores: HashMap<Id, f64> = scored.iter().cloned().collect();
    let take_n = top_n.min(scored.len());
    let seeds: HashSet<Id> = scored.into_iter().take(take_n).map(|(id, _)| id).collect();

    let node_ids = if hops == 0 {
        seeds
    } else {
        expand_hops(graph, &seeds, hops)
    };

    let subgraph_text_scores: HashMap<Id, f64> = node_ids
        .iter()
        .map(|id| {
            let s = text_match_scores.get(id).copied().unwrap_or_else(|| {
                graph
                    .nodes
                    .get(id)
                    .map(|b| structural_block_score(b, &query))
                    .unwrap_or(0.0)
            });
            (id.clone(), s)
        })
        .collect();

    println!(
        "🔎 L1 structural-seeds={} expanded={} (top_n={} hops={})",
        take_n,
        node_ids.len(),
        top_n,
        hops
    );

    PromptSubgraph {
        node_ids,
        text_match_scores: subgraph_text_scores,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::model::BlockInfo;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn block(file: &str, name: &str) -> BlockInfo {
        BlockInfo::new(
            PathBuf::from(file),
            "function_item",
            "rust",
            1,
            3,
            0,
            10,
            format!("fn {name}() {{}}"),
            name,
            HashSet::new(),
        )
    }

    #[test]
    fn structural_seeds_prefer_full_symbol() {
        let mut g = CodeGraph::new();
        let b1 = block("src/gnn/forward.rs", "cpu_gnn_forward");
        let b2 = block("src/x.rs", "forward");
        g.nodes.insert(b1.id.clone(), b1);
        g.nodes.insert(b2.id.clone(), b2);
        g.file_hashes.insert("src/gnn/forward.rs".into(), 1);
        g.file_hashes.insert("src/x.rs".into(), 1);
        g.rebuild_module_hashes();

        let sub = retrieve_prompt_subgraph_with_l0(&g, "cpu_gnn_forward", 5, 0, 32);
        let mut ranked: Vec<_> = sub
            .text_match_scores
            .iter()
            .filter_map(|(id, s)| g.nodes.get(id).map(|b| (b.name.as_str(), *s)))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        assert_eq!(ranked[0].0, "cpu_gnn_forward", "got {:?}", ranked);
    }

    #[test]
    fn prose_without_identifiers_fail_closed_empty() {
        let mut g = CodeGraph::new();
        let b = block("src/a.rs", "something_real");
        g.nodes.insert(b.id.clone(), b);
        let sub = retrieve_prompt_subgraph_with_l0(&g, "jak to działa proszę", 5, 0, 32);
        assert!(
            sub.node_ids.is_empty(),
            "prose-only must fail-closed, got {:?}",
            sub.node_ids
        );
    }

    #[test]
    fn fake_symbol_fail_closed_empty() {
        let mut g = CodeGraph::new();
        let b = block("src/a.rs", "load_graph");
        g.nodes.insert(b.id.clone(), b);
        let sub = retrieve_prompt_subgraph_with_l0(&g, "QuantumFluxTransducer", 5, 0, 32);
        assert!(sub.node_ids.is_empty());
    }
}
