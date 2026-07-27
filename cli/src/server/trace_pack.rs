//! Trace dossier packer: token/char budget + side guarantees.
//!
//! Chief-of-staff packing for Trace: never erase a non-empty L1 side while budget
//! remains; hard ceiling is only a fuse. Score ranks *within* fill order, not a
//! global fight that zeroes callees under high fan-in callers.

use crate::server::dto::CallerCallee;
use code_graph::CodeGraph;

/// ~4 chars per token (no tokenizer dependency).
/// Short/long share honesty (omitted counts); only sample size changes.
pub const SHORT_CHAR_BUDGET: usize = 4_000;
pub const LONG_CHAR_BUDGET: usize = 20_000;
/// Absolute max list rows (callers+callees) — fuse only.
pub const SHORT_HARD_CEILING: usize = 16;
pub const LONG_HARD_CEILING: usize = 72;


/// Soft I4 sample ranking strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SampleMode {
    #[default]
    Score,
    /// Stronger parent-dir diversity (fewer rows per directory).
    Diverse,
}

impl SampleMode {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("diverse") | Some("diversity") => Self::Diverse,
            _ => Self::Score,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Score => "score",
            Self::Diverse => "diverse",
        }
    }

    pub fn max_per_parent_dir(self) -> usize {
        match self {
            Self::Score => MAX_PER_PARENT_DIR_SCORE,
            Self::Diverse => MAX_PER_PARENT_DIR_DIVERSE,
        }
    }
}

/// Cap agent `sample_offset` (avoid huge skips on mega-hubs).
pub const MAX_SAMPLE_OFFSET: usize = 500;
/// Cap `exclude_symbols` list length.
pub const MAX_EXCLUDE_SYMBOLS: usize = 64;
const MAX_PER_PARENT_DIR_SCORE: usize = 3;
const MAX_PER_PARENT_DIR_DIVERSE: usize = 1;

#[derive(Debug, Clone, Copy)]
pub struct TracePackConfig {
    /// Char budget for packed list lines (name+path estimates).
    pub char_budget: usize,
    /// Max total neighbor rows (safety fuse).
    pub hard_ceiling: usize,
    /// Prefer packing callees before callers after min-1 guarantees (Trace into fn).
    pub callees_first: bool,
    /// Skip N ranked candidates (per side) after exclude, before pack.
    pub sample_offset: usize,
    pub sample_mode: SampleMode,
}

impl Default for TracePackConfig {
    fn default() -> Self {
        Self::short(20)
    }
}

impl TracePackConfig {
    /// Default / **short** sample (orient, pin, bridges).
    pub fn short(max_context_blocks: usize) -> Self {
        Self {
            char_budget: SHORT_CHAR_BUDGET,
            hard_ceiling: SHORT_HARD_CEILING.max(max_context_blocks.min(24)),
            callees_first: true,
            sample_offset: 0,
            sample_mode: SampleMode::Score,
        }
    }

    /// **Long** sample (edit planning under a pin) — larger but still fused.
    pub fn long(max_context_blocks: usize) -> Self {
        Self {
            char_budget: LONG_CHAR_BUDGET,
            hard_ceiling: LONG_HARD_CEILING.max(max_context_blocks.saturating_mul(3)),
            callees_first: true,
            sample_offset: 0,
            sample_mode: SampleMode::Score,
        }
    }

    pub fn for_detail(long: bool, max_context_blocks: usize) -> Self {
        if long {
            Self::long(max_context_blocks)
        } else {
            Self::short(max_context_blocks)
        }
    }

    pub fn with_window(mut self, offset: usize, mode: SampleMode) -> Self {
        self.sample_offset = offset.min(MAX_SAMPLE_OFFSET);
        self.sample_mode = mode;
        self
    }

    // TODO(optional): `unlimited` pack — only if dogfood shows long under tight
    // scope_paths still starves a real agent job. Prefer bumping LONG_* first.
}

/// Telemetry for Soft I4 sample window (offset / exclude / mode).
#[derive(Debug, Clone, Default)]
pub struct SampleWindowMeta {
    pub sample_offset: usize,
    pub sample_mode: SampleMode,
    pub exclude_count: usize,
    /// Ranked candidates after exclude (callers side) before offset.
    pub callers_ranked: usize,
    pub callees_ranked: usize,
    /// Offset past end or empty after exclude while warehouse non-empty.
    pub sample_window_exhausted: bool,
}

