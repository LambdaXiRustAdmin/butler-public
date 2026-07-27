//! Reverse CALL spine: seed <- parent <- ... toward entry (compact Trace).
//!
//! Algorithm: not a naive who-calls loop. Filter every hop by Direction, Trust, Topology:
//! 1. Direction - Incoming CALL only (never callees).
//! 2. Trust - Drop trait/boilerplate, testish, hard Trace-noise names.
//! 3. Topology - Halt at 0 product callers (entry) or fan-in > SPINE_FANIN_HALT (hub).
//!
//! Lives here so mod.rs / render.rs stay lean.

use crate::server::dto::CallerCallee;
use crate::server::filters::{
    caller_callee_from_block_at_hop, is_testish_seed_block, is_trace_noise_name,
};
use code_graph::{BlockInfo, CodeGraph, Id, ProjectPaths};
use std::collections::HashSet;
use std::path::Path;

/// Max hops up the reverse CALL chain (not including seed).
pub(crate) const SPINE_MAX_HOPS: usize = 4;
/// Product fan-in above this => hub scale terminus (not a linear spine).
pub(crate) const SPINE_FANIN_HALT: usize = 5;

/// True if this block must never sit on a reverse CALL spine.
fn is_spine_noise_block(b: &BlockInfo) -> bool {
    if is_testish_seed_block(b) {
        return true;
    }
    if is_trace_noise_name(&b.name) {
        return true;
    }
    if super::render::is_trace_neighbor_noise_name(&b.name) {
        return true;
    }
    let k = b.kind.to_ascii_lowercase();
    if !(k.contains("function") || k.contains("method") || k.contains("fn")) {
        return true;
    }
    false
}

/// Product CALL fan-in after noise wall (hub scale).
fn product_call_fanin(graph: &CodeGraph, b: &BlockInfo) -> usize {
    graph
        .callers(&b.id)
        .into_iter()
        .filter_map(|id| graph.get_block(id))
        .filter(|c| !is_spine_noise_block(c))
        .count()
}

fn resolve_block_for_hint<'a>(graph: &'a CodeGraph, h: &CallerCallee) -> Option<&'a BlockInfo> {
    let file_tail = h.file.rsplit('/').next().unwrap_or(h.file.as_str());
    let mut best: Option<(&BlockInfo, usize)> = None;
    for b in graph.blocks_for_name(&h.name) {
        let bf = b.file.to_string_lossy().replace('\\', "/");
        if !file_tail.is_empty() && !bf.ends_with(file_tail) {
            continue;
        }
        let dist = b.start_line.abs_diff(h.line);
        match best {
            None => best = Some((b, dist)),
            Some((_, d)) if dist < d => best = Some((b, dist)),
            _ => {}
        }
    }
    best.map(|(b, _)| b)
}

/// Map pack hop-1 parents to graph blocks (noise wall applied).
fn resolve_hint_parents<'a>(
    graph: &'a CodeGraph,
    hints: &[CallerCallee],
    seen: &HashSet<Id>,
) -> Vec<&'a BlockInfo> {
    let mut out: Vec<&BlockInfo> = Vec::new();
    let mut seen_ids: HashSet<Id> = HashSet::new();
    for h in hints {
        if h.hop > 1 {
            continue;
        }
        if is_trace_noise_name(&h.name)
            || super::render::is_trace_neighbor_noise_name(&h.name)
        {
            continue;
        }
        let b = match resolve_block_for_hint(graph, h) {
            Some(b) => b,
            None => continue,
        };
        if seen.contains(&b.id) || !seen_ids.insert(b.id.clone()) {
            continue;
        }
        if is_spine_noise_block(b) {
            continue;
        }
        out.push(b);
    }
    out
}

fn entry_name_rank(name: &str) -> i32 {
    let n = name.to_ascii_lowercase();
    if n.starts_with("handle_") || n.starts_with("run_") {
        return 3;
    }
    if n == "main" || n.starts_with("dispatch_") || n.contains("context") {
        return 2;
    }
    if n.starts_with("try_") || n.starts_with("do_") {
        return 1;
    }
    0
}

/// Walk reverse CALL from `target` toward entry. Returns parent...ancestors (seed omitted).
///
/// `direct_hints`: hop-1 pack parents when warehouse reverse is empty (first hop only).
pub(crate) fn reverse_call_spine(
    graph: &CodeGraph,
    target: &BlockInfo,
    root: &Path,
    pp: &ProjectPaths,
    direct_hints: &[CallerCallee],
) -> Vec<CallerCallee> {
    // Keep signature stable for callers; body starts simple for tree-sitter.
    reverse_call_spine_body(graph, target, root, pp, direct_hints)
}

