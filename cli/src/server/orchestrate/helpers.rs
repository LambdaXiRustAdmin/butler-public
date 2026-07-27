//! Shared Trace/Arch helpers (Thesis O1 peel).
//! Scope framing, hop split, bridge list, goal normalize — zero intentional behavior change.

use crate::server::dto::*;
use code_graph::{BlockInfo, CodeGraph};
use std::path::Path;

use super::trace_pack_helpers::short_path;

/// while Arch still listed the same edge via `find_bridges`.
pub(crate) fn collect_seed_bridge_neighbors(
    graph: &CodeGraph,
    target: &BlockInfo,
    pp: &code_graph::ProjectPaths,
    is_noisy_name: &dyn Fn(&str) -> bool,
) -> (Vec<CallerCallee>, Vec<CallerCallee>) {
    let mut bridge_callers = Vec::new();
    for (id, kind) in graph.bridge_callers(&target.id) {
        let Some(b) = graph.get_block(id) else {
            continue;
        };
        if is_noisy_name(&b.name) {
            continue;
        }
        let mut cc = crate::server::filters::caller_callee_from_block_at_hop(b, pp, 1);
        cc.relation = Some(kind.as_relation_label().to_string());
        bridge_callers.push(cc);
    }
    let mut bridge_callees = Vec::new();
    for (id, kind) in graph.bridge_children(&target.id) {
        let Some(b) = graph.get_block(id) else {
            continue;
        };
        if is_noisy_name(&b.name) {
            continue;
        }
        let mut cc = crate::server::filters::caller_callee_from_block_at_hop(b, pp, 1);
        cc.relation = Some(kind.as_relation_label().to_string());
        bridge_callees.push(cc);
    }
    (bridge_callers, bridge_callees)
}

pub(crate) fn lang_cluster_of(b: &BlockInfo) -> (String, String) {
    let c = code_graph::cluster_for_block(b);
    (
        code_graph::normalize_lang_label(&b.lang),
        c.badge().to_string(),
    )
}

pub(crate) fn cluster_infos_from_scoped(scoped: &[&BlockInfo]) -> Vec<ClusterInfo> {
    code_graph::summarize_clusters(scoped.iter().copied())
        .into_iter()
        .map(|s| ClusterInfo {
            id: s.id.as_str().to_string(),
            label: s.id.label().to_string(),
            badge: s.id.badge().to_string(),
            nodes: s.nodes,
            files: s.files,
            entries: s.entries,
        })
        .collect()
}

pub(crate) fn bridge_infos(graph: &CodeGraph, scoped: &[&BlockInfo], root: &Path, max: usize) -> Vec<BridgeInfo> {
    let pp = code_graph::ProjectPaths::new(root);
    code_graph::find_bridges(graph, scoped, max)
        .into_iter()
        .map(|b| BridgeInfo {
            from_name: b.from_name,
            from_file: pp.to_display(Path::new(&b.from_file)),
            from_lang: b.from_lang,
            from_cluster: b.from_cluster.badge().to_string(),
            to_name: b.to_name,
            to_file: pp.to_display(Path::new(&b.to_file)),
            to_lang: b.to_lang,
            to_cluster: b.to_cluster.badge().to_string(),
        })
        .collect()
}

pub(crate) fn format_loc_lang(
    name: &str,
    file: &str,
    line: usize,
    lang: Option<&str>,
    cluster: Option<&str>,
    relation: Option<&str>,
) -> String {
    format_loc_lang_hop(name, file, line, lang, cluster, relation, 1)
}

pub(crate) fn format_loc_lang_hop(
    name: &str,
    file: &str,
    line: usize,
    lang: Option<&str>,
    cluster: Option<&str>,
    relation: Option<&str>,
    hop: u8,
) -> String {
    let mut s = format!("{} @ {}:{}", name, short_path(file), line);
    match (lang, cluster) {
        (Some(l), Some(c)) => s = format!("{name} · {l} · {c} @ {}:{}", short_path(file), line),
        (Some(l), None) => s = format!("{name} · {l} @ {}:{}", short_path(file), line),
        (None, Some(c)) => s = format!("{name} · {c} @ {}:{}", short_path(file), line),
        _ => {}
    }
    if let Some(r) = relation.filter(|r| !r.is_empty()) {
        s = format!("{s} · {r}");
    }
    // hop=1 is the default direct edge — only label transitive blast hops.
    if hop > 1 {
        s = format!("{s} · hop={hop}");
    }
    s
}

