//! Trace CALL neighborhood expansion (L1→L2) + type-seed fallbacks.
//!
//! Extracted from `handle_orchestrate` (M1b) — zero intentional behavior change.
//! **Invariant:** fill L1 callers *and* L1 callees before any L2 hop (shared visited budget).

use code_graph::{BlockInfo, CodeGraph, Id};
use std::collections::HashSet;
use std::path::Path;

pub(crate) struct TraceLimits {
    max_fan_out: usize,
    max_visited_nodes: usize,
}

pub(crate) struct TraceStats {
    pub fan_out_pruned: usize,
    pub visited_capped: bool,
    pub nodes_visited: usize,
}

fn block_score(graph: &CodeGraph, id: &Id) -> f64 {
    graph.get_block(id.clone()).map(|b| b.score).unwrap_or(0.0)
}

fn type_trace_candidate_ok(
    candidate: &BlockInfo,
    target: &BlockInfo,
    is_test_block: &impl Fn(&BlockInfo) -> bool,
) -> bool {
    if candidate.id == target.id || is_test_block(candidate) {
        return false;
    }
    if crate::server::filters::is_homonym_type_def(candidate, target) {
        return false;
    }
    if crate::server::filters::is_peripheral_relative_to_target(candidate, target) {
        return false;
    }
    true
}

/// True for method/function-like blocks usable as type-neighborhood callees.
fn is_method_like_kind(kind: &str) -> bool {
    let k = kind.to_lowercase();
    k.contains("function")
        || k.contains("method")
        || k.contains("fn_item")
        || k.contains("function_item")
        || k.contains("function_definition")
        || k.contains("method_definition")
        || k.contains("constructor")
}

/// L1.1: method/function blocks nested in a type's line span (same file).
/// Works when sources are stripped — uses `file_node_index` + line ranges only.
fn type_nested_method_ids(graph: &CodeGraph, target: &BlockInfo) -> Vec<Id> {
    if target.end_line <= target.start_line {
        return Vec::new();
    }
    // Warehouse keys are repo-relative slash paths; try a few dialects.
    let raw = target.file.to_string_lossy().replace('\\', "/");
    let key_candidates = [
        raw.trim_start_matches("./").to_string(),
        raw.clone(),
        code_graph::snooper::utils::normalize_path(&raw),
    ];
    let mut file_ids: Option<&Vec<code_graph::Id>> = None;
    for k in &key_candidates {
        if let Some(ids) = graph.file_node_index.get(k) {
            file_ids = Some(ids);
            break;
        }
    }
    // Fallback: scan index keys ending with file basename (O(files) worst case — rare).
    if file_ids.is_none() {
        let base = target
            .file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if !base.is_empty() {
            for (k, ids) in &graph.file_node_index {
                if k.ends_with(base) {
                    file_ids = Some(ids);
                    break;
                }
            }
        }
    }
    let Some(ids) = file_ids else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for id in ids {
        if id == &target.id {
            continue;
        }
        let Some(b) = graph.get_block(id.clone()) else {
            continue;
        };
        // Strictly nested in type body (Python class methods, Rust impl-ish co-location).
        if b.start_line <= target.start_line || b.end_line > target.end_line {
            continue;
        }
        if !is_method_like_kind(&b.kind) {
            continue;
        }
        out.push(id.clone());
    }
    out
}

