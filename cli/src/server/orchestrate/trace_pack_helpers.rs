//! Trace packaging helpers (P1b peel from orchestrate/mod.rs).
//!
//! Cap/pack neighbors, loc-fallback unique-def gate, symbol locations, empty-callers
//! honesty lines. Zero intentional behavior change — move only.
//!
//! Re-exported from `mod.rs` so `render` / `arch` / `detail_tests` keep `super::…` paths.

use crate::server::dto::*;
use crate::server::paths::format_project_path;
use code_graph::{BlockInfo, CodeGraph};
use std::collections::HashSet;
use std::path::Path;

use super::lang_cluster_of;

pub(super) fn report_incomplete(st: &StructuredReport) -> bool {
    if let Some(c) = st.state.confidence.as_deref() {
        return c != "edges_full";
    }
    if let Some(p) = st.state.percent {
        return p < 100;
    }
    !st.state.edge_build.contains("Complete")
}

/// Hub-scale reverse fan-in (matches [`fan_band`] "hub-scale" floor).
pub(super) const HUB_FANIN_NEXT: usize = 51;

/// Dossier pack under char budget (see [`crate::server::trace_pack`]).
/// `long` → larger sample (`detail=long|dense`); short is default orient mode.
/// `focus_names` → Soft I4 hop continuity (force real CALL parents into sample).
/// Window: `sample_offset` / `sample_mode` / `exclude_names`.
pub(super) fn cap_trace_payload_focus(
    callers: Vec<CallerCallee>,
    callees: Vec<CallerCallee>,
    graph: &CodeGraph,
    max_blocks: usize,
    scope_prefixes: &[String],
    long: bool,
    focus_names: &[String],
    sample_offset: Option<u32>,
    sample_mode: Option<&str>,
    exclude_names: &[String],
) -> (
    crate::server::trace_pack::TracePack,
    Vec<String>,
    crate::server::trace_pack::SampleWindowMeta,
) {
    let cfg = crate::server::trace_pack::window_from_req(long, max_blocks, sample_offset, sample_mode);
    crate::server::trace_pack::pack_trace_neighbors_focus(
        callers,
        callees,
        graph,
        cfg,
        scope_prefixes,
        focus_names,
        exclude_names,
    )
}

pub(super) fn next_action_mega_hub(seed_in: usize, blank_scope: bool, already_long: bool) -> String {
    if blank_scope {
        format!(
            "mega-hub ({seed_in} CALL callers) — sample only; pin scope_paths, sample_offset for next window, sample_mode=diverse, or focus_symbol when hopping"
        )
    } else if already_long {
        format!(
            "mega-hub ({seed_in} CALL callers warehouse-wide) — long sample under scope; sample_offset / exclude_symbols / narrow scope / focus_symbol"
        )
    } else {
        format!(
            "mega-hub ({seed_in} CALL callers warehouse-wide) — short sample; detail=long, sample_offset, sample_mode=diverse, or focus_symbol"
        )
    }
}

pub(super) fn collect_unique_hubs<'a>(
    ranked: impl IntoIterator<Item = &'a BlockInfo>,
    graph: &CodeGraph,
    pp: &code_graph::ProjectPaths,
    max_hubs: usize,
) -> Vec<Hub> {
    // Demoscene: pre-alloc, borrow for seen (no name.clone() on dedup checks), tiny max_hubs.
    let mut hubs = Vec::with_capacity(max_hubs);
    let mut seen_names: HashSet<&str> = HashSet::new();
    for h in ranked {
        if !seen_names.insert(h.name.as_str()) {
            continue;
        }
        let (lang, cluster) = lang_cluster_of(h);
        hubs.push(Hub {
            name: h.name.clone(),
            file: format_project_path(pp.root(), &h.file),
            score: graph
                .nodes
                .get(&h.id)
                .map(|real_block| real_block.score)
                .unwrap_or(0.0),
            lang: Some(lang),
            cluster: Some(cluster),
        });
        if hubs.len() >= max_hubs {
            break;
        }
    }
    hubs
}