/// Count direct (hop≤1) vs transitive (hop≥2) neighbors for headlines / telemetry.
pub(crate) fn hop_split(items: &[CallerCallee]) -> (usize, usize) {
    let mut direct = 0usize;
    let mut transitive = 0usize;
    for c in items {
        if c.hop <= 1 {
            direct += 1;
        } else {
            transitive += 1;
        }
    }
    (direct, transitive)
}

/// Honest edge census from Trace telemetry (pre-pack totals, not just shown sample).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct EdgeCensus {
    pub callers_total: usize,
    pub callees_total: usize,
    pub callers_shown: usize,
    pub callees_shown: usize,
    pub callers_direct: usize,
    pub callers_transitive: usize,
    pub callees_direct: usize,
    pub callees_transitive: usize,
    pub fan_out_pruned: usize,
    pub visited_capped: bool,
    pub bridges_in: usize,
    pub bridges_out: usize,
}

pub(crate) fn edge_census_from_report(st: &StructuredReport) -> EdgeCensus {
    let t = &st.telemetry;
    let u = |k: &str| t.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let b = |k: &str| t.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut c = EdgeCensus {
        callers_total: u("callers_total"),
        callees_total: u("callees_total"),
        callers_shown: u("callers_shown"),
        callees_shown: u("callees_shown"),
        callers_direct: u("callers_direct"),
        callers_transitive: u("callers_transitive"),
        callees_direct: u("callees_direct"),
        callees_transitive: u("callees_transitive"),
        fan_out_pruned: u("fan_out_pruned"),
        visited_capped: b("visited_capped"),
        bridges_in: u("bridge_callers"),
        bridges_out: u("bridge_callees"),
    };
    // Fallback when telemetry missing (tests / error paths).
    if c.callers_total == 0 && !st.callers.is_empty() {
        c.callers_total = st.callers.len();
        c.callers_shown = st.callers.len();
        let (d, tr) = hop_split(&st.callers);
        c.callers_direct = d;
        c.callers_transitive = tr;
    }
    if c.callees_total == 0 && !st.callees.is_empty() {
        c.callees_total = st.callees.len();
        c.callees_shown = st.callees.len();
        let (d, tr) = hop_split(&st.callees);
        c.callees_direct = d;
        c.callees_transitive = tr;
    }
    if c.callers_shown == 0 {
        c.callers_shown = st.callers.len();
    }
    if c.callees_shown == 0 {
        c.callees_shown = st.callees.len();
    }
    if c.bridges_in == 0 {
        c.bridges_in = st.bridge_callers.len();
    }
    if c.bridges_out == 0 {
        c.bridges_out = st.bridge_callees.len();
    }
    c
}

/// Compact one-liner for one CALL side: sample size + comprehensive count.
pub(crate) fn call_side_bit(shown: usize, total: usize, direct: usize, transitive: usize) -> String {
    let total = total.max(shown);
    let hop_sum = direct + transitive;
    let (direct, transitive) = if hop_sum == 0 {
        (total, 0)
    } else {
        (direct, transitive)
    };
    if total > shown {
        if transitive > 0 {
            format!("{shown}/{total} CALL ({direct}d+{transitive}h)")
        } else {
            format!("{shown}/{total} CALL")
        }
    } else if transitive > 0 {
        format!("{direct} direct+{transitive} hop≥2 CALL")
    } else {
        format!("{total} CALL")
    }
}

/// Magnitude band for human scope framing (not a substitute for exact counts).
pub(crate) fn fan_band(n: usize) -> &'static str {
    match n {
        0 => "none",
        1..=2 => "narrow",
        3..=15 => "moderate",
        16..=50 => "wide",
        51..=200 => "hub-scale",
        _ => "critical-hub",
    }
}

