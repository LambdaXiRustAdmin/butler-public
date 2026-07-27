//! Neighborhood-card frontier (Butler as navigator, not sniper search).
//!
//! Seeds centers via:
//! - expand_critical: unvisited 1-hop neighbor of an existing critical (structure growth)
//! - random_walk: uniform random eligible node (topological diversity)
//! - query_seed: mild name/source keyword hit (optional minority)
//!
//! Never defaults to global top-degree hubs.

use super::cards::{build_card_with_budget, CardBudget, NeighborhoodCard};
use super::source::Source;
use super::types::FatGraph;
use code_graph::snooper::model::CodeGraph;
use std::collections::HashSet;

/// How to pick the next batch of centers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedStrategy {
    /// Mix: expand-from-critical when possible, else random walk, light query seeds.
    Neighborhood,
    /// Legacy name maps to Neighborhood.
    RandomWalk,
    /// Old degree-heavy path (kept for A/B; not default).
    PriorityLegacy,
}

impl SeedStrategy {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "priority" | "legacy" | "degree" => SeedStrategy::PriorityLegacy,
            "random_walk" | "random" => SeedStrategy::RandomWalk,
            _ => SeedStrategy::Neighborhood,
        }
    }
}

fn in_scope(file: &str, scope_paths: &[String], ignore_paths: &[String]) -> bool {
    let in_scope = scope_paths.is_empty() || scope_paths.iter().any(|s| file.contains(s));
    let not_ignored = ignore_paths.is_empty() || !ignore_paths.iter().any(|s| file.contains(s));
    in_scope && not_ignored
}

/// Definition-like centers only — never seed on call_expression / match_arm candy.
pub fn is_labelable_kind(kind: &str) -> bool {
    let k = kind.to_lowercase();
    // Rust / Python / TS-ish definition forms Tree-sitter emits.
    const OK: &[&str] = &[
        "function_item",
        "function_definition",
        "method_definition",
        "struct_item",
        "enum_item",
        "impl_item",
        "trait_item",
        "mod_item",
        "class_definition",
        "class_declaration",
        "type_alias_item",
        "const_item",
        "static_item",
        "macro_definition",
        "interface_declaration",
        "type_alias_declaration",
        "export_statement", // only if name is real — still better than match_arm
    ];
    OK.iter().any(|p| k == *p)
        || k.contains("function")
        || k.contains("struct")
        || k.contains("class")
        || k.contains("impl")
        || k.contains("trait")
        || k.contains("enum")
        || (k.contains("mod") && !k.contains("module_import"))
}

fn is_junk_name(name: &str) -> bool {
    let n = name.trim();
    n.is_empty()
        || n == "unknown"
        || n == "Some"
        || n == "None"
        || n == "Ok"
        || n == "Err"
        || n == "result"
        || n.len() == 1
}

fn eligible_ids(
    g: &CodeGraph,
    visited: &HashSet<String>,
    scope_paths: &[String],
    ignore_paths: &[String],
) -> Vec<String> {
    g.nodes
        .values()
        .filter(|b| !visited.contains(b.id.as_str()))
        .filter(|b| in_scope(&b.file.to_string_lossy(), scope_paths, ignore_paths))
        .filter(|b| is_labelable_kind(&b.kind))
        .filter(|b| !is_junk_name(&b.name))
        // Prefer production-ish paths over *test packages*, not every path with "test" in it
        // (e.g. examples/test_data must remain harvestable).
        .filter(|b| {
            let f = b.file.to_string_lossy().to_lowercase();
            let is_test_pkg = f.contains("/tests/")
                || f.contains("\\tests\\")
                || f.contains("/test/")
                || f.contains("\\test\\")
                || f.ends_with("_test.rs")
                || f.ends_with("_test.py")
                || f.ends_with("_test.ts")
                || f.ends_with(".test.ts")
                || f.ends_with(".test.js")
                || f.ends_with("_spec.rs");
            !is_test_pkg
        })
        .map(|b| b.id.as_str().to_string())
        .collect()
}