/// True when `target.name` has exactly one function-like def (one file) in the warehouse.
///
/// Loc-fallback may invent reverse parents only in that case. Multi-file / multi-def
/// same-name graphs must stay silent on empty reverse (use `peer_callers` for twins).
pub(super) fn loc_fallback_unique_fn_def(graph: &CodeGraph, target: &BlockInfo) -> bool {
    let mut files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut n_fn = 0usize;
    for b in graph.blocks_for_name(&target.name) {
        let k = b.kind.to_ascii_lowercase();
        // Function-like only — types/structs are not CALL reverse recovery targets.
        let fn_like = k.contains("function")
            || k.contains("method")
            || k.contains("async_function")
            || (k.contains("constructor") && !k.contains("destructor"));
        if !fn_like {
            continue;
        }
        n_fn += 1;
        files.insert(b.file.to_string_lossy().replace('\\', "/"));
        if n_fn > 1 || files.len() > 1 {
            return false;
        }
    }
    // Unique function-like def, and ★ is that def (or same id family).
    n_fn == 1
}

/// Tightest function/method/class def whose span contains `line` in `file`.
///
/// **Must stay O(nodes_in_file), never O(warehouse).** Full `nodes.values()` walks on vite
/// (~60k) × N call locations paid 0.6–2s in loc_fallback alone.
pub(super) fn enclosing_callable<'a>(
    graph: &'a CodeGraph,
    file: &str,
    line: usize,
    root: &Path,
) -> Option<&'a BlockInfo> {
    let pp = code_graph::ProjectPaths::new(root);
    let mut best: Option<&BlockInfo> = None;
    let mut best_span = usize::MAX;

    let consider = |b: &'a BlockInfo, best: &mut Option<&'a BlockInfo>, best_span: &mut usize| {
        let k = b.kind.to_ascii_lowercase();
        if k.contains("call") {
            return;
        }
        if !(k.contains("function")
            || k.contains("method")
            || k.contains("class")
            || k.contains("struct")
            || k.contains("impl"))
        {
            return;
        }
        if b.start_line <= line && line <= b.end_line.max(b.start_line) {
            let span = b.end_line.saturating_sub(b.start_line);
            if span < *best_span {
                *best_span = span;
                *best = Some(b);
            }
        }
    };

    // Preferred path: file_node_index → only blocks in this file.
    if graph.file_node_index_is_warm() {
        let mut ids: Option<&Vec<code_graph::Id>> = None;
        // Common key forms (warehouse abs, root-relative, host/container mount swap).
        let norm = code_graph::snooper::normalize_path(file);
        let rel = pp.key(file);
        let abs = {
            let p = Path::new(file);
            if p.is_absolute() {
                norm.clone()
            } else {
                code_graph::snooper::normalize_path(&root.join(p).to_string_lossy())
            }
        };
        for key in [norm.as_str(), rel.as_str(), abs.as_str()] {
            if let Some(v) = graph.file_node_index.get(key) {
                ids = Some(v);
                break;
            }
        }
        // Path dialect miss: O(files) same_file match, not O(nodes).
        if ids.is_none() {
            for (path, v) in &graph.file_node_index {
                if pp.same_file(path.as_str(), file) {
                    ids = Some(v);
                    break;
                }
            }
        }
        if let Some(ids) = ids {
            for id in ids {
                if let Some(b) = graph.nodes.get(id) {
                    consider(b, &mut best, &mut best_span);
                }
            }
            return best;
        }
        // Index warm but file unknown — no enclosing def in warehouse.
        return None;
    }

    // Cold index: only small warehouses may linear-scan.
    if graph.nodes.len() > 25_000 {
        return None;
    }
    for b in graph.nodes.values() {
        if !pp.same_file(&b.file, file) {
            continue;
        }
        consider(b, &mut best, &mut best_span);
    }
    best
}