/// Agent-facing scope frame: “called by N · calls M · wide fan-in” with honesty tags.
///
/// Uses **warehouse direct degrees** (true reverse/forward adjacency into ★) —
/// not the ranked sample size, and not same-name peer reverse.
pub(crate) fn scope_frame_line(
    seed_in_degree: usize,
    seed_out_degree: usize,
    fan_out_pruned: usize,
    visited_capped: bool,
    edges_complete: bool,
    bridges: usize,
) -> String {
    scope_frame_line_with_peers(
        seed_in_degree,
        seed_out_degree,
        0,
        fan_out_pruned,
        visited_capped,
        edges_complete,
        bridges,
    )
}

pub(crate) fn scope_frame_line_with_peers(
    seed_in_degree: usize,
    seed_out_degree: usize,
    peer_callers: usize,
    fan_out_pruned: usize,
    visited_capped: bool,
    edges_complete: bool,
    bridges: usize,
) -> String {
    let in_band = fan_band(seed_in_degree);
    let out_band = fan_band(seed_out_degree);
    let mut parts: Vec<String> = Vec::new();

    // Primary human frame — direct CALL into ★ only
    if seed_in_degree == 0 && seed_out_degree == 0 {
        parts.push("scope: isolated in CALL graph (0 callers, 0 callees)".into());
    } else {
        let mut core = format!(
            "scope: called by {seed_in_degree} ({in_band} fan-in) · calls {seed_out_degree} ({out_band} fan-out)"
        );
        // “Touches everything” only when both sides are hub-scale — rare, honest.
        if seed_in_degree >= 200 || seed_out_degree >= 200 {
            core.push_str(" — large blast surface");
        } else if seed_in_degree >= 50 || seed_out_degree >= 50 {
            core.push_str(" — treat as shared infrastructure");
        }
        parts.push(core);
    }

    if peer_callers > 0 {
        parts.push(format!(
            "+{peer_callers} same-name peer caller(s) (not CALL into ★)"
        ));
    }

    if bridges > 0 {
        parts.push(format!("{bridges} interconnect bridge(s)"));
    }

    let mut tags: Vec<&str> = Vec::new();
    if !edges_complete {
        tags.push("edges partial — counts may grow");
    }
    if fan_out_pruned > 0 || visited_capped {
        // Degrees are full warehouse; Trace *lists* are capped.
        tags.push("lists capped (totals/degrees above are complete for this seed)");
    }
    if !tags.is_empty() {
        parts.push(format!("[{}]", tags.join("; ")));
    }

    parts.join(" ")
}

/// Repo-relative scope prefixes only — never host abs / `home/…` / mount leaks.
///
/// `suggested_scopes` is for `scope_paths` (e.g. `include/`, `src/core/`), not a dump
/// of the first two segments of `/home/<user>/projects/...`.
///
/// Already-relative paths are kept as-is (do **not** run `to_rel` on them — project-name
/// strip can eat `include/pybind11/` → `attr.h` when the root is named `pybind11`).
// extracted to submodule; was lines 759-839]



pub(crate) fn hub_cap_for_summary(max_blocks: usize, hub_budget_pct: f64) -> usize {
    let pct = hub_budget_pct.clamp(0.05, 1.0);
    let cap = ((max_blocks as f64) * pct).round() as usize;
    cap.clamp(1, max_blocks)
}

/// Canonical orchestrate goal string for the Trace/Find/Arch match arms.
/// Uses [`crate::server::mode_intent`] so synonyms like `architect` / `trace` agree
/// with context_engine routing (Pack A).
pub(crate) fn normalize_goal(raw_goal: &str) -> String {
    use crate::server::mode_intent::{resolve_mode_intent, ModeIntent};
    match resolve_mode_intent(raw_goal) {
        ModeIntent::TraceBlastRadius => "TraceBlastRadius".to_string(),
        ModeIntent::FindImplementation => "FindImplementation".to_string(),
        ModeIntent::ArchitecturalSummary | ModeIntent::Architecture => {
            "ArchitecturalSummary".to_string()
        }
        // Non-orchestrate modes: leave raw (handle_orchestrate will miss/error as before).
        _ => raw_goal.trim().to_string(),
    }
}



