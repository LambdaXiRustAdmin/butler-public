//! Compact trust dossier + dense content rendering.
use super::disambiguate::serious_alt_file_count;
use super::receipt::{
    compute_trace_receipt, next_action_disambiguate, receipt_compact_bit,
};
use super::{
    call_side_bit, edge_census_from_report, empty_callers_line, format_loc_lang,
    format_loc_lang_hop, hop_split, report_incomplete, scope_frame_line, short_path,
    truncate_def, EdgeCensus,
};
use crate::server::build_status;
use crate::server::dto::*;

/// Content + pack length mode.
///
/// **Short** (default): compact trust dossier + tight neighbor sample.  
/// **Long**: dense dump + larger neighbor sample. Same honesty (degrees/omitted) both ways.
///
/// Aliases: short/compact/simple → Short; long/dense/full/verbose/rich → Long.
///
/// TODO(optional): `unlimited` / uncapped sample only if a concrete agent use case
/// needs rows beyond long under a tight pin (forensics/export). Not mission-default —
/// prefer raise long ceiling or re-pin over inventing unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentDetail {
    /// short / compact — orient, pin, bridges, next
    Compact,
    /// long / dense — work the neighborhood under a pin
    Dense,
}

impl ContentDetail {
    pub fn from_req(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("dense")
            | Some("long")
            | Some("full")
            | Some("verbose")
            | Some("rich") => Self::Dense,
            // short / compact / simple / default
            _ => Self::Compact,
        }
    }

    /// True when agent asked for a larger sample (`detail=long|dense|…`).
    pub fn is_long(self) -> bool {
        matches!(self, Self::Dense)
    }

    /// Canonical product name for next: / docs.
    pub fn as_length_label(self) -> &'static str {
        match self {
            Self::Compact => "short",
            Self::Dense => "long",
        }
    }
}

/// Short repo-relative path for compact headlines (accurate seed visibility).
pub(super) fn short_display_path(file: &str) -> String {
    let f = file.replace('\\', "/");
    // Prefer last 3 segments so torch/nn/modules/module.py stays readable.
    let parts: Vec<&str> = f.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 {
        parts.join("/")
    } else {
        parts[parts.len() - 3..].join("/")
    }
}

/// User-visible first line: pain language + DIRECT N. Hop-2 is not a caller.
fn pain_direct_lead(name: &str, cen: &EdgeCensus) -> String {
    let callers = if cen.callers_direct + cen.callers_transitive == 0 {
        cen.callers_total
    } else {
        cen.callers_direct
    };
    let callees = if cen.callees_direct + cen.callees_transitive == 0 {
        cen.callees_total
    } else {
        cen.callees_direct
    };
    format!(
        "Before you edit {name}: here are the direct callers / callees. {} total. Hop-2 is not a caller.\n",
        callers + callees
    )
}

/// Headline one-liner (always used for Compact; also first line of Dense).
/// Includes preferred seed path + alt location count so agents can re-pin mega-homonyms.
pub(super) fn compact_headline(st: &StructuredReport) -> String {
    // T.2: alts-first — not a hard error; lead with disambiguate + locations count
    if st.blast_domain.as_deref() == Some("disambiguate")
        || st
            .telemetry
            .get("disambiguate")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        let n = st
            .telemetry
            .get("serious_alt_files")
            .and_then(|v| v.as_u64())
            .unwrap_or_else(|| {
                st.locations
                    .as_ref()
                    .map(|l| serious_alt_file_count(l) as u64)
                    .unwrap_or(0)
            });
        let name = st
            .target
            .as_ref()
            .map(|t| t.name.as_str())
            .or_else(|| {
                st.error
                    .as_ref()
                    .and_then(|e| e.split('\'').nth(1))
            })
            .unwrap_or("?");
        let receipt_bit = receipt_compact_bit(st);
        let next = st
            .next_action
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or("call who_calls again with scope_paths set to exactly ONE path from locations, then re-Trace");
        return format!(
            "Disambiguate '{name}': {n} serious production locations — pin scope_paths (see locations) before Trace neighborhood.{receipt_bit}\nnext: {next}"
        );
    }
    if let Some(err) = st.error.as_ref().filter(|e| !e.is_empty()) {
        if let Some(next) = st.next_action.as_ref().filter(|n| !n.is_empty()) {
            // Prefer explicit next_action over prose already embedding "next:"
            if err.contains("next:") {
                return format!("Orchestrate error: {err}");
            }
            return format!("Orchestrate error: {err}\nnext: {next}");
        }
        return format!(
            "Orchestrate error: {err}\nnext: call who_calls with a recognized goal/symbol or mode=arch to orient"
        );
    }
    if let Some(t) = st.target.as_ref() {
        let blocks = 1 + st.callers.len() + st.callees.len();
        // path:line always — never `file.rs: 9 direct…` (LLM parses that as line 9).
        let seed = format!("{}:{}", short_display_path(&t.file), t.line);
        let alt_n = st
            .locations
            .as_ref()
            .map(|locs| locs.iter().filter(|l| !l.preferred).count())
            .unwrap_or(0);
        let alt_bit = if alt_n > 0 {
            format!("; {alt_n} alt locations")
        } else {
            String::new()
        };
        let cen = edge_census_from_report(st);
        let callers_bit = format!(
            "{} callers",
            call_side_bit(
                cen.callers_shown,
                cen.callers_total,
                cen.callers_direct,
                cen.callers_transitive,
            )
        );
        let callees_bit = format!(
            "{} callees",
            call_side_bit(
                cen.callees_shown,
                cen.callees_total,
                cen.callees_direct,
                cen.callees_transitive,
            )
        );
        let br_n = cen.bridges_in + cen.bridges_out;
        let mut bridge_bit = if br_n > 0 {
            format!("; {br_n} interconnect bridge(s)")
        } else {
            String::new()
        };
        // Comprehensive census trailer when sample ≠ neighborhood or BFS pruned.
        let mut census_bits: Vec<String> = Vec::new();
        if cen.callers_total > cen.callers_shown || cen.callees_total > cen.callees_shown {
            census_bits.push(format!(
                "census callers={}/{} callees={}/{}",
                cen.callers_shown,
                cen.callers_total,
                cen.callees_shown,
                cen.callees_total
            ));
        }
        if cen.fan_out_pruned > 0 {
            census_bits.push(format!("fan_out_pruned={}", cen.fan_out_pruned));
        }
        if cen.visited_capped {
            census_bits.push("visited_capped".into());
        }
        if !census_bits.is_empty() {
            bridge_bit = format!("{bridge_bit} [{}]", census_bits.join(" "));
        }
        let domain_bit = match st.blast_domain.as_deref() {
            Some("type_neighborhood") => {
                " domain=type_neighborhood (not full ABI/layout — CALL only)".to_string()
            }
            Some("call") => " domain=call".to_string(),
            Some(other) => format!(" domain={other}"),
            None => String::new(),
        };
        // Human scope frame: “called by N · calls M · wide fan-in” (warehouse degrees).
        let scope_bit = {
            let t = &st.telemetry;
            let in_d = t
                .get("seed_in_degree")
                .and_then(|v| v.as_u64())
                .unwrap_or(cen.callers_direct as u64) as usize;
            let out_d = t
                .get("seed_out_degree")
                .and_then(|v| v.as_u64())
                .unwrap_or(cen.callees_direct as u64) as usize;
            let edges_complete = t
                .get("edges_complete")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let frame = scope_frame_line(
                in_d,
                out_d,
                cen.fan_out_pruned,
                cen.visited_capped,
                edges_complete,
                cen.bridges_in + cen.bridges_out,
            );
            format!(" {frame}")
        };
        let receipt_bit = receipt_compact_bit(st);
        let pain = pain_direct_lead(&t.name, &cen);
        // · separates seed locus from census (colon after path is only for line).
        if report_incomplete(st) {
            let pct = st.state.percent.unwrap_or(0).min(99);
            let conf = match st.state.confidence.as_deref() {
                Some("index_exact") => build_status::ConfidenceRung::IndexExact,
                Some("edges_partial") => build_status::ConfidenceRung::EdgesPartial,
                Some("edges_full") => build_status::ConfidenceRung::EdgesFull,
                _ => build_status::ConfidenceRung::Inventory,
            };
            let tag = build_status::honest_partial_tag(pct, conf);
            return format!(
                "{pain}Trace for {} ★ {seed} · {callers_bit} so far, {callees_bit} so far{bridge_bit} ({} blocks){alt_bit}.{domain_bit}{scope_bit}{receipt_bit} {tag}",
                t.name,
                blocks,
            );
        }
        return format!(
            "{pain}Trace for {} ★ {seed} · {callers_bit}, {callees_bit}{bridge_bit} ({} highly relevant blocks){alt_bit}.{domain_bit}{scope_bit}{receipt_bit}",
            t.name,
            blocks
        );
    }
    if st.skeleton.is_some() || st.hubs.is_some() {
        // Counts only — full map is built in orchestrate_content_arch_compact (agents read content).
        let paths = st.skeleton.as_ref().map(|s| s.len()).unwrap_or(0);
        let hubs_n = st.hubs.as_ref().map(|h| h.len()).unwrap_or(0);
        let cl_n = st.clusters.as_ref().map(|c| c.len()).unwrap_or(0);
        let br_n = st.bridges.as_ref().map(|b| b.len()).unwrap_or(0);
        if cl_n > 0 || br_n > 0 {
            return format!(
                "Architectural summary: {paths} skeleton paths, {hubs_n} hubs, {cl_n} clusters, {br_n} bridges."
            );
        }
        return format!("Architectural summary: {paths} skeleton paths, {hubs_n} hubs.");
    }
    let blocks = st.callers.len() + st.callees.len();
    if blocks > 0 {
        format!("Found {blocks} highly relevant blocks.")
    } else {
        "Orchestrate completed.".to_string()
    }
}