#[derive(Clone)]
pub struct TracePack {
    pub callers: Vec<CallerCallee>,
    pub callees: Vec<CallerCallee>,
    pub callers_total: usize,
    pub callees_total: usize,
    pub chars_used: usize,
    /// Why we stopped packing (if truncated).
    pub truncation_reason: Option<&'static str>,
}

impl TracePack {
    pub fn callers_omitted(&self) -> usize {
        self.callers_total.saturating_sub(self.callers.len())
    }
    pub fn callees_omitted(&self) -> usize {
        self.callees_total.saturating_sub(self.callees.len())
    }
}

/// Approximate dense list line cost for one neighbor.
pub fn neighbor_line_chars(cc: &CallerCallee) -> usize {
    let lang = cc.lang.as_deref().map(|s| s.len() + 3).unwrap_or(0);
    let cluster = cc.cluster.as_deref().map(|s| s.len() + 3).unwrap_or(0);
    8 + cc.name.len() + cc.file.len() + lang + cluster + 12
}

/// Resolve dossier row → score via **name_index** (O(hits)), never `nodes.values()`.
/// Monster warehouses (gecko 4.8M): sort_by used to re-scan all nodes per compare → multi-second.
fn score_for(graph: &CodeGraph, cc: &CallerCallee) -> f64 {
    graph
        .blocks_for_name(&cc.name)
        .into_iter()
        .filter(|b| b.start_line == cc.line)
        .map(|b| b.score)
        .fold(0.0f64, |a, s| a.max(s))
}

struct PackState {
    out_callers: Vec<CallerCallee>,
    out_callees: Vec<CallerCallee>,
    chars: usize,
    trunc: Option<&'static str>,
    hard_ceiling: usize,
    char_budget: usize,
}

impl PackState {
    fn try_push(&mut self, cc: CallerCallee, as_callee: bool) -> bool {
        if self.out_callers.len() + self.out_callees.len() >= self.hard_ceiling {
            self.trunc = Some("hard_ceiling");
            return false;
        }
        let cost = neighbor_line_chars(&cc);
        let side_empty = if as_callee {
            self.out_callees.is_empty()
        } else {
            self.out_callers.is_empty()
        };
        // Min-1: allow first on a side even if over budget, as long as budget not fully spent
        // on the other side alone without room for a single line.
        if self.chars + cost > self.char_budget {
            if !(side_empty && self.chars < self.char_budget) {
                self.trunc = Some("token_budget");
                return false;
            }
        }
        self.chars += cost;
        if as_callee {
            self.out_callees.push(cc);
        } else {
            self.out_callers.push(cc);
        }
        true
    }
}

/// True when neighbor `file` sits under any repo-relative scope prefix.
///
/// Hub UX: with `scope_paths` set, pack in-scope sample first so God-module reverse
/// does not fill the dossier with unrelated global callers.
pub fn neighbor_in_scope(file: &str, scope_prefixes: &[String]) -> bool {
    if scope_prefixes.is_empty() {
        return true;
    }
    let f = file.replace('\\', "/");
    let f = f.trim_start_matches("./");
    for raw in scope_prefixes {
        let owned = raw.replace('\\', "/");
        let p = owned
            .trim_start_matches("./")
            .trim_end_matches('/');
        if p.is_empty() || p == "." {
            return true;
        }
        if f == p
            || f.starts_with(&format!("{p}/"))
            || f.contains(&format!("/{p}/"))
            || f.ends_with(&format!("/{p}"))
            || (p.contains('.') && f.ends_with(p))
        {
            return true;
        }
    }
    false
}

/// Cheap path testish signal for pack ranking (no BlockInfo required).
pub fn is_testish_neighbor_file(file: &str) -> bool {
    let f = file.replace('\\', "/").to_ascii_lowercase();
    f.contains("/tests/")
        || f.contains("/test/")
        || f.contains("/benches/")
        || f.contains("/benchmarks/")
        || f.contains("/__tests__/")
        || f.contains("/fixtures/")
        || f.contains("_test.")
        || f.ends_with("_test.rs")
        || f.ends_with("_test.go")
        || f.ends_with("_test.py")
        || f.ends_with(".test.ts")
        || f.ends_with(".test.js")
        || f.ends_with("_spec.rs")
}