/// Usage/reference sites for struct/class/type targets (call graph alone is often empty).
fn collect_type_reference_ids(
    graph: &CodeGraph,
    target: &BlockInfo,
    visited: &mut HashSet<Id>,
    limits: &TraceLimits,
    stats: &mut TraceStats,
    is_noisy: &impl Fn(&str) -> bool,
    is_test_block: &impl Fn(&BlockInfo) -> bool,
    compress_tests: bool,
    test_omitted: &mut usize,
) -> Vec<Id> {
    let mut candidate_ids: Vec<Id> = graph.callers(&target.id);

    // Slim Complete warehouses strip sources — O(nodes) source scans are pure tax and
    // never match. Use CALL reverse + nested methods' callers (L1.1).
    if !graph.sources_stripped() {
        for b in graph.nodes.values() {
            if !type_trace_candidate_ok(b, target, is_test_block) {
                continue;
            }
            let k = b.kind.to_lowercase();
            if k.contains("impl")
                && crate::server::filters::contains_word_boundary(&b.source, &target.name)
            {
                candidate_ids.push(b.id.clone());
            }
        }

        for b in graph.nodes.values() {
            if !type_trace_candidate_ok(b, target, is_test_block) {
                continue;
            }
            let k = b.kind.to_lowercase();
            if !(k.contains("fn")
                || k.contains("function")
                || k.contains("method")
                || k.contains("impl")
                || k.contains("constructor"))
            {
                continue;
            }
            if crate::server::filters::contains_word_boundary(&b.source, &target.name) {
                candidate_ids.push(b.id.clone());
            }
        }
    } else {
        // Name peers (O(hits)): other exact-name defs often hang CALL edges we can use.
        for b in graph.blocks_for_name(&target.name) {
            if b.id == target.id {
                continue;
            }
            candidate_ids.extend(graph.callers(&b.id));
        }
        // Callers of nested methods ≈ type neighborhood (QuerySet.filter users, etc.).
        for mid in type_nested_method_ids(graph, target) {
            candidate_ids.extend(graph.callers(&mid));
        }
    }

    collect_trace_neighbors(
        graph,
        candidate_ids,
        visited,
        limits,
        stats,
        is_noisy,
        is_test_block,
        compress_tests,
        test_omitted,
        Some(target),
        &[], // type-reference flood: no scope bias here
    )
}

/// Methods and impl blocks associated with a type target.
fn collect_type_impl_callees(
    graph: &CodeGraph,
    target: &BlockInfo,
    visited: &mut HashSet<Id>,
    limits: &TraceLimits,
    stats: &mut TraceStats,
    is_noisy: &impl Fn(&str) -> bool,
    is_test_block: &impl Fn(&BlockInfo) -> bool,
    compress_tests: bool,
    test_omitted: &mut usize,
) -> Vec<Id> {
    let mut candidate_ids: Vec<Id> = Vec::new();
    if graph.sources_stripped() {
        // Children of the type + same-name peers (no O(nodes) empty-source scan).
        candidate_ids.extend(graph.children(&target.id));
        for b in graph.blocks_for_name(&target.name) {
            if b.id != target.id {
                candidate_ids.extend(graph.children(&b.id));
            }
        }
        // L1.1: same-file methods nested in class/struct line span (Python class body).
        candidate_ids.extend(type_nested_method_ids(graph, target));
    } else {
        for b in graph.nodes.values() {
            if !type_trace_candidate_ok(b, target, is_test_block) {
                continue;
            }
            let k = b.kind.to_lowercase();
            if !k.contains("impl") {
                continue;
            }
            if !crate::server::filters::contains_word_boundary(&b.source, &target.name) {
                continue;
            }
            candidate_ids.push(b.id.clone());
            for child in graph.children(&b.id) {
                candidate_ids.push(child);
            }
        }
        candidate_ids.extend(type_nested_method_ids(graph, target));
    }

    collect_trace_neighbors(
        graph,
        candidate_ids,
        visited,
        limits,
        stats,
        is_noisy,
        is_test_block,
        compress_tests,
        test_omitted,
        Some(target),
        &[],
    )
}