/// Honest per-neighbor basis tag (only signals we store — never invent import-bound).
pub(crate) fn neighbor_basis_tag(cc: &CallerCallee) -> &'static str {
    match cc.relation.as_deref() {
        Some("export") => "export",
        Some("ipc") => "ipc",
        Some("twin") => "twin",
        Some("ffi") => "ffi",
        Some("name_peer") => "name_peer",
        _ if cc.hop >= 2 => "transitive",
        _ => "call",
    }
}

pub(super) fn neighbor_trust_band(cc: &CallerCallee, receipt_conf: &str) -> &'static str {
    match neighbor_basis_tag(cc) {
        "export" | "ipc" | "twin" | "ffi" => "high",
        "name_peer" | "transitive" => "medium",
        _ => match receipt_conf {
            "high" => "high",
            "medium" => "medium",
            _ => "low",
        },
    }
}

/// Normalize paths for same-file checks (display suffixes vs abs roots).
pub(super) fn paths_same_file(a: &str, b: &str) -> bool {
    let na = a.replace('\\', "/").trim_start_matches("./").to_ascii_lowercase();
    let nb = b.replace('\\', "/").trim_start_matches("./").to_ascii_lowercase();
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    if na == nb {
        return true;
    }
    // Prefer basename+parent match so `/repo/a/b.rs` ≡ `a/b.rs` ≡ `b.rs` only when unique parent.
    let ta = na.rsplit('/').take(2).collect::<Vec<_>>();
    let tb = nb.rsplit('/').take(2).collect::<Vec<_>>();
    if ta.len() >= 2 && tb.len() >= 2 {
        return ta[0] == tb[0] && ta[1] == tb[1];
    }
    // Last resort: one path ends with the other (and the boundary is a `/`).
    let (long, short) = if na.len() >= nb.len() {
        (na.as_str(), nb.as_str())
    } else {
        (nb.as_str(), na.as_str())
    };
    long.ends_with(short)
        && (long.len() == short.len()
            || long.as_bytes().get(long.len() - short.len() - 1) == Some(&b'/'))
}

pub(super) fn format_neighbor_trust_line(
    i: usize,
    cc: &CallerCallee,
    receipt_conf: &str,
    seed_file: Option<&str>,
) -> String {
    let basis = neighbor_basis_tag(cc);
    let trust = neighbor_trust_band(cc, receipt_conf);
    let hop = if cc.hop > 1 {
        format!(" hop={}", cc.hop)
    } else {
        String::new()
    };
    let same = seed_file
        .map(|s| paths_same_file(s, &cc.file))
        .unwrap_or(false);
    let tags = if same {
        format!("basis: {basis} | trust: {trust} | same-file")
    } else {
        format!("basis: {basis} | trust: {trust}")
    };
    let mut line = format!(
        "{}. `{}` @ {}:{}{}  [{tags}]",
        i,
        cc.name,
        short_display_path(&cc.file),
        cc.line,
        hop,
    );
    if let Some(why) = cc.why.as_ref().filter(|s| !s.trim().is_empty()) {
        line.push_str(&format!("\n   why: {why}"));
    }
    line
}