fn parent_dir_key(file: &str) -> String {
    let f = file.replace('\\', "/");
    match f.rfind('/') {
        Some(i) => f[..i].to_string(),
        None => String::new(),
    }
}

fn sort_for_pack(graph: &CodeGraph, items: &mut [CallerCallee], scope_prefixes: &[String]) {
    let scores: Vec<f64> = items.iter().map(|cc| score_for(graph, cc)).collect();
    let in_scope: Vec<i32> = items
        .iter()
        .map(|cc| i32::from(neighbor_in_scope(&cc.file, scope_prefixes)))
        .collect();
    let prod: Vec<i32> = items
        .iter()
        .map(|cc| i32::from(!is_testish_neighbor_file(&cc.file)))
        .collect();
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        // In-scope first when agent pinned a domain (de-god sample).
        in_scope[b]
            .cmp(&in_scope[a])
            // Prefer non-test paths for edit-map quality (rank polish).
            .then_with(|| prod[b].cmp(&prod[a]))
            .then_with(|| {
                scores[b]
                    .partial_cmp(&scores[a])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| items[a].name.cmp(&items[b].name))
            .then_with(|| a.cmp(&b))
    });
    let reordered: Vec<CallerCallee> = order.into_iter().map(|i| items[i].clone()).collect();
    for (slot, cc) in items.iter_mut().zip(reordered) {
        *slot = cc;
    }
}

/// Normalize exclude list: trim, drop empty, dedupe, cap length.
pub fn normalize_exclude_symbols(raw: Option<&[String]>) -> Vec<String> {
    let mut out = Vec::new();
    let Some(list) = raw else {
        return out;
    };
    for s in list {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        if out.iter().any(|e: &String| e == t) {
            continue;
        }
        out.push(t.to_string());
        if out.len() >= MAX_EXCLUDE_SYMBOLS {
            break;
        }
    }
    out
}

pub fn clamp_sample_offset(raw: Option<u32>) -> usize {
    raw.map(|n| (n as usize).min(MAX_SAMPLE_OFFSET)).unwrap_or(0)
}

fn name_matches_exclude(name: &str, exclude: &[String]) -> bool {
    exclude.iter().any(|e| {
        name == e.as_str() || name.ends_with(&format!("::{e}"))
    })
}

fn filter_excluded(items: Vec<CallerCallee>, exclude: &[String]) -> Vec<CallerCallee> {
    if exclude.is_empty() {
        return items;
    }
    items
        .into_iter()
        .filter(|c| !name_matches_exclude(&c.name, exclude))
        .collect()
}