/// rg-shaped locations for exact `symbol`, scoped; preferred row matches Find target.
pub(super) fn build_symbol_locations(
    graph: &CodeGraph,
    preferred: &BlockInfo,
    scoped: &[&BlockInfo],
    symbol: &str,
    _root: &Path,
    max: usize,
) -> Vec<SymbolLocation> {
    if symbol.is_empty() {
        return vec![];
    }
    // Prefer secondary index when warm; fall back to scoped linear scan.
    let from_index: Vec<&code_graph::NameLocation> = graph
        .locations_for_name(symbol)
        .iter()
        .filter(|loc| {
            scoped.is_empty()
                || scoped.iter().any(|b| {
                    b.id == loc.id
                        || (b.name == loc.name
                            && b.file == loc.file
                            && b.start_line == loc.start_line)
                })
        })
        .collect();

    // Index enforcer: never mountain-walk when name_index is warm.
    let mut blocks: Vec<&BlockInfo> = if !from_index.is_empty() {
        from_index
            .iter()
            .filter_map(|loc| graph.nodes.get(&loc.id))
            .collect()
    } else if graph.name_index.is_empty() {
        // Cold index only.
        scoped
            .iter()
            .copied()
            .filter(|b| b.name == symbol)
            .collect()
    } else {
        vec![]
    };

    // Identity pass: one row per node id (kills double-★ path twins).
    {
        let mut seen = HashSet::new();
        blocks.retain(|b| seen.insert(b.id.clone()));
    }

    // Preferred target first, then production paths (not benchmarks/tests), entry
    // landmarks, score, path:line. ProjectPaths anchors path forms to repo-relative equality.
    let pp = code_graph::ProjectPaths::new(_root);
    let is_pref = |b: &BlockInfo| -> bool {
        b.id == preferred.id
            || (b.name == preferred.name
                && b.start_line == preferred.start_line
                && pp.same_file(&b.file, &preferred.file))
    };
    blocks.sort_by(|a, b| {
        let pa = is_pref(a) as i32;
        let pb = is_pref(b) as i32;
        pb.cmp(&pa)
            .then_with(|| {
                // A′.10: production spines before benchmarks/tests/benches.
                let ta = crate::server::filters::is_testish_seed_block(a) as i32;
                let tb = crate::server::filters::is_testish_seed_block(b) as i32;
                ta.cmp(&tb)
            })
            .then_with(|| {
                let ra = crate::server::filters::application_path_priority(
                    &a.file.to_string_lossy(),
                );
                let rb = crate::server::filters::application_path_priority(
                    &b.file.to_string_lossy(),
                );
                rb.cmp(&ra)
            })
            .then_with(|| {
                let ea = crate::server::filters::is_entry_landmark(a, _root) as i32;
                let eb = crate::server::filters::is_entry_landmark(b, _root) as i32;
                eb.cmp(&ea)
            })
            .then(
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.file.cmp(&b.file))
            .then(a.start_line.cmp(&b.start_line))
    });
    blocks.truncate(max);

    blocks
        .into_iter()
        .map(|b| {
            let (lang, cluster) = lang_cluster_of(b);
            SymbolLocation {
                name: b.name.clone(),
                // Display via root anchor (host mount rewrite when configured).
                file: pp.to_display(&b.file),
                line: b.start_line,
                end_line: if b.end_line > b.start_line {
                    Some(b.end_line)
                } else {
                    None
                },
                kind: b.kind.clone(),
                preferred: is_pref(b),
                lang: Some(lang),
                cluster: Some(cluster),
            }
        })
        .collect()
}
/// Short path for agent-readable lines (prefer last 3 components).
pub(super) fn short_path(p: &str) -> String {
    let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 {
        p.to_string()
    } else {
        parts[parts.len() - 3..].join("/")
    }
}