/// Trait / boilerplate names that dominate type-neighborhood ranks but are rarely edit targets.
/// Soft demotion only in **display** order (graph unchanged).
pub(super) fn is_trace_neighbor_noise_name(name: &str) -> bool {
    let n = name.trim();
    if n.is_empty() {
        return false;
    }
    let nl = n.to_ascii_lowercase();
    // Exact boilerplate / trait impl surface (Rust + common idioms).
    matches!(
        nl.as_str(),
        "fmt"
            | "clone"
            | "default"
            | "new"
            | "drop"
            | "from"
            | "into"
            | "try_from"
            | "try_into"
            | "as_ref"
            | "as_mut"
            | "deref"
            | "deref_mut"
            | "borrow"
            | "borrow_mut"
            | "hash"
            | "eq"
            | "partial_cmp"
            | "cmp"
            | "debug"
            | "display"
            | "from_str"
            | "serialize"
            | "deserialize"
            | "type_id"
            | "clone_from"
    ) || matches!(
        n,
        "Debug"
            | "Default"
            | "Display"
            | "Clone"
            | "Hash"
            | "Eq"
            | "PartialEq"
            | "PartialOrd"
            | "Ord"
            | "Send"
            | "Sync"
            | "Copy"
            | "From"
            | "Into"
            | "TryFrom"
            | "TryInto"
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NeighborBucket {
    /// Cross-file product neighborhood — edit targets first.
    External = 0,
    /// Same-file helpers — useful, not entry points.
    Local = 1,
    /// Trait / boilerplate noise — last.
    Noise = 2,
}

fn neighbor_bucket(cc: &CallerCallee, seed_file: Option<&str>) -> NeighborBucket {
    if is_trace_neighbor_noise_name(&cc.name) {
        return NeighborBucket::Noise;
    }
    if let Some(sf) = seed_file {
        if paths_same_file(sf, &cc.file) {
            return NeighborBucket::Local;
        }
    }
    NeighborBucket::External
}

/// Partition neighbors for glove-fit Trace: External → Local → Noise (rank within bucket preserved).
pub(super) fn partition_neighbors_glove_fit<'a>(
    neighbors: &'a [CallerCallee],
    seed_file: Option<&str>,
) -> (Vec<&'a CallerCallee>, Vec<&'a CallerCallee>, Vec<&'a CallerCallee>) {
    let mut external = Vec::new();
    let mut local = Vec::new();
    let mut noise = Vec::new();
    for c in neighbors {
        match neighbor_bucket(c, seed_file) {
            NeighborBucket::External => external.push(c),
            NeighborBucket::Local => local.push(c),
            NeighborBucket::Noise => noise.push(c),
        }
    }
    (external, local, noise)
}

/// Flatten glove-fit order for dense dumps (external → local → noise).
fn neighbors_glove_fit_order<'a>(
    neighbors: &'a [CallerCallee],
    seed_file: Option<&str>,
) -> Vec<&'a CallerCallee> {
    let (e, l, n) = partition_neighbors_glove_fit(neighbors, seed_file);
    let mut out = Vec::with_capacity(neighbors.len());
    out.extend(e);
    out.extend(l);
    out.extend(n);
    out
}

/// Emit compact caller/callee sections: external first, local helpers, noise last.
fn append_neighbor_sections(
    lines: &mut Vec<String>,
    kind: &str, // "callers" | "callees"
    neighbors: &[CallerCallee],
    total: usize,
    top: usize,
    receipt_conf: &str,
    seed_file: Option<&str>,
) {
    if neighbors.is_empty() {
        lines.push(format!("{kind}: (none in sample / graph)"));
        return;
    }
    let (external, local, noise) = partition_neighbors_glove_fit(neighbors, seed_file);

    // Budget: fill external first (edit targets), then local, then noise only if room.
    let mut rem = top;
    let ext_n = external.len().min(rem);
    rem = rem.saturating_sub(ext_n);
    let loc_n = local.len().min(rem);
    rem = rem.saturating_sub(loc_n);
    let noise_n = noise.len().min(rem);

    let kind_label = if kind == "callers" { "callers" } else { "callees" };
    // Always surface bucket census so all-external (e.g. dispatch_tool-only) still
    // reads as an edit map, not a flat unlabeled list.
    if !local.is_empty() || !noise.is_empty() || !external.is_empty() {
        if local.is_empty() && noise.is_empty() && !external.is_empty() {
            lines.push(format!(
                "{kind_label} (top {} of {total} · all external):",
                neighbors.len().min(top)
            ));
        } else if external.is_empty() && noise.is_empty() && !local.is_empty() {
            lines.push(format!(
                "{kind_label} (top {} of {total} · all same-file helpers):",
                neighbors.len().min(top)
            ));
        } else if external.is_empty() && local.is_empty() && !noise.is_empty() {
            lines.push(format!(
                "{kind_label} (top {} of {total} · mostly trait/boilerplate noise):",
                neighbors.len().min(top)
            ));
        } else {
            lines.push(format!(
                "{kind_label} (top {top} of {total} · {} external · {} local · {} noise):",
                external.len(),
                local.len(),
                noise.len()
            ));
        }
    } else {
        lines.push(format!(
            "{kind_label} (top {} of {total}):",
            neighbors.len().min(top)
        ));
    }

    let mut shown = 0usize;
    if ext_n > 0 {
        // Always label external — including all-external lists (graph-truth edit map).
        let hdr = if kind == "callers" {
            "external callers (cross-file — primary edit targets):"
        } else {
            "external callees (cross-file):"
        };
        lines.push(hdr.into());
        for (i, c) in external.iter().take(ext_n).enumerate() {
            lines.push(format_neighbor_trust_line(
                i + 1,
                c,
                receipt_conf,
                seed_file,
            ));
            shown += 1;
        }
    }
    if loc_n > 0 {
        lines.push("local helpers (same-file — not external entry points):".into());
        for (i, c) in local.iter().take(loc_n).enumerate() {
            lines.push(format_neighbor_trust_line(
                i + 1,
                c,
                receipt_conf,
                seed_file,
            ));
            shown += 1;
        }
    }
    if noise_n > 0 {
        lines.push(
            "trait/boilerplate noise (fmt/Debug/Default/… — rarely edit targets):".into(),
        );
        for (i, c) in noise.iter().take(noise_n).enumerate() {
            let mut line =
                format_neighbor_trust_line(i + 1, c, receipt_conf, seed_file);
            // Loud tag so agents skip these when picking edit sites.
            if !line.contains("noise") {
                line = line.replacen("]", " | noise]", 1);
            }
            lines.push(line);
            shown += 1;
        }
    }
    let _ = shown;
}