fn collect_trace_neighbors(
    graph: &CodeGraph,
    neighbor_ids: impl IntoIterator<Item = Id>,
    visited: &mut HashSet<Id>,
    limits: &TraceLimits,
    stats: &mut TraceStats,
    is_noisy: &impl Fn(&str) -> bool,
    is_test_block: &impl Fn(&BlockInfo) -> bool,
    compress_tests: bool,
    test_omitted: &mut usize,
    path_anchor: Option<&BlockInfo>,
    scope_prefixes: &[String],
) -> Vec<Id> {
    let mut ids: Vec<Id> = Vec::new();
    for id in neighbor_ids {
        if visited.len() >= limits.max_visited_nodes {
            stats.visited_capped = true;
            break;
        }
        // Peek before insert so filtered structural edges don't burn visited slots.
        if visited.contains(&id) {
            continue;
        }
        let Some(b) = graph.get_block(id.clone()) else {
            continue;
        };
        if is_noisy(&b.name) || crate::server::filters::is_trace_noise_name(&b.name) {
            continue;
        }
        // C decl↔def implements edges are structural, not calls — hide from Trace blast.
        // Predicate lives in code_graph C/C++ semantics (not CLI filters).
        if let Some(anchor) = path_anchor {
            if code_graph::is_c_decl_def_implements_pair(anchor, b) {
                continue;
            }
            // Usage edges still link fn→type when a name appears in a signature.
            // Those are not CALLS — do not present types as callers/callees of a function.
            if !crate::server::filters::is_type_trace_target(&anchor.kind)
                && crate::server::filters::is_type_trace_target(&b.kind)
            {
                continue;
            }
        }
        if compress_tests && is_test_block(b) {
            *test_omitted += 1;
            continue;
        }
        if !visited.insert(id.clone()) {
            continue;
        }
        ids.push(id);
    }

    // Prefer cross-lang (FFI / polyglot bridges) before same-lang path score so
    // py→rs / rs→py edges survive fan-out caps and surface in Trace callees/callers.
    let anchor_lang = path_anchor.map(|a| a.lang.as_str());
    ids.sort_by(|a, b| {
        let cross = |id: &Id| -> i32 {
            match (anchor_lang, graph.get_block(id.clone())) {
                (Some(al), Some(nb)) if !nb.lang.is_empty() && nb.lang != al => 1,
                _ => 0,
            }
        };
        let path_rank = |id: &Id| -> i32 {
            graph
                .get_block(id.clone())
                .map(|b| {
                    crate::server::filters::application_path_priority(&b.file.to_string_lossy())
                })
                .unwrap_or(0)
        };
        // Prefer path-local neighbors (same package tree as seed) over distant generics.
        let local = |id: &Id| -> i32 {
            match (path_anchor, graph.get_block(id.clone())) {
                (Some(anchor), Some(nb)) => {
                    let af = anchor.file.to_string_lossy().replace('\\', "/");
                    let bf = nb.file.to_string_lossy().replace('\\', "/");
                    let ap = af.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
                    if !ap.is_empty() && bf.starts_with(ap) {
                        2
                    } else if af.split('/').take(2).collect::<Vec<_>>()
                        == bf.split('/').take(2).collect::<Vec<_>>()
                    {
                        1
                    } else {
                        0
                    }
                }
                _ => 0,
            }
        };
        let weak = |id: &Id| -> i32 {
            graph
                .get_block(id.clone())
                .map(|b| crate::server::filters::trace_name_weak_penalty(&b.name))
                .unwrap_or(0)
        };
        // Entry / pipeline names (run_*, handle_*, main) — keep product parents
        // inside max_fan_out when hubs would otherwise fill with local helpers (I4).
        let entry = |id: &Id| -> i32 {
            graph
                .get_block(id.clone())
                .map(|b| {
                    let n = b.name.to_ascii_lowercase();
                    if n == "main" || n.starts_with("handle_") || n.starts_with("run_") {
                        3
                    } else if n.starts_with("dispatch_")
                        || n.starts_with("build_")
                        || n.starts_with("execute_")
                    {
                        2
                    } else if n.starts_with("try_") || n.starts_with("do_") {
                        1
                    } else {
                        0
                    }
                })
                .unwrap_or(0)
        };
        // Agent scope_paths: keep in-scope reverse/forward parents inside max_fan_out (hub UX).
        let scoped = |id: &Id| -> i32 {
            if scope_prefixes.is_empty() {
                return 0;
            }
            graph
                .get_block(id.clone())
                .map(|b| {
                    i32::from(crate::server::trace_pack::neighbor_in_scope(
                        &b.file.to_string_lossy(),
                        scope_prefixes,
                    ))
                })
                .unwrap_or(0)
        };
        cross(b)
            .cmp(&cross(a))
            .then(scoped(b).cmp(&scoped(a)))
            .then(entry(b).cmp(&entry(a)))
            .then(local(b).cmp(&local(a)))
            .then(weak(a).cmp(&weak(b))) // lower penalty first
            .then(path_rank(b).cmp(&path_rank(a)))
            .then(
                block_score(graph, b)
                    .partial_cmp(&block_score(graph, a))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    if let Some(anchor) = path_anchor {
        // Keep **all real CALL neighbors** (I4 reverse integrity). Path "peripheral"
        // demotion used to drop product parents (e.g. harness build_flat_plan →
        // gnn node_count) while warehouse_in still counted them — high|complete
        // Trace then missed reverse edges. Type-reference floods already filter
        // peripheral in `type_trace_candidate_ok` before reaching here.
        // Still drop C decl↔def implements pairs (not CALL).
        ids.retain(|id| {
            graph
                .get_block(id.clone())
                .map(|b| !code_graph::is_c_decl_def_implements_pair(b, anchor))
                .unwrap_or(false)
        });
    }
    if ids.len() > limits.max_fan_out {
        stats.fan_out_pruned += ids.len() - limits.max_fan_out;
        ids.truncate(limits.max_fan_out);
    }
    stats.nodes_visited = visited.len();
    ids
}

fn expand_trace_frontier<F>(
    graph: &CodeGraph,
    frontier: &[Id],
    visited: &mut HashSet<Id>,
    limits: &TraceLimits,
    stats: &mut TraceStats,
    is_noisy: &impl Fn(&str) -> bool,
    is_test_block: &impl Fn(&BlockInfo) -> bool,
    compress_tests: bool,
    test_omitted: &mut usize,
    path_anchor: Option<&BlockInfo>,
    scope_prefixes: &[String],
    neighbor_fn: F,
) -> Vec<Id>
where
    F: Fn(&CodeGraph, &Id) -> Vec<Id>,
{
    let mut neighbor_ids: Vec<Id> = Vec::new();
    for node_id in frontier {
        if visited.len() >= limits.max_visited_nodes {
            stats.visited_capped = true;
            break;
        }
        for id in neighbor_fn(graph, node_id) {
            if visited.contains(&id) {
                continue;
            }
            neighbor_ids.push(id);
        }
    }
    collect_trace_neighbors(
        graph,
        neighbor_ids,
        visited,
        limits,
        stats,
        is_noisy,
        is_test_block,
        compress_tests,
        test_omitted,
        path_anchor,
        scope_prefixes,
    )
}


/// Result of CALL-neighborhood expansion (L1 then L2) + char-budget trim.
pub(crate) struct TraceNeighborhood {
    pub callers_by_depth: Vec<Vec<Id>>,
    pub callees_by_depth: Vec<Vec<Id>>,
    /// `(caller_id, peer_def_id)` — CALL into same-name peers, **not** into ★.
    pub peer_callers: Vec<(Id, Id)>,
    pub test_callers_omitted: Vec<usize>,
    pub test_callees_omitted: Vec<usize>,
    pub stats: TraceStats,
    pub blast_depth: usize,
    pub max_fan_out: usize,
    pub max_visited_nodes: usize,
}

/// Expand CALL neighborhood for a resolved Trace seed.
///
/// **Invariant:** L1 callers and L1 callees both run **before** any L2 expansion so a
/// shared `visited` budget cannot starve callees on popular symbols (sdsnewlen-class).
pub(crate) fn expand_trace_neighborhood(
    graph: &CodeGraph,
    target: &BlockInfo,
    root_path: &Path,
    noise_cfg: &crate::server::filters::NoiseFilterConfig,
    compress_tests: bool,
    req_depth: usize,
    max_call_graph_depth: usize,
    max_fan_out: usize,
    max_visited_nodes: usize,
    scope_prefixes: &[String],
) -> TraceNeighborhood {
    let type_target = crate::server::filters::is_type_trace_target(&target.kind);
    let is_noisy = |name: &str| -> bool {
        crate::server::filters::is_trace_noise_name(name)
    };
    let is_test_block = |b: &BlockInfo| -> bool {
        crate::server::filters::is_noise(b, root_path, noise_cfg)
            || b.source.contains("#[test]")
    };

    let max_depth = 2usize;
    let blast_depth = req_depth.min(max_call_graph_depth).min(max_depth);

    let mut test_callers_omitted: Vec<usize> = vec![0; blast_depth];
    let mut test_callees_omitted: Vec<usize> = vec![0; blast_depth];
    let mut callers_by_depth: Vec<Vec<Id>> = vec![vec![]; blast_depth];
    let mut callees_by_depth: Vec<Vec<Id>> = vec![vec![]; blast_depth];
    let mut visited: HashSet<Id> = HashSet::new();
    visited.insert(target.id.clone());

    let limits = TraceLimits {
        max_fan_out,
        max_visited_nodes,
    };
    let mut trace_stats = TraceStats {
        fan_out_pruned: 0,
        visited_capped: false,
        nodes_visited: 1,
    };

    let path_anchor = Some(target);
    // L1 both directions FIRST. Shared `visited` + max_visited used to fill on
    // L2 caller fan-in for popular symbols (sdsnewlen: 200 cap) so L1 callees
    // never ran → empty callees despite real CALL edges (_sdsnewlen).
    // L1 **CALL** into ★ only (P.4). Same-name peer reverse is separate
    // (`peer_callers`) so agents do not treat peer parents as hard edges into the pin.
    let mut l1_callers = collect_trace_neighbors(
        graph,
        graph.callers(&target.id),
        &mut visited,
        &limits,
        &mut trace_stats,
        &is_noisy,
        &is_test_block,
        compress_tests,
        &mut test_callers_omitted[0],
        path_anchor,
        scope_prefixes,
    );
    // Twin-id recovery: callers of other same-name defs (labeled, not merged into CALL).
    let peer_callers = graph.name_peer_callers(target);
    if l1_callers.is_empty() && type_target {
        l1_callers = collect_type_reference_ids(
            graph,
            target,
            &mut visited,
            &limits,
            &mut trace_stats,
            &is_noisy,
            &is_test_block,
            compress_tests,
            &mut test_callers_omitted[0],
        );
    }
    if !l1_callers.is_empty() {
        callers_by_depth[0] = l1_callers;
    }

    let mut l1_callees = collect_trace_neighbors(
        graph,
        graph.children(&target.id),
        &mut visited,
        &limits,
        &mut trace_stats,
        &is_noisy,
        &is_test_block,
        compress_tests,
        &mut test_callees_omitted[0],
        path_anchor,
        scope_prefixes,
    );
    if l1_callees.is_empty() && type_target {
        l1_callees = collect_type_impl_callees(
            graph,
            target,
            &mut visited,
            &limits,
            &mut trace_stats,
            &is_noisy,
            &is_test_block,
            compress_tests,
            &mut test_callees_omitted[0],
        );
    }
    if !l1_callees.is_empty() {
        callees_by_depth[0] = l1_callees;
    }

    // L2 only after both L1 sides are filled (remaining visited budget).
    if blast_depth > 1 && !callers_by_depth[0].is_empty() {
        let l2 = expand_trace_frontier(
            graph,
            &callers_by_depth[0],
            &mut visited,
            &limits,
            &mut trace_stats,
            &is_noisy,
            &is_test_block,
            compress_tests,
            &mut test_callers_omitted[1],
            path_anchor,
            scope_prefixes,
            |g, id| g.callers(id),
        );
        if !l2.is_empty() {
            callers_by_depth[1] = l2;
        }
    }

    if blast_depth > 1 && !callees_by_depth[0].is_empty() {
        let l2 = expand_trace_frontier(
            graph,
            &callees_by_depth[0],
            &mut visited,
            &limits,
            &mut trace_stats,
            &is_noisy,
            &is_test_block,
            compress_tests,
            &mut test_callees_omitted[1],
            path_anchor,
            scope_prefixes,
            |g, id| g.children(id),
        );
        if !l2.is_empty() {
            callees_by_depth[1] = l2;
        }
    }

    // Pre-pack size is already bounded by max_fan_out. Char/token budgeting is
    // owned by `trace_pack` (honest callers_omitted vs warehouse_in). Do **not**
    // silently pop L1 callers here — that hid reverse parents (I4) while
    // seed_in_degree still reported full warehouse fan-in.

    TraceNeighborhood {
        callers_by_depth,
        callees_by_depth,
        peer_callers,
        test_callers_omitted,
        test_callees_omitted,
        stats: trace_stats,
        blast_depth,
        max_fan_out,
        max_visited_nodes,
    }
}