pub(super) fn truncate_def(s: &str, max_chars: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max_chars {
        return t.to_string();
    }
    let mut out: String = t.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

/// Empty-callers line: never imply dead code when warehouse CALL fan-in is 0.
///
/// Incomplete warehouse, C/C++ public APIs, dual-stack bridges, and general
/// 0-CALL cases (callbacks, framework entry, external clients) all get honesty tags.
pub(super) fn empty_callers_line(t: &TargetInfo, st: &StructuredReport) -> String {
    if report_incomplete(st) {
        let pct = st.state.percent.unwrap_or(0).min(99);
        let conf = st
            .state
            .confidence
            .as_deref()
            .unwrap_or("inventory");
        return format!(
            "callers: (0 so far — graph {pct}% · confidence:{conf}; do not treat as dead code; rewalk)"
        );
    }
    let warehouse_in = st
        .telemetry
        .get("seed_in_degree")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let has_bridges = !st.bridge_callers.is_empty() || !st.bridge_callees.is_empty();
    let has_peers = !st.peer_callers.is_empty();

    // Dual-stack: 0 CALL but live path may be export/ipc/twin.
    if warehouse_in == 0 && has_bridges {
        return "callers: (none CALL — see bridges; dual-stack/export may own the live path; not dead code)"
            .into();
    }
    // Twin-id recovery: 0 CALL into ★ but same-name peers have callers.
    if warehouse_in == 0 && has_peers {
        return "callers: (none CALL into ★ — see peer_callers; same-name peers only; not dead code)"
            .into();
    }

    if looks_c_public_api_target(t, st) {
        // Header prototype locations (non-preferred) — point agents at the export surface.
        let header_bits: Vec<String> = st
            .locations
            .as_ref()
            .into_iter()
            .flatten()
            .filter(|loc| {
                !loc.preferred
                    && (loc.kind.contains("function_declaration")
                        || is_c_header_path(&loc.file))
            })
            .take(3)
            .map(|loc| format!("{}:{}", short_path(&loc.file), loc.line))
            .collect();
        if header_bits.is_empty() {
            return "callers: (none CALL — likely public API / export; try demos, clients, or related hubs; not dead code)"
                .into();
        }
        return format!(
            "callers: (none CALL — likely public API / export; header: {}; not dead code)",
            header_bits.join(", ")
        );
    }

    // General 0-CALL honesty (callback chasm, framework entry, external callers).
    if warehouse_in == 0 {
        return "callers: (none CALL — not proof of dead code; may be callback/reference, framework entry, or external)"
            .into();
    }
    // Sample empty but warehouse has fan-in (pack omit / scope).
    "callers: (none in sample — warehouse has callers; widen scope or reverse from a known site)"
        .into()
}

fn is_c_header_path(file: &str) -> bool {
    let f = file.replace('\\', "/").to_ascii_lowercase();
    f.ends_with(".h")
        || f.ends_with(".hpp")
        || f.ends_with(".hh")
        || f.ends_with(".hxx")
        || f.contains("/include/")
}

fn looks_c_public_api_target(t: &TargetInfo, st: &StructuredReport) -> bool {
    // Cheap path/lang probe (no BlockInfo here) — mirrors code_graph C family rules.
    let file = t.file.replace('\\', "/").to_ascii_lowercase();
    let lang = t.lang.as_deref().unwrap_or("").to_ascii_lowercase();
    let cluster = t
        .cluster
        .as_deref()
        .or(st.active_cluster.as_deref())
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_c = matches!(lang.as_str(), "c" | "cpp" | "c++" | "cxx")
        || cluster.contains("c_cpp")
        || cluster.contains("core:c")
        || file.ends_with(".c")
        || file.ends_with(".h")
        || file.ends_with(".cpp")
        || file.ends_with(".hpp")
        || file.ends_with(".cc")
        || file.ends_with(".cxx")
        || file.ends_with(".hh")
        || file.ends_with(".hxx")
        || file.contains("/include/");
    if !is_c {
        return false;
    }
    if let Some(def) = t.definition.as_ref() {
        let first = def.lines().next().unwrap_or("").trim_start();
        if first.starts_with("static ") || first.starts_with("static\t") {
            return false;
        }
    }
    if let Some(locs) = st.locations.as_ref() {
        let any_fn = locs.iter().any(|l| {
            l.name == t.name
                && (l.kind.contains("function_definition")
                    || l.kind.contains("function_declaration"))
        });
        if any_fn {
            return true;
        }
    }
    true
}