/// Top parent-dir facets from reverse callers (repo-relative, for `suggested_scopes`).
///
/// Uses hop≤1 when present; falls back to all rows. Facet = first 1–2 path segments
/// with trailing `/` (e.g. `cli/src/`).
pub fn caller_dir_facets(callers: &[CallerCallee], max: usize) -> Vec<String> {
    if max == 0 || callers.is_empty() {
        return Vec::new();
    }
    let mut counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let hop1: Vec<&CallerCallee> = callers.iter().filter(|c| c.hop <= 1).collect();
    let iter: Box<dyn Iterator<Item = &CallerCallee>> = if hop1.is_empty() {
        Box::new(callers.iter())
    } else {
        Box::new(hop1.into_iter())
    };
    for c in iter {
        if let Some(facet) = path_to_scope_facet(&c.file) {
            *counts.entry(facet).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().take(max).map(|(s, _)| s).collect()
}

/// Repo-relative scope pin from a file path (1–2 segments + `/`).
///
/// Never emits host prefixes (`home/…`, absolute `/projects/…`). Prefers
/// anchors like `src/`, `cli/`, `code_graph/` when the display path is absolute.
pub fn path_to_scope_facet(file: &str) -> Option<String> {
    let f = file.replace('\\', "/");
    let f = f.trim_start_matches("./");
    // Absolute or host-shaped → peel to a repo-ish relative tail.
    let f = if f.starts_with('/') || f.starts_with("home/") || f.starts_with("Users/") {
        const ANCHORS: &[&str] = &[
            "/cli/",
            "/code_graph/",
            "/src/",
            "/crates/",
            "/lib/",
            "/pkg/",
            "/internal/",
            "/cmd/",
            "/app/",
        ];
        let mut rel: Option<&str> = None;
        for a in ANCHORS {
            if let Some(i) = f.find(a) {
                // keep without leading slash: cli/... or src/...
                rel = Some(f[i + 1..].trim_start_matches('/'));
                break;
            }
        }
        rel?
    } else {
        f.trim_start_matches('/')
    };
    if f.is_empty() {
        return None;
    }
    // Reject residual host fragments.
    if f.starts_with("home/") || f.starts_with("Users/") || f.starts_with("projects/") {
        return None;
    }
    let parts: Vec<&str> = f.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    // Drop filename if last segment looks like a file.
    let dirs: Vec<&str> = if parts.last().map(|p| p.contains('.')).unwrap_or(false) {
        parts[..parts.len() - 1].to_vec()
    } else {
        parts
    };
    if dirs.is_empty() {
        return None;
    }
    let take = dirs.len().min(2);
    Some(format!("{}/", dirs[..take].join("/")))
}

/// Build config from request-style window fields.
pub fn window_from_req(
    long: bool,
    max_blocks: usize,
    sample_offset: Option<u32>,
    sample_mode: Option<&str>,
) -> TracePackConfig {
    TracePackConfig::for_detail(long, max_blocks).with_window(
        clamp_sample_offset(sample_offset),
        SampleMode::parse(sample_mode),
    )
}

/// Pack L1 callers/callees into a dossier under char budget + hard ceiling.
///
/// `scope_prefixes`: when non-empty, in-scope neighbors are packed before out-of-scope
/// (warehouse totals still count all; sample becomes scope-shaped).
///
/// `focus_names`: hop continuity (Soft I4) — if a name is a real CALL parent in
/// `callers`, force-include it at the front of the sample after packing.
///
/// `exclude_names`: drop from ranked window before offset/pack (warehouse totals unchanged).
///
/// Returns `(pack, focus_injected, sample_window_meta)`.
pub fn pack_trace_neighbors_focus(
    callers: Vec<CallerCallee>,
    callees: Vec<CallerCallee>,
    graph: &CodeGraph,
    cfg: TracePackConfig,
    scope_prefixes: &[String],
    focus_names: &[String],
    exclude_names: &[String],
) -> (TracePack, Vec<String>, SampleWindowMeta) {
    let callers_total = callers.len();
    let callees_total = callees.len();
    let full_callers = callers.clone();
    let mut callers = filter_excluded(callers, exclude_names);
    let mut callees = filter_excluded(callees, exclude_names);
    sort_for_pack(graph, &mut callers, scope_prefixes);
    sort_for_pack(graph, &mut callees, scope_prefixes);

    let callers_ranked = callers.len();
    let callees_ranked = callees.len();
    let off = cfg.sample_offset.min(MAX_SAMPLE_OFFSET);
    if off > 0 {
        if off < callers.len() {
            callers = callers.split_off(off);
        } else {
            callers.clear();
        }
        if off < callees.len() {
            callees = callees.split_off(off);
        } else {
            callees.clear();
        }
    }
    let sample_window_exhausted = (callers_total > 0 || callees_total > 0)
        && callers.is_empty()
        && callees.is_empty()
        && (off > 0 || !exclude_names.is_empty());

    let max_per_dir = cfg.sample_mode.max_per_parent_dir();
    let mut st = PackState {
        out_callers: Vec::new(),
        out_callees: Vec::new(),
        chars: 0,
        trunc: None,
        hard_ceiling: cfg.hard_ceiling,
        char_budget: cfg.char_budget,
    };

    let mut ci = 0usize;
    let mut ei = 0usize;
    let mut caller_dir_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    let mut push_caller_diverse = |st: &mut PackState, cc: CallerCallee| -> bool {
        let key = parent_dir_key(&cc.file);
        let n = caller_dir_counts.get(&key).copied().unwrap_or(0);
        // Diversity: avoid filling reverse sample with one God module.
        if n >= max_per_dir && !key.is_empty() {
            return false; // skip this row; try later neighbors
        }
        if st.try_push(cc, false) {
            *caller_dir_counts.entry(key).or_insert(0) += 1;
            true
        } else {
            false
        }
    };

    // Phase A: min-1 per non-empty side.
    if ei < callees.len() {
        let cc = callees[ei].clone();
        ei += 1;
        let _ = st.try_push(cc, true);
    }
    if ci < callers.len() {
        let cc = callers[ci].clone();
        ci += 1;
        let _ = push_caller_diverse(&mut st, cc);
    }

    // Phase B: round-robin fill (start with callees if preferred).
    let mut prefer_callee = cfg.callees_first;
    let mut caller_skips = 0usize;
    loop {
        if st.out_callers.len() + st.out_callees.len() >= cfg.hard_ceiling {
            st.trunc.get_or_insert("hard_ceiling");
            break;
        }
        let can_c = ci < callers.len();
        let can_e = ei < callees.len();
        if !can_c && !can_e {
            break;
        }
        let pick_callee = match (can_e, can_c) {
            (true, false) => true,
            (false, true) => false,
            (true, true) => prefer_callee,
            (false, false) => break,
        };
        prefer_callee = !prefer_callee;

        if pick_callee {
            let cc = callees[ei].clone();
            ei += 1;
            if !st.try_push(cc, true) {
                if can_c {
                    // fall through to try a caller
                    let cc = callers[ci].clone();
                    ci += 1;
                    if !push_caller_diverse(&mut st, cc) {
                        caller_skips += 1;
                        if caller_skips > callers.len() {
                            break;
                        }
                    }
                } else {
                    break;
                }
            }
        } else {
            let cc = callers[ci].clone();
            ci += 1;
            if !push_caller_diverse(&mut st, cc) {
                caller_skips += 1;
                // diversity skip is not terminal — continue scanning callers
                if ci >= callers.len() && !can_e {
                    break;
                }
                if st.trunc.is_some() {
                    break;
                }
                continue;
            }
        }
    }

    if (ci < callers.len() || ei < callees.len()) && st.trunc.is_none() {
        st.trunc = Some("token_budget");
    }

    let mut pack = TracePack {
        callers: st.out_callers,
        callees: st.out_callees,
        callers_total,
        callees_total,
        chars_used: st.chars,
        truncation_reason: st.trunc,
    };
    let injected = inject_focus_callers(&mut pack, &full_callers, focus_names, cfg.hard_ceiling);
    let meta = SampleWindowMeta {
        sample_offset: off,
        sample_mode: cfg.sample_mode,
        exclude_count: exclude_names.len(),
        callers_ranked,
        callees_ranked,
        sample_window_exhausted: sample_window_exhausted && pack.callers.is_empty() && pack.callees.is_empty(),
    };
    (pack, injected, meta)
}

/// Concrete next re-pull when sample is truncated / hub (P1 pin mitigation).
pub fn sample_window_next_action(
    callers_omitted: usize,
    sample_offset: usize,
    shown: usize,
    sample_mode: SampleMode,
    facets: &[String],
    blank_scope: bool,
    exhausted: bool,
) -> Option<String> {
    if callers_omitted == 0 && !exhausted {
        return None;
    }
    if exhausted {
        if let Some(f) = facets.first() {
            return Some(format!(
                "sample window exhausted — pin scope_paths=[\"{f}\"] or clear exclude_symbols / lower sample_offset"
            ));
        }
        return Some(
            "sample window exhausted — pin scope_paths to a product dir, or clear exclude_symbols / lower sample_offset"
                .into(),
        );
    }
    // Prefer one concrete action (agents ignore multi-option essays).
    if blank_scope {
        if let Some(f) = facets.first() {
            return Some(format!(
                "wrong sample? pin scope_paths=[\"{f}\"] (or sample_offset={} / sample_mode=diverse / exclude_symbols from this sample)",
                sample_offset + shown.max(1)
            ));
        }
    }
    if sample_mode == SampleMode::Score && callers_omitted > shown {
        return Some(format!(
            "wrong sample? sample_offset={} or sample_mode=diverse or exclude_symbols=[names above]",
            sample_offset + shown.max(1)
        ));
    }
    Some(format!(
        "wrong sample? sample_offset={} or exclude_symbols=[names in this sample] or narrower scope_paths",
        sample_offset + shown.max(1)
    ))
}

/// Force-include hop-continuity parents in the callers **sample**.
///
/// Only injects if `focus` appears as a real row in `full_callers` (true CALL parent
/// of ★ in the pre-pack neighborhood). Moves to front if already sampled.
/// Returns names successfully focused.
pub fn inject_focus_callers(
    pack: &mut TracePack,
    full_callers: &[CallerCallee],
    focus_names: &[String],
    hard_ceiling: usize,
) -> Vec<String> {
    let mut injected = Vec::new();
    for raw in focus_names {
        let want = raw.trim();
        if want.is_empty() {
            continue;
        }
        // Already in sample → move to front.
        if let Some(i) = pack
            .callers
            .iter()
            .position(|c| c.name == want || c.name.ends_with(&format!("::{want}")))
        {
            let cc = pack.callers.remove(i);
            pack.callers.insert(0, cc);
            if !injected.iter().any(|s: &String| s == want) {
                injected.push(want.to_string());
            }
            continue;
        }
        // Prefer hop≤1 match by exact name.
        let found = full_callers
            .iter()
            .find(|c| c.name == want && c.hop <= 1)
            .or_else(|| full_callers.iter().find(|c| c.name == want))
            .or_else(|| {
                full_callers
                    .iter()
                    .find(|c| c.name.ends_with(&format!("::{want}")) && c.hop <= 1)
            })
            .cloned();
        let Some(cc) = found else {
            continue;
        };
        // Make room under hard ceiling (drop tail callers, never drop all callees).
        while pack.callers.len() + pack.callees.len() >= hard_ceiling && !pack.callers.is_empty() {
            pack.callers.pop();
        }
        if pack.callers.len() + pack.callees.len() >= hard_ceiling {
            // still full of callees only — drop a callee tail
            if !pack.callees.is_empty() {
                pack.callees.pop();
            }
        }
        pack.callers.insert(0, cc);
        injected.push(want.to_string());
    }
    injected
}

/// Collect focus names from request-style options (single + multi).
pub fn focus_names_from_parts(
    focus_symbol: Option<&str>,
    focus_symbols: Option<&[String]>,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(s) = focus_symbol {
        let t = s.trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
    }
    if let Some(list) = focus_symbols {
        for s in list {
            let t = s.trim();
            if !t.is_empty() && !out.iter().any(|e| e == t) {
                out.push(t.to_string());
            }
        }
    }
    out
}

/// Names requested as focus but not present in the callers sample (not a CALL parent).
pub fn focus_missed(focus_names: &[String], injected: &[String]) -> Vec<String> {
    focus_names
        .iter()
        .filter(|f| !injected.iter().any(|i| i == *f))
        .cloned()
        .collect()
}

/// Stamp Soft I4 focus telemetry (memo hydrate / early-exit).
///
/// Does not invent parents — only reports which requested focus names already
/// appear in the callers sample (re-order is live-pack only).
pub fn stamp_focus_telemetry(
    telemetry: &mut serde_json::Value,
    callers: &[CallerCallee],
    focus_names: &[String],
) {
    if focus_names.is_empty() {
        return;
    }
    let mut injected = Vec::new();
    for want in focus_names {
        let hit = callers.iter().any(|c| {
            c.name == *want || c.name.ends_with(&format!("::{want}"))
        });
        if hit {
            injected.push(want.clone());
        }
    }
    let missed = focus_missed(focus_names, &injected);
    if let Some(obj) = telemetry.as_object_mut() {
        if let Some(first) = focus_names.first() {
            obj.insert("focus_symbol".into(), serde_json::json!(first));
        }
        obj.insert("focus_symbols".into(), serde_json::json!(focus_names));
        obj.insert("focus_injected".into(), serde_json::json!(injected));
        obj.insert("focus_missed".into(), serde_json::json!(missed));
    }
}

/// Stamp Soft I4 sample-window fields on memo hydrate (request is source of truth).
pub fn stamp_sample_window_telemetry(
    telemetry: &mut serde_json::Value,
    sample_offset: Option<u32>,
    sample_mode: Option<&str>,
    exclude_symbols: Option<&[String]>,
) {
    let off = clamp_sample_offset(sample_offset);
    let mode = SampleMode::parse(sample_mode);
    let excl = normalize_exclude_symbols(exclude_symbols);
    if let Some(obj) = telemetry.as_object_mut() {
        obj.insert("sample_offset".into(), serde_json::json!(off));
        obj.insert("sample_mode".into(), serde_json::json!(mode.as_str()));
        obj.insert("exclude_symbols".into(), serde_json::json!(excl));
        obj.insert("exclude_count".into(), serde_json::json!(excl.len()));
    }
}

/// Resolve Trace blast depth: hard cap **2**. Optional `expand_hops` (1–2) overrides `depth`.
/// Returns `(effective_depth, telemetry_fields)`.
pub fn resolve_trace_depth(req_depth: usize, expand_hops: Option<u8>) -> (usize, serde_json::Value) {
    let mut d = req_depth.max(1);
    let mut meta = serde_json::Map::new();
    if let Some(eh) = expand_hops {
        let req_h = eh as usize;
        let eff = req_h.clamp(1, 2);
        d = eff;
        meta.insert("expand_hops_requested".into(), req_h.into());
        meta.insert("expand_hops_effective".into(), eff.into());
        meta.insert("expand_hops_capped".into(), (req_h > 2).into());
    }
    d = d.min(2);
    meta.insert("depth_hard_cap".into(), 2.into());
    (d, serde_json::Value::Object(meta))
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph::CodeGraph;

    fn cc(name: &str, line: usize) -> CallerCallee {
        CallerCallee {
            name: name.into(),
            file: "a.c".into(),
            line,
            hop: 1,
            lang: Some("c".into()),
            cluster: Some("core:c".into()),
            relation: None,
            cite: None,
            why: None,
        }
    }

    #[test]
    fn high_fan_in_keeps_callee() {
        let g = CodeGraph::new();
        let callers: Vec<_> = (0..40).map(|i| cc(&format!("caller_{i}"), i + 1)).collect();
        let callees = vec![cc("_sdsnewlen", 98)];
        let pack = pack_trace_neighbors_focus(
            callers,
            callees,
            &g,
            TracePackConfig {
                char_budget: 2_000,
                hard_ceiling: 20,
                callees_first: true,
                ..TracePackConfig::default()
            },
            &[],
            &[],
            &[],
        )
        .0;
        assert_eq!(pack.callees_total, 1);
        assert!(
            !pack.callees.is_empty(),
            "must keep callee under fan-in callers"
        );
        assert_eq!(pack.callees[0].name, "_sdsnewlen");
        assert!(pack.callers_omitted() > 0);
    }

    #[test]
    fn empty_callee_side_ok() {
        let g = CodeGraph::new();
        let pack = pack_trace_neighbors_focus(
            vec![cc("a", 1), cc("b", 2)],
            vec![],
            &g,
            TracePackConfig::default(),
            &[],
            &[],
            &[],
        )
        .0;
        assert!(pack.callees.is_empty());
        assert_eq!(pack.callers.len(), 2);
    }

    #[test]
    fn hard_ceiling_fuse() {
        let g = CodeGraph::new();
        let callers: Vec<_> = (0..100).map(|i| cc(&format!("c{i}"), i + 1)).collect();
        let callees: Vec<_> = (0..100).map(|i| cc(&format!("e{i}"), i + 1)).collect();
        let pack = pack_trace_neighbors_focus(
            callers,
            callees,
            &g,
            TracePackConfig {
                char_budget: 1_000_000,
                hard_ceiling: 10,
                callees_first: true,
                ..TracePackConfig::default()
            },
            &[],
            &[],
            &[],
        )
        .0;
        assert_eq!(pack.callers.len() + pack.callees.len(), 10);
        assert_eq!(pack.truncation_reason, Some("hard_ceiling"));
        assert!(!pack.callees.is_empty());
        assert!(!pack.callers.is_empty());
    }

    #[test]
    fn scope_prefixes_pack_in_scope_first() {
        let g = CodeGraph::new();
        let mut far = cc("far_caller", 1);
        far.file = "other/crate/src/lib.rs".into();
        let mut near = cc("near_caller", 2);
        near.file = "auth/src/login.rs".into();
        let pack = pack_trace_neighbors_focus(
            vec![far, near],
            vec![],
            &g,
            TracePackConfig {
                char_budget: 500,
                hard_ceiling: 1,
                callees_first: true,
                ..TracePackConfig::default()
            },
            &["auth/".into()],
            &[],
            &[],
        )
        .0;
        assert_eq!(pack.callers.len(), 1);
        assert_eq!(pack.callers[0].name, "near_caller");
        assert!(neighbor_in_scope("auth/src/login.rs", &["auth/".into()]));
        assert!(!neighbor_in_scope(
            "other/crate/src/lib.rs",
            &["auth/".into()]
        ));
    }

    #[test]
    fn focus_injects_parent_to_front_of_sample() {
        let g = CodeGraph::new();
        let mut focus = cc("run_http_proxy", 10);
        focus.file = "cli/src/mcp/mod.rs".into();
        let mut callers: Vec<_> = (0..30)
            .map(|i| {
                let mut c = cc(&format!("noise_{i}"), i + 1);
                c.file = format!("other/mod_{i}.rs");
                c
            })
            .collect();
        callers.push(focus);
        let (pack, injected, _) = pack_trace_neighbors_focus(
            callers,
            vec![cc("leaf", 1)],
            &g,
            TracePackConfig {
                char_budget: 4_000,
                hard_ceiling: 8,
                callees_first: true,
                ..TracePackConfig::default()
            },
            &[],
            &["run_http_proxy".into()],
            &[],
        );
        assert_eq!(injected, vec!["run_http_proxy".to_string()]);
        assert_eq!(pack.callers[0].name, "run_http_proxy");
        assert!(pack.callers.len() + pack.callees.len() <= 8);
    }

    #[test]
    fn resolve_trace_depth_caps_at_two() {
        let (d, m) = resolve_trace_depth(9, Some(5));
        assert_eq!(d, 2);
        assert_eq!(m["expand_hops_capped"], true);
        let (d2, _) = resolve_trace_depth(1, None);
        assert_eq!(d2, 1);
    }

    #[test]
    fn sample_offset_skips_ranked_callers() {
        let g = CodeGraph::new();
        // Identical scores → stable name order: a0, a1, a2, ...
        let callers: Vec<_> = (0..20)
            .map(|i| {
                let mut c = cc(&format!("a{i:02}"), i + 1);
                c.file = format!("mod/f{i}.rs");
                c
            })
            .collect();
        let cfg0 = TracePackConfig {
            char_budget: 50_000,
            hard_ceiling: 5,
            callees_first: true,
            sample_offset: 0,
            sample_mode: SampleMode::Score,
        };
        let (p0, _, m0) = pack_trace_neighbors_focus(
            callers.clone(),
            vec![],
            &g,
            cfg0,
            &[],
            &[],
            &[],
        );
        let cfg5 = TracePackConfig {
            sample_offset: 5,
            ..cfg0
        };
        let (p5, _, m5) = pack_trace_neighbors_focus(
            callers,
            vec![],
            &g,
            cfg5,
            &[],
            &[],
            &[],
        );
        assert_eq!(m0.sample_offset, 0);
        assert_eq!(m5.sample_offset, 5);
        assert!(!p0.callers.is_empty() && !p5.callers.is_empty());
        assert_ne!(
            p0.callers[0].name, p5.callers[0].name,
            "offset should change first sampled caller"
        );
        assert_eq!(p0.callers_total, p5.callers_total);
    }

    #[test]
    fn exclude_symbols_drops_name_from_sample() {
        let g = CodeGraph::new();
        let callers = vec![cc("keep_me", 1), cc("drop_me", 2), cc("also_keep", 3)];
        let (pack, _, meta) = pack_trace_neighbors_focus(
            callers,
            vec![],
            &g,
            TracePackConfig {
                char_budget: 50_000,
                hard_ceiling: 10,
                callees_first: true,
                ..TracePackConfig::default()
            },
            &[],
            &[],
            &["drop_me".into()],
        );
        assert_eq!(meta.exclude_count, 1);
        assert!(pack.callers.iter().all(|c| c.name != "drop_me"));
        assert_eq!(pack.callers_total, 3);
        assert!(pack.callers.iter().any(|c| c.name == "keep_me"));
    }

    #[test]
    fn path_to_scope_facet_two_segments() {
        assert_eq!(
            path_to_scope_facet("cli/src/server/mod.rs").as_deref(),
            Some("cli/src/")
        );
        assert_eq!(path_to_scope_facet("main.rs"), None);
        assert_eq!(
            path_to_scope_facet("/home/user/projects/example-repo/cli/src/server/mod.rs")
                .as_deref(),
            Some("cli/src/")
        );
        assert_eq!(path_to_scope_facet("home/user/projects/foo.rs"), None);
    }
}