/// Numerical Recipes / Knuth-style LCG for **deterministic** card picks (repro gold runs).
/// Not cryptographic; period is large enough for harvest sampling, not for security.
/// Multiplier `6364136223846793005` is the common 64-bit LCG constant; we take high bits.
fn next_u32(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1);
    (*state >> 33) as u32
}

fn pick_index(state: &mut u64, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (next_u32(state) as usize) % len
}

fn query_hits(g: &CodeGraph, eligible: &[String], query: &str) -> Vec<String> {
    let lower = query.to_lowercase();
    let terms: Vec<_> = lower
        .split_whitespace()
        .filter(|t| t.len() > 2)
        .collect();
    if terms.is_empty() {
        return vec![];
    }
    let mut scored: Vec<(String, i32)> = eligible
        .iter()
        .filter_map(|id| {
            let b = g.nodes.values().find(|b| b.id.as_str() == id)?;
            let score = terms
                .iter()
                .filter(|t| {
                    b.name.to_lowercase().contains(*t) || b.source.to_lowercase().contains(*t)
                })
                .count() as i32;
            if score > 0 {
                Some((id.clone(), score))
            } else {
                None
            }
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.into_iter().map(|(id, _)| id).collect()
}

fn expand_from_criticals(
    g: &CodeGraph,
    critical_ids: &[String],
    visited: &HashSet<String>,
    eligible: &HashSet<String>,
    rng: &mut u64,
) -> Option<String> {
    if critical_ids.is_empty() {
        return None;
    }
    // Try several random criticals for an unvisited *labelable* neighbor.
    for _ in 0..critical_ids.len().min(8) {
        let c = &critical_ids[pick_index(rng, critical_ids.len())];
        let Some(key) = g.nodes.keys().find(|k| k.as_str() == c) else {
            continue;
        };
        let mut neigh: Vec<String> = Vec::new();
        let push = |neigh: &mut Vec<String>, nid: &code_graph::snooper::model::Id| {
            let s = nid.as_str().to_string();
            if visited.contains(&s) || !eligible.contains(&s) {
                return;
            }
            if let Some(b) = g.nodes.get(nid) {
                if is_labelable_kind(&b.kind) && !is_junk_name(&b.name) {
                    neigh.push(s);
                }
            }
        };
        if let Some(outs) = g.edges.get(key) {
            for n in outs {
                push(&mut neigh, n);
            }
        }
        if let Some(ins) = g.reverse.get(key) {
            for n in ins {
                push(&mut neigh, n);
            }
        }
        // Same-file definitions as structural fallback when edges are thin.
        if neigh.is_empty() {
            if let Some(cb) = g.nodes.get(key) {
                let file = cb.file.clone();
                for b in g.nodes.values() {
                    if b.file == file
                        && b.id.as_str() != c
                        && !visited.contains(b.id.as_str())
                        && eligible.contains(b.id.as_str())
                        && is_labelable_kind(&b.kind)
                        && !is_junk_name(&b.name)
                    {
                        neigh.push(b.id.as_str().to_string());
                    }
                }
            }
        }
        if !neigh.is_empty() {
            return Some(neigh[pick_index(rng, neigh.len())].clone());
        }
    }
    None
}

/// Next batch of neighborhood cards for the LLM.
pub fn next_cards(
    current: &FatGraph,
    batch_size: usize,
    query: &str,
    source: &Source,
    scope_paths: &[String],
    ignore_paths: &[String],
    strategy: SeedStrategy,
    use_degree_bias: bool,
) -> Vec<NeighborhoodCard> {
    next_cards_with_budget(
        current,
        batch_size,
        query,
        source,
        scope_paths,
        ignore_paths,
        strategy,
        use_degree_bias,
        CardBudget::default(),
    )
}

pub fn next_cards_with_budget(
    current: &FatGraph,
    batch_size: usize,
    query: &str,
    source: &Source,
    scope_paths: &[String],
    ignore_paths: &[String],
    strategy: SeedStrategy,
    use_degree_bias: bool,
    budget: CardBudget,
) -> Vec<NeighborhoodCard> {
    let Some(g) = source.load_code_graph() else {
        return vec![];
    };

    let visited: HashSet<_> = current.nodes.iter().map(|n| n.id.clone()).collect();
    let mut eligible = eligible_ids(g, &visited, scope_paths, ignore_paths);
    if eligible.is_empty() {
        return vec![];
    }

    let mut rng = (current.nodes.len() as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(query.len() as u64)
        .wrapping_add(1);

    let eligible_set: HashSet<_> = eligible.iter().cloned().collect();
    let hits = query_hits(g, &eligible, query);

    let mut centers: Vec<(String, &'static str)> = Vec::new();

    match strategy {
        SeedStrategy::PriorityLegacy => {
            // Mild legacy: query match + optional degree, still not pure hub dump.
            let mut degree_map: std::collections::HashMap<String, i32> =
                std::collections::HashMap::new();
            if use_degree_bias {
                for (id, outs) in &g.edges {
                    *degree_map.entry(id.as_str().to_string()).or_insert(0) += outs.len() as i32;
                }
                for (id, ins) in &g.reverse {
                    *degree_map.entry(id.as_str().to_string()).or_insert(0) += ins.len() as i32;
                }
            }
            let mut scored: Vec<(String, i32)> = eligible
                .iter()
                .map(|id| {
                    let q = if hits.iter().any(|h| h == id) { 3 } else { 0 };
                    let d = if use_degree_bias {
                        degree_map.get(id).copied().unwrap_or(0).min(5)
                    } else {
                        0
                    };
                    (id.clone(), q + d)
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1));
            for (id, _) in scored.into_iter().take(batch_size) {
                centers.push((id, "priority_legacy"));
            }
        }
        SeedStrategy::RandomWalk | SeedStrategy::Neighborhood => {
            while centers.len() < batch_size && !eligible.is_empty() {
                // Prefer structure growth once we have gold criticals (steak, not random candy).
                let roll = next_u32(&mut rng) % 100;
                let expand_threshold = if current.critical_node_ids.is_empty() {
                    15u32
                } else {
                    65u32
                };
                let pick = if roll < expand_threshold {
                    expand_from_criticals(
                        g,
                        &current.critical_node_ids,
                        &visited,
                        &eligible_set,
                        &mut rng,
                    )
                    .map(|id| (id, "expand_critical"))
                } else if roll < expand_threshold + 15 && !hits.is_empty() {
                    // Mild query seed among definition-like hits only
                    let id = hits[pick_index(&mut rng, hits.len().min(16))].clone();
                    if eligible_set.contains(&id) && !centers.iter().any(|(c, _)| c == &id) {
                        Some((id, "query_seed"))
                    } else {
                        None
                    }
                } else {
                    None
                };

                let (id, reason) = if let Some(p) = pick {
                    p
                } else {
                    let idx = pick_index(&mut rng, eligible.len());
                    (eligible[idx].clone(), "random_walk")
                };

                // Remove from eligible pool
                if let Some(pos) = eligible.iter().position(|e| e == &id) {
                    eligible.swap_remove(pos);
                }
                if centers.iter().any(|(c, _)| c == &id) {
                    continue;
                }
                centers.push((id, reason));
            }
        }
    }

    centers
        .into_iter()
        .filter_map(|(id, reason)| build_card_with_budget(g, &id, query, reason, budget))
        .collect()
}

/// Back-compat: id-only batch (tests / callers that still want ids).
pub fn next_batch(
    current: &FatGraph,
    batch_size: usize,
    query: &str,
    source: &Source,
    scope_paths: &[String],
    ignore_paths: &[String],
) -> Vec<String> {
    next_cards(
        current,
        batch_size,
        query,
        source,
        scope_paths,
        ignore_paths,
        SeedStrategy::Neighborhood,
        false,
    )
    .into_iter()
    .map(|c| c.center_id)
    .collect()
}