fn reverse_call_spine_body(
    graph: &CodeGraph,
    target: &BlockInfo,
    root: &Path,
    pp: &ProjectPaths,
    direct_hints: &[CallerCallee],
) -> Vec<CallerCallee> {
    let _ = root;
    if crate::server::filters::is_type_trace_target(&target.kind) {
        return Vec::new();
    }

    let mut path: Vec<CallerCallee> = Vec::with_capacity(SPINE_MAX_HOPS);
    let mut cur = target.id.clone();
    let mut seen: HashSet<Id> = HashSet::new();
    seen.insert(cur.clone());
    let mut used_hint_bootstrap = false;

    for hop_usize in 1..=SPINE_MAX_HOPS {
        let hop = hop_usize as u8;
        let mut candidates: Vec<&BlockInfo> = Vec::new();

        // Hop-1: prefer dossier pack parents so spine[0] ∈ callers (I6).
        // Full reverse only when pack is empty (or resolve failed).
        if hop == 1 && !direct_hints.is_empty() && !used_hint_bootstrap {
            used_hint_bootstrap = true;
            candidates = resolve_hint_parents(graph, direct_hints, &seen);
        }

        if candidates.is_empty() {
            // Hop-1: hard CALL into ★ only (peer reverse is labeled separately in dossier).
            let raw = if hop == 1 {
                graph.callers(&target.id)
            } else {
                graph.callers(&cur)
            };
            for id in &raw {
                if seen.contains(id) {
                    continue;
                }
                let b = match graph.get_block(id.clone()) {
                    Some(b) => b,
                    None => continue,
                };
                if is_spine_noise_block(b) {
                    continue;
                }
                candidates.push(b);
            }
        }

        if candidates.is_empty() {
            break;
        }
        if candidates.len() > SPINE_FANIN_HALT {
            break;
        }

        candidates.sort_by(|a, b| {
            let fa = product_call_fanin(graph, a);
            let fb = product_call_fanin(graph, b);
            fa.cmp(&fb)
                .then_with(|| entry_name_rank(b.name.as_str()).cmp(&entry_name_rank(a.name.as_str())))
                .then_with(|| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.name.cmp(&b.name))
        });

        let best = candidates[0];
        let mut step = caller_callee_from_block_at_hop(best, pp, hop);
        step.cite = None;
        step.why = None;
        path.push(step);
        seen.insert(best.id.clone());

        if product_call_fanin(graph, best) > SPINE_FANIN_HALT {
            break;
        }
        cur = best.id.clone();
    }

    path
}

/// Resolve seed by id (or name+file fallback) and walk reverse CALL spine.
pub(crate) fn reverse_call_spine_for_seed(
    graph: &CodeGraph,
    root: &Path,
    seed_id: &str,
    seed_name: &str,
    seed_file: &str,
    direct_hints: &[CallerCallee],
) -> Vec<CallerCallee> {
    let pp = ProjectPaths::new(root);
    let id = Id::from_string(seed_id.to_string());
    let target = graph.get_block(id).or_else(|| {
        let file_tail = seed_file.rsplit('/').next().unwrap_or(seed_file);
        graph.nodes.values().find(|b| {
            b.name == seed_name
                && (seed_file.is_empty() || b.file.to_string_lossy().ends_with(file_tail))
        })
    });
    match target {
        Some(t) => reverse_call_spine(graph, t, root, &pp, direct_hints),
        None => Vec::new(),
    }
}

/// Compact Trace lines for reverse spine. Empty when no path.
pub(crate) fn compact_spine_lines(seed_name: &str, path: &[CallerCallee]) -> Vec<String> {
    if path.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::with_capacity(path.len() + 2);
    lines.push("call path (reverse spine · CALL only):".into());
    lines.push(format!("  {seed_name}"));
    for c in path {
        lines.push(format!(
            "  <- {} @ {}:{}",
            c.name,
            short_path(&c.file),
            c.line
        ));
    }
    lines
}

fn short_path(file: &str) -> String {
    let f = file.replace('\\', "/");
    let parts: Vec<&str> = f.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 {
        parts.join("/")
    } else {
        parts[parts.len() - 3..].join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanin_halt_constant_is_tight_pipeline() {
        assert!(SPINE_FANIN_HALT <= 8);
        assert!(SPINE_MAX_HOPS <= 5);
    }

    #[test]
    fn entry_name_rank_prefers_handle() {
        assert!(entry_name_rank("handle_context") > entry_name_rank("helper"));
        assert!(entry_name_rank("dispatch_tool") > entry_name_rank("helper"));
    }

    #[test]
    fn noise_wall_drops_trait_boilerplate_and_weak_names() {
        use super::super::render::is_trace_neighbor_noise_name;
        assert!(is_trace_neighbor_noise_name("fmt"));
        assert!(is_trace_neighbor_noise_name("clone"));
        assert!(!is_trace_neighbor_noise_name("dispatch_tool"));
    }

    #[test]
    fn compact_spine_empty_when_no_path() {
        assert!(compact_spine_lines("seed", &[]).is_empty());
    }

    #[test]
    fn compact_spine_renders_seed_and_parents() {
        let path = vec![CallerCallee {
            name: "dispatch_tool".into(),
            file: "/repo/cli/src/server/tools.rs".into(),
            line: 40,
            hop: 1,
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
            relation: None,
            cite: None,
            why: None,
        }];
        let lines = compact_spine_lines("handle_orchestrate", &path);
        let joined = lines.join("\n");
        assert!(joined.contains("call path (reverse spine · CALL only)"));
        assert!(joined.contains("  handle_orchestrate"));
        assert!(joined.contains("<- dispatch_tool @"));
        assert!(joined.contains("tools.rs:40"));
    }
}