/// Path tree for Arch maps — **directory nodes** so `filters/` is not indented under `dto.rs`.
///
/// Previous depth-only indent made `filters/foo` look nested under the previous flat file.
pub(super) fn format_skeleton_tree(paths: &[String], max_lines: usize) -> Vec<String> {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Node {
        dirs: BTreeMap<String, Node>,
        files: Vec<String>,
    }

    let mut items: Vec<String> = paths
        .iter()
        .filter(|p| !p.starts_with("[..."))
        .map(|p| p.replace('\\', "/"))
        .collect();
    items.sort();
    items.dedup();

    let split: Vec<Vec<String>> = items
        .iter()
        .map(|p| {
            p.split('/')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .collect();

    // Strip common leading segments shared by every path (e.g. …/cli/src/server).
    let mut common = 0usize;
    if let Some(first) = split.first() {
        'outer: while common < first.len() {
            let seg = &first[common];
            for other in &split {
                if other.len() <= common || &other[common] != seg {
                    break 'outer;
                }
            }
            if split.iter().all(|o| o.len() > common + 1) {
                common += 1;
            } else {
                break;
            }
        }
    }

    let mut root = Node::default();
    for parts in &split {
        if common >= parts.len() {
            continue;
        }
        let rel = &parts[common..];
        if rel.is_empty() {
            continue;
        }
        let mut cur = &mut root;
        for (i, seg) in rel.iter().enumerate() {
            if i + 1 == rel.len() {
                cur.files.push(seg.clone());
            } else {
                cur = cur.dirs.entry(seg.clone()).or_default();
            }
        }
    }

    fn walk(node: &Node, depth: usize, lines: &mut Vec<String>, max_lines: usize) {
        for (name, child) in &node.dirs {
            if lines.len() >= max_lines {
                return;
            }
            let indent = "  ".repeat(depth.min(8));
            lines.push(format!("  {indent}{name}/"));
            walk(child, depth + 1, lines, max_lines);
        }
        for f in &node.files {
            if lines.len() >= max_lines {
                return;
            }
            let indent = "  ".repeat(depth.min(8));
            lines.push(format!("  {indent}{f}"));
        }
    }

    let mut lines = Vec::new();
    walk(&root, 0, &mut lines, max_lines);
    if lines.len() >= max_lines && items.len() > max_lines {
        // Approximate remainder note
        lines.push(format!(
            "  … truncated (max {max_lines} tree lines; structured.skeleton has full list)"
        ));
    }
    lines
}

/// Arch compact: mini map in **content** (agents that ignore structuredContent).
///
/// Completeness matters: partial lists → agents list_dir. Prefer full mid-size trees.
pub(super) fn orchestrate_content_arch_compact(st: &StructuredReport) -> String {
    let skel = st.skeleton.as_deref().unwrap_or(&[]);
    let hubs = st.hubs.as_deref().unwrap_or(&[]);
    let skel_paths: Vec<String> = skel
        .iter()
        .filter(|p| !p.starts_with("[..."))
        .cloned()
        .collect();
    let unique_n = st
        .telemetry
        .get("unique_files_under_scope")
        .and_then(|v| v.as_u64())
        .unwrap_or(skel_paths.len() as u64) as usize;
    let rolled_up = st
        .telemetry
        .get("skeleton_rolled_up")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let coverage_complete = st
        .telemetry
        .get("coverage_complete")
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| {
            !rolled_up
                && st
                    .telemetry
                    .get("payload_omitted")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    == 0
                && unique_n > 0
        });

    let mut lines: Vec<String> = Vec::new();
    let cov = if coverage_complete {
        format!("coverage: {unique_n} files under scope (complete)")
    } else if rolled_up {
        format!(
            "coverage: {unique_n} files under scope rolled into {} dir/entry rows (incomplete — re-Arch with scope_paths)",
            skel_paths.len()
        )
    } else {
        let omitted = st
            .telemetry
            .get("payload_omitted")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        format!(
            "coverage: {} of ~{} files shown (incomplete; omitted≈{})",
            skel_paths.len(),
            unique_n.max(skel_paths.len()),
            omitted
        )
    };
    let br_n = st.bridges.as_ref().map(|b| b.len()).unwrap_or(0);
    let br_bit = if br_n > 0 {
        format!(", {br_n} interconnect bridge(s)")
    } else {
        String::new()
    };
    lines.push(format!(
        "Architectural summary: {} skeleton paths, {} hubs{br_bit}. {cov}",
        skel_paths.len(),
        hubs.len()
    ));

    if !skel_paths.is_empty() {
        lines.push(format!("tree ({} paths):", skel_paths.len()));
        // Adaptive tree lines: follow skeleton_full_max telemetry when present (repo-adaptive).
        let max_tree = st
            .telemetry
            .get("skeleton_full_max")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).saturating_add(40).min(360))
            .unwrap_or(200);
        lines.extend(format_skeleton_tree(&skel_paths, max_tree));
    }

    if !hubs.is_empty() {
        lines.push(format!("hubs (top {}):", hubs.len().min(12)));
        let mut ranked = hubs.to_vec();
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for h in ranked.iter().take(12) {
            let tag = match (h.lang.as_deref(), h.cluster.as_deref()) {
                (Some(l), Some(c)) => format!(" · {l} · {c}"),
                (Some(l), None) => format!(" · {l}"),
                (None, Some(c)) => format!(" · {c}"),
                _ => String::new(),
            };
            lines.push(format!(
                "  - {}{}  @ {}",
                h.name,
                tag,
                short_display_path(&h.file)
            ));
        }
    }

    // P.13: surface interconnect on Arch compact (Trace is primary; agents still read Arch first).
    if let Some(bridges) = st.bridges.as_ref() {
        if !bridges.is_empty() {
            lines.push(format!("interconnect bridges ({}):", bridges.len().min(8)));
            for b in bridges.iter().take(8) {
                lines.push(format!(
                    "  - {} ({}) → {} ({})  @ {}",
                    b.from_name,
                    b.from_lang,
                    b.to_name,
                    b.to_lang,
                    short_display_path(&b.from_file)
                ));
            }
        }
    }

    if !st.suggested_scopes.is_empty() {
        lines.push(format!(
            "suggested_scopes: {}",
            st.suggested_scopes
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let default_next = if coverage_complete {
        "format this map (tree+hubs); Trace a hub — do not list_dir when coverage complete"
    } else if rolled_up {
        "re-Arch with scope_paths on a listed directory for full basenames; prefer suggested_scopes over list_dir"
    } else {
        "narrow scope_paths or detail=dense for full skeleton; avoid blind recursive file walk"
    };
    let next = st
        .next_action
        .as_deref()
        .filter(|n| !n.is_empty())
        .unwrap_or(default_next);
    if !lines.iter().any(|l| l.starts_with("next:")) {
        lines.push(format!("next: {next}"));
    }

    lines.join("\n")
}

/// Compact = **text UI for trust** (agents that ignore structuredContent).
///
/// Always includes: headline + receipt + scope frame + top neighbors with basis +
/// bridges + cap honesty + next_action when set. Capped so 7B/local stay usable.
pub(super) fn orchestrate_content_compact(st: &StructuredReport) -> String {
    // Arch map: real paths/hubs in content (not counts-only — OOD tool habit).
    if st.target.is_none() && (st.skeleton.is_some() || st.hubs.is_some()) {
        return orchestrate_content_arch_compact(st);
    }

    // Errors / disambiguate without target: headline already carries next.
    if st.target.is_none() {
        let mut out = compact_headline(st);
        if let Some(next) = st.next_action.as_ref().filter(|n| !n.is_empty()) {
            if !out.contains("next:") {
                out.push_str(&format!("\nnext: {next}"));
            }
        }
        if st.blast_domain.as_deref() == Some("disambiguate") {
            if !st.suggested_scopes.is_empty() {
                out.push_str(&format!(
                    "\nsuggested_scopes: {}",
                    st.suggested_scopes.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
                ));
            }
            if let Some(locs) = st.locations.as_ref() {
                let show = locs.iter().take(5);
                out.push_str("\nlocations (pin one):");
                for (i, loc) in show.enumerate() {
                    let mark = if loc.preferred { "★" } else { "-" };
                    out.push_str(&format!(
                        "\n  {mark} {}. {} @ {}:{}",
                        i + 1,
                        loc.name,
                        short_display_path(&loc.file),
                        loc.line
                    ));
                }
            }
        }
        return out;
    }

    let mut lines: Vec<String> = Vec::new();
    lines.push(compact_headline(st));

    let r = st
        .receipt
        .clone()
        .unwrap_or_else(|| compute_trace_receipt(st));
    lines.push(format!(
        "receipt: confidence={} · basis={} · edges={} · ladder={}",
        r.confidence, r.basis, r.edges, r.ladder
    ));

    // Explicit scope / cap frame (warehouse degrees when present).
    let cen = edge_census_from_report(st);
    let t_meta = &st.telemetry;
    let in_d = t_meta
        .get("seed_in_degree")
        .and_then(|v| v.as_u64())
        .unwrap_or(cen.callers_direct as u64) as usize;
    let out_d = t_meta
        .get("seed_out_degree")
        .and_then(|v| v.as_u64())
        .unwrap_or(cen.callees_direct as u64) as usize;
    let edges_complete = t_meta
        .get("edges_complete")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    lines.push(scope_frame_line(
        in_d,
        out_d,
        cen.fan_out_pruned,
        cen.visited_capped,
        edges_complete,
        cen.bridges_in + cen.bridges_out,
    ));
    // Hub UX: explicit mega-hub note (agents ignore omitted counts otherwise).
    let hub_scale = t_meta
        .get("hub_scale")
        .and_then(|v| v.as_bool())
        .unwrap_or(in_d >= 51 || out_d >= 51);
    let pack_scope_bias = t_meta
        .get("pack_scope_bias")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let callers_omitted = t_meta
        .get("callers_omitted")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let callees_omitted = t_meta
        .get("callees_omitted")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    // Soft I4 honesty: unmissable sample-vs-warehouse line (not only mega-hub).
    let sample_offset = t_meta
        .get("sample_offset")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let sample_mode = t_meta
        .get("sample_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("score");
    if callers_omitted > 0 || callees_omitted > 0 || hub_scale || sample_offset > 0 {
        let cr_shown = cen.callers_shown.max(st.callers.len());
        let ce_shown = cen.callees_shown.max(st.callees.len());
        let cr_wh = in_d.max(cen.callers_total).max(cr_shown);
        let ce_wh = out_d.max(cen.callees_total).max(ce_shown);
        let cr_lo = if cr_shown == 0 {
            0
        } else {
            sample_offset + 1
        };
        let cr_hi = sample_offset + cr_shown;
        lines.push(format!(
            "note: callers sample {cr_lo}–{cr_hi} of warehouse {cr_wh} (offset={sample_offset} omitted {callers_omitted} · mode={sample_mode}) · \
             callees sample {ce_shown} of warehouse {ce_wh} (omitted {callees_omitted}) — \
             not full reverse/forward; wrong window → sample_offset / exclude_symbols / sample_mode=diverse / scope_paths; \
             hop parent → focus_symbol."
        ));
    }
    if t_meta
        .get("sample_window_exhausted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        lines.push(
            "note: sample window exhausted (offset/exclude past ranked candidates) — pin scope_paths or lower sample_offset."
                .into(),
        );
    }
    if hub_scale {
        if pack_scope_bias {
            lines.push(format!(
                "note: mega-hub (warehouse fan-in {in_d}) — sample biased to scope_paths; pin further or focus_symbol for hop continuity."
            ));
        } else {
            lines.push(format!(
                "note: mega-hub (warehouse fan-in {in_d}) — sample only; pin scope_paths or pass focus_symbol from previous Trace."
            ));
        }
    }
    if let Some(arr) = t_meta.get("focus_injected").and_then(|v| v.as_array()) {
        let names: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
        if !names.is_empty() {
            lines.push(format!(
                "note: focus_symbol injected into callers sample: {}.",
                names.join(", ")
            ));
        }
    }
    if let Some(arr) = t_meta.get("focus_missed").and_then(|v| v.as_array()) {
        let names: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
        if !names.is_empty() {
            lines.push(format!(
                "note: focus_symbol not in warehouse callers of ★ (not a CALL parent, or wrong pin): {}.",
                names.join(", ")
            ));
        }
    }
    if cen.callers_total > cen.callers_shown || cen.callees_total > cen.callees_shown {
        lines.push(format!(
            "note: list capped (callers shown {}/{} · callees {}/{}) — not the full graph.",
            cen.callers_shown,
            cen.callers_total.max(cen.callers_shown),
            cen.callees_shown,
            cen.callees_total.max(cen.callees_shown)
        ));
    }

    const TOP: usize = 5;
    let conf = r.confidence.as_str();
    let seed_file = st.target.as_ref().map(|t| t.file.as_str());

    // Callers / callees — external first; same-file tagged so helpers ≠ product entrypoints.
    // Direct CALL into ★ only (same-name peers listed separately below).
    if st.callers.is_empty() {
        if let Some(t) = st.target.as_ref() {
            lines.push(empty_callers_line(t, st));
        } else {
            lines.push("callers: (none in sample / graph)".into());
        }
    } else {
        let total = cen.callers_total.max(st.callers.len());
        append_neighbor_sections(
            &mut lines,
            "callers",
            &st.callers,
            total,
            TOP,
            conf,
            seed_file,
        );
    }

    // Same-name peer reverse — not CALL into ★ (gin/prometheus Default* class trap).
    if !st.peer_callers.is_empty() {
        let peer_total = st
            .telemetry
            .get("seed_in_degree_name_peers")
            .and_then(|v| v.as_u64())
            .unwrap_or(st.peer_callers.len() as u64) as usize;
        lines.push(format!(
            "peer_callers (top {} of {} · same-name only — not CALL into ★):",
            st.peer_callers.len().min(TOP),
            peer_total.max(st.peer_callers.len())
        ));
        for pc in st.peer_callers.iter().take(TOP) {
            let why = pc
                .why
                .as_deref()
                .unwrap_or("calls a different function with the same name");
            lines.push(format!(
                "  - `{}` @ {}:{}  [basis: name_peer | trust: medium]",
                pc.name,
                short_path(&pc.file),
                pc.line
            ));
            lines.push(format!("    why: {why}"));
        }
    }

    // Reverse CALL spine: seed ← parent ← … (edit path, not L2 blast).
    if let Some(t) = st.target.as_ref() {
        for line in super::spine::compact_spine_lines(&t.name, &st.caller_path) {
            lines.push(line);
        }
    }

    if st.callees.is_empty() {
        lines.push("callees: (none in sample / graph)".into());
    } else {
        let total = cen.callees_total.max(st.callees.len());
        append_neighbor_sections(
            &mut lines,
            "callees",
            &st.callees,
            total,
            TOP,
            conf,
            seed_file,
        );
    }

    // Bridges — always section so agents see dual-stack or explicit none
    let br_in = &st.bridge_callers;
    let br_out = &st.bridge_callees;
    if br_in.is_empty() && br_out.is_empty() {
        lines.push("bridges: (none — no export/ipc/twin in sample)".into());
    } else {
        lines.push(format!(
            "bridges ({} in, {} out — not CALL):",
            br_in.len(),
            br_out.len()
        ));
        for c in br_in.iter().take(3) {
            let basis = neighbor_basis_tag(c);
            let trust = neighbor_trust_band(c, conf);
            lines.push(format!(
                "  ← `{}` @ {}:{}  [basis: {} | trust: {}]",
                c.name,
                short_display_path(&c.file),
                c.line,
                basis,
                trust
            ));
            if let Some(why) = c.why.as_ref().filter(|s| !s.trim().is_empty()) {
                lines.push(format!("     why: {why}"));
            }
        }
        for c in br_out.iter().take(3) {
            let basis = neighbor_basis_tag(c);
            let trust = neighbor_trust_band(c, conf);
            lines.push(format!(
                "  → `{}` @ {}:{}  [basis: {} | trust: {}]",
                c.name,
                short_display_path(&c.file),
                c.line,
                basis,
                trust
            ));
            if let Some(why) = c.why.as_ref().filter(|s| !s.trim().is_empty()) {
                lines.push(format!("     why: {why}"));
            }
        }
    }

    if let Some(next) = st.next_action.as_ref().filter(|n| !n.is_empty()) {
        lines.push(format!("next: {next}"));
    } else if st.blast_domain.as_deref() == Some("disambiguate") {
        lines.push(format!("next: {}", next_action_disambiguate()));
    } else if !st.bridge_callers.is_empty() || !st.bridge_callees.is_empty() {
        lines.push(
            "next: Trace the bridge neighbor above, or reverse seed (export/ipc other side)"
                .into(),
        );
    } else if !st.callers.is_empty() || !st.callees.is_empty() {
        lines.push(
            "next: Trace a caller/callee above for deeper blast, or pin scope_paths to re-seed"
                .into(),
        );
    } else if in_d == 0 {
        // 0 CALL warehouse: do not tutor "dead code" — callback / framework / external.
        lines.push(
            "next: do not delete as dead code; search references/decorators/routes, or Arch for entrypoints"
                .into(),
        );
    } else {
        lines.push(
            "next: widen scope_paths, rewalk if edges partial, or ArchitecturalSummary to orient"
                .into(),
        );
    }

    lines.join("\n")
}

pub(super) fn prepend_honest_banner(st: &StructuredReport, body: String) -> String {
    if !report_incomplete(st) {
        return body;
    }
    let pct = st.state.percent.unwrap_or(0);
    let conf = match st.state.confidence.as_deref() {
        Some("index_exact") => build_status::ConfidenceRung::IndexExact,
        Some("edges_partial") => build_status::ConfidenceRung::EdgesPartial,
        Some("edges_full") => build_status::ConfidenceRung::EdgesFull,
        Some("inventory") | _ => build_status::ConfidenceRung::Inventory,
    };
    let banner = build_status::honest_partial_banner(pct, conf, Some(&st.state.edge_build));
    if banner.is_empty() {
        body
    } else {
        format!("{banner}\n\n{body}")
    }
}

/// Dense content: full agent-usable dump when `detail=dense` (or client drops structuredContent).
pub(super) fn orchestrate_content_dense(st: &StructuredReport, mermaid: Option<&str>) -> String {
    // T.2 disambiguate is a soft error — still show locations / suggested_scopes.
    let is_disambig = st.blast_domain.as_deref() == Some("disambiguate")
        || st
            .telemetry
            .get("disambiguate")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    if let Some(err) = st.error.as_ref().filter(|e| !e.is_empty()) {
        if !is_disambig {
            let mut body = format!(
                "Orchestrate error: {err}\nstate: edge_build={} | jit={} | confidence={}",
                st.state.edge_build,
                st.state.jit,
                st.state.confidence.as_deref().unwrap_or("?")
            );
            if let Some(next) = st.next_action.as_ref().filter(|n| !n.is_empty()) {
                if !err.contains("next:") {
                    body.push_str(&format!("\nnext: {next}"));
                }
            }
            if !st.suggested_scopes.is_empty() {
                body.push_str(&format!(
                    "\nsuggested_scopes: {}",
                    st.suggested_scopes.join(", ")
                ));
            }
            return prepend_honest_banner(st, body);
        }
    }

    let mut lines: Vec<String> = vec![compact_headline(st)];
    if is_disambig {
        if let Some(err) = st.error.as_ref() {
            lines.push(err.clone());
        }
        let action = st
            .next_action
            .as_deref()
            .unwrap_or("pin scope_paths to one file/dir from locations below, then re-Trace");
        lines.push(format!("next: {action}"));
        if !st.suggested_scopes.is_empty() {
            lines.push(format!(
                "suggested_scopes: {}",
                st.suggested_scopes.join(", ")
            ));
        }
    }

    // --- Trace / FindImplementation ---
    if let Some(t) = st.target.as_ref() {
        lines.push(format!(
            "state: edge_build={} | jit={}",
            st.state.edge_build, st.state.jit
        ));
        if let Some(ac) = st.active_cluster.as_ref() {
            lines.push(format!("active_cluster: {ac}"));
        }
        lines.push(format!(
            "target: {}",
            format_loc_lang(
                &t.name,
                &t.file,
                t.line,
                t.lang.as_deref(),
                t.cluster.as_deref(),
                None,
            )
        ));
        if let Some(mod_name) = st.module_resolved_from.as_ref() {
            lines.push(format!("module_resolved_from: {mod_name}"));
            if let Some(cands) = st.module_interior_candidates.as_ref() {
                if cands.len() > 1 {
                    lines.push(format!(
                        "module_interior_candidates (top {}):",
                        cands.len().min(3)
                    ));
                    for c in cands.iter().take(3) {
                        lines.push(format!(
                            "  - {} @ {}:{} [{}] rank={:.0}",
                            c.name,
                            short_path(&c.file),
                            c.line,
                            c.kind,
                            c.rank_score
                        ));
                    }
                }
            }
        }
        // rg-shaped multi-hit list (before preferred def dive)
        if let Some(locs) = st.locations.as_ref() {
            if !locs.is_empty() {
                let show = locs.len().min(12);
                lines.push(format!(
                    "locations ({} match{}, showing {}):",
                    locs.len(),
                    if locs.len() == 1 { "" } else { "es" },
                    show
                ));
                for (i, loc) in locs.iter().take(show).enumerate() {
                    let mark = if loc.preferred { "  ★" } else { "  -" };
                    let end = loc
                        .end_line
                        .filter(|&e| e > loc.line)
                        .map(|e| format!("-{e}"))
                        .unwrap_or_default();
                    let lang_c = match (loc.lang.as_deref(), loc.cluster.as_deref()) {
                        (Some(l), Some(c)) => format!("  {l} · {c}"),
                        (Some(l), None) => format!("  {l}"),
                        (None, Some(c)) => format!("  {c}"),
                        _ => String::new(),
                    };
                    lines.push(format!(
                        "{mark} {}. {}  {}{}  {}:{}{}{}",
                        i + 1,
                        loc.name,
                        loc.kind,
                        lang_c,
                        short_path(&loc.file),
                        loc.line,
                        end,
                        if loc.preferred { "  [preferred]" } else { "" }
                    ));
                }
                if locs.len() > show {
                    lines.push(format!("  … +{} more", locs.len() - show));
                }
            }
        }
        // Track T.1 receipt (dense) — explicit trust, not lore
        {
            let r = st
                .receipt
                .clone()
                .unwrap_or_else(|| compute_trace_receipt(st));
            lines.push(format!(
                "receipt: confidence={} · basis={} · edges={} · ladder={}",
                r.confidence, r.basis, r.edges, r.ladder
            ));
        }
        // P.4 / P.5 domain banner
        if let Some(dom) = st.blast_domain.as_deref() {
            let kind = st.seed_kind.as_deref().unwrap_or("?");
            match dom {
                "type_neighborhood" => lines.push(format!(
                    "domain: type_neighborhood · seed_kind={kind}\n\
                     note: CALL Trace is a type neighborhood, NOT full ABI/layout blast radius \
                     (embeds, *mut T, repr(C) need separate search)."
                )),
                "call" => lines.push(format!(
                    "domain: call · seed_kind={kind}\n\
                     note: callers/callees below are CALL edges; interconnect bridges listed separately."
                )),
                other => lines.push(format!("domain: {other} · seed_kind={kind}")),
            }
        }
        if let Some(def) = t.definition.as_ref().filter(|d| !d.trim().is_empty()) {
            lines.push("definition:".to_string());
            lines.push(truncate_def(def, 900));
        }
        // Comprehensive edge census + human scope frame (not a full edge dump).
        {
            let cen = edge_census_from_report(st);
            let t = &st.telemetry;
            let in_d = t
                .get("seed_in_degree")
                .and_then(|v| v.as_u64())
                .unwrap_or(cen.callers_direct as u64) as usize;
            let out_d = t
                .get("seed_out_degree")
                .and_then(|v| v.as_u64())
                .unwrap_or(cen.callees_direct as u64) as usize;
            let edges_complete = t
                .get("edges_complete")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            lines.push(scope_frame_line(
                in_d,
                out_d,
                cen.fan_out_pruned,
                cen.visited_capped,
                edges_complete,
                cen.bridges_in + cen.bridges_out,
            ));
            let mut bits = vec![format!(
                "edge_census: CALL callers shown={}/{} ({} direct, {} hop≥2)",
                cen.callers_shown,
                cen.callers_total.max(cen.callers_shown),
                cen.callers_direct,
                cen.callers_transitive
            )];
            bits.push(format!(
                "callees shown={}/{} ({} direct, {} hop≥2)",
                cen.callees_shown,
                cen.callees_total.max(cen.callees_shown),
                cen.callees_direct,
                cen.callees_transitive
            ));
            bits.push(format!("seed_in_degree={in_d} seed_out_degree={out_d}"));
            if cen.fan_out_pruned > 0 {
                bits.push(format!("fan_out_pruned={}", cen.fan_out_pruned));
            }
            if cen.visited_capped {
                bits.push("visited_capped=true".into());
            }
            bits.push(format!(
                "bridges in={} out={}",
                cen.bridges_in, cen.bridges_out
            ));
            lines.push(bits.join("; "));
            if cen.callers_total > cen.callers_shown || cen.callees_total > cen.callees_shown {
                lines.push(
                    "note: lists below are a ranked sample — seed_in/out_degree is the full warehouse direct CALL count for this seed; shown/total is the Trace neighborhood after fan-out caps."
                        .into(),
                );
            }
        }
        if !st.callers.is_empty() {
            let cen = edge_census_from_report(st);
            let total = cen.callers_total.max(st.callers.len());
            let (d, tr) = hop_split(&st.callers);
            let head = if total > st.callers.len() {
                format!(
                    "CALL callers (showing {} of {total}; neighborhood {}d+{}h — hop>1 not direct):",
                    st.callers.len(),
                    cen.callers_direct,
                    cen.callers_transitive
                )
            } else if tr > 0 {
                format!(
                    "CALL callers ({}: {d} direct, {tr} hop≥2 — hop>1 is not a direct caller):",
                    st.callers.len()
                )
            } else {
                format!("CALL callers ({}):", st.callers.len())
            };
            lines.push(head);
            let seed = t.file.as_str();
            let ordered = neighbors_glove_fit_order(&st.callers, Some(seed));
            for (i, c) in ordered.iter().take(12).enumerate() {
                let mut loc = format_loc_lang_hop(
                    &c.name,
                    &c.file,
                    c.line,
                    c.lang.as_deref(),
                    c.cluster.as_deref(),
                    c.relation.as_deref(),
                    c.hop,
                );
                if paths_same_file(seed, &c.file) {
                    loc.push_str(" · same-file");
                }
                if is_trace_neighbor_noise_name(&c.name) {
                    loc.push_str(" · noise");
                }
                lines.push(format!("  - {loc}"));
                // Cite pack + why-edge: top 3 neighbors (agent→user trust)
                if i < 3 {
                    if let Some(why) = c.why.as_ref().filter(|s| !s.trim().is_empty()) {
                        lines.push(format!("      why: {why}"));
                    }
                    if let Some(cite) = c.cite.as_ref().filter(|s| !s.trim().is_empty()) {
                        for cl in cite.lines().take(6) {
                            lines.push(format!("      | {cl}"));
                        }
                    }
                }
            }
            if ordered.len() > 12 {
                lines.push(format!("  … +{} more shown", ordered.len() - 12));
            }
            let omit = total.saturating_sub(st.callers.len());
            if omit > 0 {
                lines.push(format!(
                    "  … +{omit} more in graph (not shown; raise fan-out / detail offline for full list)"
                ));
            }
        } else {
            lines.push(empty_callers_line(t, st));
        }
        // Same-name peer reverse — not CALL into ★
        if !st.peer_callers.is_empty() {
            let peer_total = st
                .telemetry
                .get("seed_in_degree_name_peers")
                .and_then(|v| v.as_u64())
                .unwrap_or(st.peer_callers.len() as u64) as usize;
            lines.push(format!(
                "peer_callers (showing {} of {peer_total} · same-name only — not CALL into ★):",
                st.peer_callers.len()
            ));
            for pc in st.peer_callers.iter().take(12) {
                let mut loc = format_loc_lang_hop(
                    &pc.name,
                    &pc.file,
                    pc.line,
                    pc.lang.as_deref(),
                    pc.cluster.as_deref(),
                    pc.relation.as_deref(),
                    pc.hop,
                );
                loc.push_str(" · name_peer");
                lines.push(format!("  - {loc}"));
                if let Some(why) = pc.why.as_ref().filter(|s| !s.trim().is_empty()) {
                    lines.push(format!("      why: {why}"));
                }
            }
        }
        if !st.callees.is_empty() {
            let cen = edge_census_from_report(st);
            let total = cen.callees_total.max(st.callees.len());
            let (d, tr) = hop_split(&st.callees);
            let head = if total > st.callees.len() {
                format!(
                    "CALL callees (showing {} of {total}; neighborhood {}d+{}h — hop>1 not direct):",
                    st.callees.len(),
                    cen.callees_direct,
                    cen.callees_transitive
                )
            } else if tr > 0 {
                format!(
                    "CALL callees ({}: {d} direct, {tr} hop≥2 — hop>1 is not a direct call):",
                    st.callees.len()
                )
            } else {
                format!("CALL callees ({}):", st.callees.len())
            };
            lines.push(head);
            let seed = t.file.as_str();
            let ordered = neighbors_glove_fit_order(&st.callees, Some(seed));
            for (i, c) in ordered.iter().take(12).enumerate() {
                let mut loc = format_loc_lang_hop(
                    &c.name,
                    &c.file,
                    c.line,
                    c.lang.as_deref(),
                    c.cluster.as_deref(),
                    c.relation.as_deref(),
                    c.hop,
                );
                if paths_same_file(seed, &c.file) {
                    loc.push_str(" · same-file");
                }
                if is_trace_neighbor_noise_name(&c.name) {
                    loc.push_str(" · noise");
                }
                lines.push(format!("  - {loc}"));
                if i < 3 {
                    if let Some(why) = c.why.as_ref().filter(|s| !s.trim().is_empty()) {
                        lines.push(format!("      why: {why}"));
                    }
                    if let Some(cite) = c.cite.as_ref().filter(|s| !s.trim().is_empty()) {
                        for cl in cite.lines().take(6) {
                            lines.push(format!("      | {cl}"));
                        }
                    }
                }
            }
            if ordered.len() > 12 {
                lines.push(format!("  … +{} more shown", ordered.len() - 12));
            }
            let omit = total.saturating_sub(st.callees.len());
            if omit > 0 {
                lines.push(format!(
                    "  … +{omit} more in graph (not shown; raise fan-out / detail offline for full list)"
                ));
            }
        } else {
            lines.push("CALL callees: (none in scope / graph)".to_string());
        }
        // P.4: typed interconnect neighbors (Export/Ipc/Twin) — separate from CALL
        let br_in = &st.bridge_callers;
        let br_out = &st.bridge_callees;
        if !br_in.is_empty() || !br_out.is_empty() {
            lines.push(format!(
                "interconnect bridges ({} in, {} out — not CALL; relation=export|ipc|twin):",
                br_in.len(),
                br_out.len()
            ));
            for (i, c) in br_in.iter().take(8).enumerate() {
                lines.push(format!(
                    "  ← {}",
                    format_loc_lang_hop(
                        &c.name,
                        &c.file,
                        c.line,
                        c.lang.as_deref(),
                        c.cluster.as_deref(),
                        c.relation.as_deref(),
                        c.hop,
                    )
                ));
                if i < 3 {
                    if let Some(why) = c.why.as_ref().filter(|s| !s.trim().is_empty()) {
                        lines.push(format!("      why: {why}"));
                    }
                }
            }
            for (i, c) in br_out.iter().take(8).enumerate() {
                lines.push(format!(
                    "  → {}",
                    format_loc_lang_hop(
                        &c.name,
                        &c.file,
                        c.line,
                        c.lang.as_deref(),
                        c.cluster.as_deref(),
                        c.relation.as_deref(),
                        c.hop,
                    )
                ));
                if i < 3 {
                    if let Some(why) = c.why.as_ref().filter(|s| !s.trim().is_empty()) {
                        lines.push(format!("      why: {why}"));
                    }
                }
            }
        }
        // Arch-style cross-cluster bridges (when present)
        if let Some(bridges) = st.bridges.as_ref() {
            if !bridges.is_empty() {
                lines.push(format!("cluster bridges ({} cross-cluster):", bridges.len()));
                for b in bridges.iter().take(8) {
                    lines.push(format!(
                        "  - {} · {} · {} → {} · {} · {}",
                        b.from_name,
                        b.from_lang,
                        b.from_cluster,
                        b.to_name,
                        b.to_lang,
                        b.to_cluster
                    ));
                }
                if bridges.len() > 8 {
                    lines.push(format!("  … +{} more", bridges.len() - 8));
                }
            }
        }
    } else if st.skeleton.is_some() || st.hubs.is_some() {
        lines.push(format!(
            "state: edge_build={} | jit={}",
            st.state.edge_build, st.state.jit
        ));
        if let Some(clusters) = st.clusters.as_ref() {
            if !clusters.is_empty() {
                lines.push(format!("clusters ({}):", clusters.len()));
                for c in clusters.iter().take(8) {
                    let ents = if c.entries.is_empty() {
                        String::new()
                    } else {
                        format!("  entries: {}", c.entries.join(", "))
                    };
                    lines.push(format!(
                        "  - {} ({})  nodes={} files={}{}",
                        c.badge, c.label, c.nodes, c.files, ents
                    ));
                }
            }
        }
        if let Some(bridges) = st.bridges.as_ref() {
            if !bridges.is_empty() {
                lines.push(format!("bridges ({} cross-cluster):", bridges.len()));
                for b in bridges.iter().take(10) {
                    lines.push(format!(
                        "  - {} · {} · {} → {} · {} · {}  ({} → {})",
                        b.from_name,
                        b.from_lang,
                        b.from_cluster,
                        b.to_name,
                        b.to_lang,
                        b.to_cluster,
                        short_path(&b.from_file),
                        short_path(&b.to_file)
                    ));
                }
                if bridges.len() > 10 {
                    lines.push(format!("  … +{} more", bridges.len() - 10));
                }
            }
        }
        if let Some(hubs) = st.hubs.as_ref() {
            if !hubs.is_empty() {
                lines.push(format!("hubs (top {}):", hubs.len().min(10)));
                let mut ranked = hubs.clone();
                ranked.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                for h in ranked.iter().take(10) {
                    let tag = match (h.lang.as_deref(), h.cluster.as_deref()) {
                        (Some(l), Some(c)) => format!(" · {l} · {c}"),
                        (Some(l), None) => format!(" · {l}"),
                        (None, Some(c)) => format!(" · {c}"),
                        _ => String::new(),
                    };
                    lines.push(format!(
                        "  - {}{}  score={:.2}  {}",
                        h.name,
                        tag,
                        h.score,
                        short_path(&h.file)
                    ));
                }
            }
        }
        if let Some(skel) = st.skeleton.as_ref() {
            if !skel.is_empty() {
                lines.push(format!("skeleton ({} paths):", skel.len()));
                for p in skel.iter().take(20) {
                    lines.push(format!("  - {}", short_path(p)));
                }
                if skel.len() > 20 {
                    lines.push(format!("  … +{} more", skel.len() - 20));
                }
            }
        }
    } else {
        lines.push(format!(
            "state: edge_build={} | jit={}",
            st.state.edge_build, st.state.jit
        ));
    }

    if !st.suggested_scopes.is_empty() {
        lines.push(format!(
            "suggested_scopes: {}",
            st.suggested_scopes.join(", ")
        ));
    }

    if let Some(obj) = st.telemetry.as_object() {
        let mut bits = Vec::new();
        for key in [
            "blocks_scanned",
            "payload_blocks",
            "tokens_saved_estimate",
            "total_time_ms",
            "trace_nodes_visited",
            "fan_out_pruned",
            "visited_capped",
            "type",
        ] {
            if let Some(v) = obj.get(key) {
                if !v.is_null() {
                    bits.push(format!("{key}={v}"));
                }
            }
        }
        if !bits.is_empty() {
            lines.push(format!("telemetry: {}", bits.join(" ")));
        }
    }

    if let Some(m) = mermaid.filter(|s| !s.trim().is_empty()) {
        lines.push("mermaid:".to_string());
        lines.push(m.trim().to_string());
    }

    prepend_honest_banner(st, lines.join("\n"))
}

/// Build orchestrate `content` for the chosen detail level.
///
/// - **Compact** (default): Trace trust dossier, or Arch mini-map (skeleton+hubs+next) — agents read content.
/// - **Dense**: full report in text — for smart agents / clients that drop structuredContent.
/// - **structuredContent** always carries the full machine report either way.
pub fn orchestrate_content_summary(
    st: Option<&StructuredReport>,
    mermaid: Option<&str>,
    detail: ContentDetail,
) -> String {
    let Some(st) = st else {
        return "Orchestrate completed; no structured report available.".to_string();
    };
    match detail {
        ContentDetail::Compact => orchestrate_content_compact(st),
        ContentDetail::Dense => orchestrate_content_dense(st, mermaid),
    }
}

