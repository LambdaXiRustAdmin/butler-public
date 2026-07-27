//! Trace path memory — remember the **tour**, not only the building.
//!
//! Warehouse (`name_index`, edges) is the building. This module memoizes the result of a
//! Trace/Find walk (preferred seed + callers/callees + locations) under:
//!
//! ```text
//! TraceKey = hash(project, goal, symbol, depth, scopes, focus, graph_epoch)
//! graph_epoch = CodeGraph::current_trace_epoch()  // O(1) when warm
//! ```
//!
//! Hit → serve without seed ranking + BFS. Epoch change (dirty inventory) → natural miss.
//!
//! **Early Exit**: hot RAM `HashMap` (disk only on first load + STORE). Lookup is µs-class;
//! pair with a front-door check in `context_engine` before `scoped_block_refs` / JIT.

use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use code_graph::snooper::CodeGraph;
use serde::{Deserialize, Serialize};

use super::dto::{
    CallerCallee, ContextRequest, StateInfo, StructuredReport, SymbolLocation, TargetInfo,
};

/// Bump when seed ranking / Trace packing semantics change so disk memo cannot
/// re-serve a wrong ★ (e.g. mozilla::Mutex → js Mutex.h before qualification fix).
/// Bump when Trace memo tour shape changes (P.4/P.5: bridges + blast_domain).
/// Bump (v4): seed integrity — ★ name must equal target Ident (no search_py→search_lib_dir).
/// Bump (v5): L2.2 Export bridge semantics (co-located twin / no glossary false bridges).
/// Bump (v6): Trace bridge neighbors not dropped by examples/ test-noise filter.
/// Bump (v7): L2.3 IPC dual-stack inventory + Go type_spec domain.
/// Bump (v8): IPC bridge live-augment on early exit (slim disk re-read).
/// Bump (v9): reverse CALL spine (`caller_path`) on Trace memo.
/// Bump (v10): I8 — never ★ call_expression; lift out-of-scope defs; call-only → miss.
/// Bump (v11): I9 — no out-of-scope lift past non-call pin hits; serious alts exclude NEVER/mod.
/// Bump (v12): I4 — keep CALL reverse parents (no peripheral drop); warehouse-honest omitted.
/// Bump (v13): T.2 collision multi-file + danger min-2; scope ./ and .. collapse.
/// Bump (v14): file-pin never lifts ★ out of scope (I9 after broader T.2).
/// Bump (v15): detail short|long pack budgets + mega-hub next_action.
/// Bump (v16): peer_callers segregated from hard CALL reverse (name_peer honesty).
/// Bump (v17): file-pin I9 — no module-shell steal; Go method CALL honesty.
/// Bump (v19): loc_fallback unique-def only (class L invent gate).
/// Bump (v20): Soft I4 — focus_symbol(s) + expand_hops in TraceKey (no cross-focus memo).
/// Bump (v21): Soft I4 sample window — offset / mode / exclude_symbols in TraceKey.
const MEMO_FORMAT: u32 = 21;
const MAX_ENTRIES: usize = 512;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoNeighbor {
    pub name: String,
    pub file: String,
    pub line: usize,
    /// 1 = direct edge from seed; 2+ = transitive. Missing in old memos → default 1.
    #[serde(default = "default_memo_hop")]
    pub hop: u8,
    pub lang: Option<String>,
    pub cluster: Option<String>,
    pub relation: Option<String>,
}

fn default_memo_hop() -> u8 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoLocation {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub end_line: Option<usize>,
    pub kind: String,
    pub preferred: bool,
    pub lang: Option<String>,
    pub cluster: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoTarget {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub definition: Option<String>,
    pub lang: Option<String>,
    pub cluster: Option<String>,
    pub seed_id: String,
}

/// Structured tour result (no markdown — re-render on hit).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceMemoPayload {
    pub format: u32,
    pub graph_epoch: u64,
    pub goal: String,
    pub symbol: String,
    pub target: MemoTarget,
    pub callers: Vec<MemoNeighbor>,
    pub callees: Vec<MemoNeighbor>,
    /// Reverse CALL spine (seed omitted). Default empty for pre-v9 memos.
    #[serde(default)]
    pub caller_path: Vec<MemoNeighbor>,
    /// Same-name peer reverse (`relation=name_peer`) — not CALL into ★. v16+.
    #[serde(default)]
    pub peer_callers: Vec<MemoNeighbor>,
    /// Typed interconnect neighbors (Export/Ipc/Twin). Default empty for old memos.
    #[serde(default)]
    pub bridge_callers: Vec<MemoNeighbor>,
    #[serde(default)]
    pub bridge_callees: Vec<MemoNeighbor>,
    /// `call` | `type_neighborhood`
    #[serde(default)]
    pub blast_domain: Option<String>,
    #[serde(default)]
    pub seed_kind: Option<String>,
    pub locations: Vec<MemoLocation>,
    pub suggested_scopes: Vec<String>,
    pub active_cluster: Option<String>,
    pub callers_omitted: usize,
    pub callees_omitted: usize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TraceMemoFile {
    format: u32,
    /// key → payload
    entries: HashMap<u64, TraceMemoPayload>,
    /// insertion order for crude LRU eviction
    order: Vec<u64>,
}

/// Per-project hot RAM slice (disk hydrated once).
#[derive(Debug, Default)]
struct HotRoot {
    entries: HashMap<u64, TraceMemoPayload>,
    order: Vec<u64>,
}

#[derive(Debug, Default)]
struct HotStore {
    by_root: HashMap<PathBuf, HotRoot>,
}

fn hot_store() -> &'static Mutex<HotStore> {
    static HOT: OnceLock<Mutex<HotStore>> = OnceLock::new();
    HOT.get_or_init(|| Mutex::new(HotStore::default()))
}

/// Fingerprint of warehouse structure for this root (O(1) when epoch cache is warm).
pub fn graph_epoch(graph: &CodeGraph) -> u64 {
    graph.current_trace_epoch()
}

/// Stable TraceKey for memo lookup.
pub fn make_trace_key(
    root: &str,
    goal: &str,
    symbol: &str,
    graph_epoch: u64,
    depth: usize,
    max_fan_out: usize,
    max_visited: usize,
    scope_paths: &Option<Vec<String>>,
    ignore_paths: &Option<Vec<String>>,
    detail: Option<&str>,
    focus_symbol: Option<&str>,
    focus_symbols: Option<&[String]>,
    expand_hops: Option<u8>,
    sample_offset: u32,
    sample_mode: Option<&str>,
    exclude_symbols: Option<&[String]>,
) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);
    goal.hash(&mut h);
    symbol.trim().hash(&mut h);
    graph_epoch.hash(&mut h);
    depth.hash(&mut h);
    max_fan_out.hash(&mut h);
    max_visited.hash(&mut h);
    detail.unwrap_or("").hash(&mut h);
    // Soft I4: focused Trace must not share a tour with unfocused same ★.
    focus_symbol.unwrap_or("").trim().hash(&mut h);
    expand_hops.unwrap_or(0).hash(&mut h);
    let mut foci: Vec<&str> = focus_symbols
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    foci.sort_unstable();
    for s in foci {
        s.hash(&mut h);
    }
    sample_offset.hash(&mut h);
    sample_mode.unwrap_or("").hash(&mut h);
    let mut excl: Vec<&str> = exclude_symbols
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    excl.sort_unstable();
    for s in excl {
        s.hash(&mut h);
    }
    let mut scopes: Vec<&str> = scope_paths
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    scopes.sort_unstable();
    for s in scopes {
        s.hash(&mut h);
    }
    let mut ignores: Vec<&str> = ignore_paths
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    ignores.sort_unstable();
    for s in ignores {
        s.hash(&mut h);
    }
    h.finish()
}

pub fn make_trace_key_from_req(
    root: &str,
    goal: &str,
    symbol: &str,
    graph_epoch: u64,
    req: &ContextRequest,
    max_fan_out: usize,
    max_visited: usize,
    max_depth: usize,
) -> u64 {
    // expand_hops overrides depth (same hard cap as live Trace pack).
    let (depth, _) =
        crate::server::trace_pack::resolve_trace_depth(req.depth, req.expand_hops);
    let depth = depth.min(max_depth);
    make_trace_key(
        root,
        goal,
        symbol,
        graph_epoch,
        depth,
        max_fan_out,
        max_visited,
        &req.scope_paths,
        &req.ignore_paths,
        req.detail.as_deref(),
        req.focus_symbol.as_deref(),
        req.focus_symbols.as_ref().map(|v| v.as_slice()),
        req.expand_hops,
        req.sample_offset.unwrap_or(0),
        req.sample_mode.as_deref(),
        req.exclude_symbols.as_ref().map(|v| v.as_slice()),
    )
}

fn memo_path(project_root: &Path) -> PathBuf {
    project_root.join(".butler/cache/trace_memo.json")
}

fn load_file(path: &Path) -> TraceMemoFile {
    let Ok(bytes) = fs::read(path) else {
        return TraceMemoFile {
            format: MEMO_FORMAT,
            entries: HashMap::new(),
            order: Vec::new(),
        };
    };
    match serde_json::from_slice::<TraceMemoFile>(&bytes) {
        Ok(f) if f.format == MEMO_FORMAT => f,
        _ => TraceMemoFile {
            format: MEMO_FORMAT,
            entries: HashMap::new(),
            order: Vec::new(),
        },
    }
}

fn save_file(path: &Path, file: &TraceMemoFile) {
    // path is `{root}/.butler/cache/trace_memo.json` — refuse nested src/examples/tests.
    if let Some(cache) = path.parent() {
        if let Some(butler) = cache.parent() {
            if let Some(root) = butler.parent() {
                if code_graph::snooper::butler_cache_write_forbidden_reason(root).is_some() {
                    return;
                }
            }
        }
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(file) {
        let _ = fs::write(path, bytes);
    }
}

fn root_key(project_root: &Path) -> PathBuf {
    // Stable identity: prefer canonical; fall back to as-given.
    project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf())
}

fn ensure_hot_root<'a>(store: &'a mut HotStore, project_root: &Path) -> &'a mut HotRoot {
    let key = root_key(project_root);
    if !store.by_root.contains_key(&key) {
        let file = load_file(&memo_path(project_root));
        store.by_root.insert(
            key.clone(),
            HotRoot {
                entries: file.entries,
                order: file.order,
            },
        );
    }
    store
        .by_root
        .get_mut(&key)
        .expect("hot root just inserted")
}

fn persist_root(project_root: &Path, hot: &HotRoot) {
    let file = TraceMemoFile {
        format: MEMO_FORMAT,
        entries: hot.entries.clone(),
        order: hot.order.clone(),
    };
    save_file(&memo_path(project_root), &file);
}

/// Lookup memo for key under project root. Epoch is re-checked against payload.
///
/// Hot path: in-process `HashMap` after first hydrate (no disk on repeat hits).
pub fn lookup(project_root: &Path, key: u64, expected_epoch: u64) -> Option<TraceMemoPayload> {
    let mut store = hot_store().lock().unwrap_or_else(|e| e.into_inner());
    let hot = ensure_hot_root(&mut store, project_root);
    let payload = hot.entries.get(&key)?.clone();
    if payload.graph_epoch != expected_epoch {
        return None;
    }
    Some(payload)
}

/// Store tour result (bounded LRU-ish eviction). Updates hot RAM + disk.
pub fn store(project_root: &Path, key: u64, payload: TraceMemoPayload) {
    let mut store = hot_store().lock().unwrap_or_else(|e| e.into_inner());
    let hot = ensure_hot_root(&mut store, project_root);
    if hot.entries.contains_key(&key) {
        hot.order.retain(|k| *k != key);
    }
    hot.order.push(key);
    hot.entries.insert(key, payload);
    while hot.entries.len() > MAX_ENTRIES {
        if let Some(old) = hot.order.first().copied() {
            hot.order.remove(0);
            hot.entries.remove(&old);
        } else {
            break;
        }
    }
    persist_root(project_root, hot);
}

pub fn neighbor_from_cc(c: &CallerCallee) -> MemoNeighbor {
    MemoNeighbor {
        name: c.name.clone(),
        file: c.file.clone(),
        line: c.line,
        hop: c.hop.max(1),
        lang: c.lang.clone(),
        cluster: c.cluster.clone(),
        relation: c.relation.clone(),
    }
}

pub fn cc_from_neighbor(n: &MemoNeighbor) -> CallerCallee {
    CallerCallee {
        name: n.name.clone(),
        file: n.file.clone(),
        line: n.line,
        hop: n.hop.max(1),
        lang: n.lang.clone(),
        cluster: n.cluster.clone(),
        relation: n.relation.clone(),
        cite: None, // memo does not store snippets; live Trace fills cite
        why: None,  // recomputed on hydrate when possible
    }
}

pub fn location_from_sym(l: &SymbolLocation) -> MemoLocation {
    MemoLocation {
        name: l.name.clone(),
        file: l.file.clone(),
        line: l.line,
        end_line: l.end_line,
        kind: l.kind.clone(),
        preferred: l.preferred,
        lang: l.lang.clone(),
        cluster: l.cluster.clone(),
    }
}

pub fn sym_from_location(l: &MemoLocation) -> SymbolLocation {
    SymbolLocation {
        name: l.name.clone(),
        file: l.file.clone(),
        line: l.line,
        end_line: l.end_line,
        kind: l.kind.clone(),
        preferred: l.preferred,
        lang: l.lang.clone(),
        cluster: l.cluster.clone(),
    }
}

/// Build a StructuredReport from memo + live edge-build state (fresh confidence banner).
pub fn report_from_memo(
    payload: &TraceMemoPayload,
    state: StateInfo,
    jit: &str,
    total_time_ms: u64,
    blocks_scanned: usize,
) -> (StructuredReport, Option<String>) {
    let callers: Vec<CallerCallee> = payload.callers.iter().map(cc_from_neighbor).collect();
    let callees: Vec<CallerCallee> = payload.callees.iter().map(cc_from_neighbor).collect();
    let peer_callers: Vec<CallerCallee> =
        payload.peer_callers.iter().map(cc_from_neighbor).collect();
    let bridge_callers: Vec<CallerCallee> =
        payload.bridge_callers.iter().map(cc_from_neighbor).collect();
    let bridge_callees: Vec<CallerCallee> =
        payload.bridge_callees.iter().map(cc_from_neighbor).collect();
    let locations: Vec<SymbolLocation> = payload.locations.iter().map(sym_from_location).collect();
    let target = TargetInfo {
        name: payload.target.name.clone(),
        file: payload.target.file.clone(),
        line: payload.target.line,
        definition: payload.target.definition.clone(),
        lang: payload.target.lang.clone(),
        cluster: payload.target.cluster.clone(),
    };
    let mermaid = mermaid_from_tour(&target, &callers, &callees, payload.callers_omitted, payload.callees_omitted);
    let blast_domain = payload
        .blast_domain
        .clone()
        .or_else(|| Some("call".into()));
    let telemetry = serde_json::json!({
        "trace_memo": true,
        "trace_memo_early_exit": jit == "early_exit",
        "graph_epoch": payload.graph_epoch,
        "seed_id": payload.target.seed_id,
        "blast_domain": blast_domain,
        "seed_kind": payload.seed_kind,
        "payload_blocks": 1 + callers.len() + callees.len() + peer_callers.len() + bridge_callers.len() + bridge_callees.len(),
        "blocks_scanned": blocks_scanned,
        "total_time_ms": total_time_ms,
        "callers_shown": callers.len(),
        "callees_shown": callees.len(),
        "peer_callers_shown": peer_callers.len(),
        "bridge_callers": bridge_callers.len(),
        "bridge_callees": bridge_callees.len(),
        "callers_omitted": payload.callers_omitted,
        "callees_omitted": payload.callees_omitted,
        "jit": jit,
    });
    let caller_path: Vec<CallerCallee> =
        payload.caller_path.iter().map(cc_from_neighbor).collect();
    let mut report = StructuredReport {
        state,
        error: None,
        target: Some(target),
        callers,
        callees,
        caller_path,
        peer_callers,
        bridge_callers,
        bridge_callees,
        blast_domain,
        seed_kind: payload.seed_kind.clone(),
        receipt: None,
        next_action: None,
        telemetry,
        suggested_scopes: payload.suggested_scopes.clone(),
        skeleton: None,
        hubs: None,
        module_resolved_from: None,
        module_interior_candidates: None,
        locations: if locations.is_empty() {
            None
        } else {
            Some(locations)
        },
        clusters: None,
        bridges: None,
        active_cluster: payload.active_cluster.clone(),
    };
    crate::server::orchestrate::attach_trace_receipt(&mut report);
    (report, Some(mermaid))
}

fn file_basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

fn sanitize_label(s: &str) -> String {
    s.replace('"', "'").chars().take(48).collect()
}

fn mermaid_from_tour(
    target: &TargetInfo,
    callers: &[CallerCallee],
    callees: &[CallerCallee],
    callers_omitted: usize,
    callees_omitted: usize,
) -> String {
    let mut m = String::from("graph LR\n");
    m.push_str(&format!(
        "    target[\"{} ({})\"]\n",
        sanitize_label(&target.name),
        sanitize_label(&file_basename(&target.file))
    ));
    const SHOW: usize = 12;
    for (i, c) in callers.iter().take(SHOW).enumerate() {
        m.push_str(&format!(
            "    c{i}[\"{} ({})\"] --> target\n",
            sanitize_label(&c.name),
            sanitize_label(&file_basename(&c.file))
        ));
    }
    if callers.len() > SHOW || callers_omitted > 0 {
        let extra = callers.len().saturating_sub(SHOW) + callers_omitted;
        if extra > 0 {
            m.push_str(&format!("    cnote[\"...and {extra} more callers\"]\n"));
        }
    }
    for (i, c) in callees.iter().take(SHOW).enumerate() {
        m.push_str(&format!(
            "    target --> e{i}[\"{} ({})\"]\n",
            sanitize_label(&c.name),
            sanitize_label(&file_basename(&c.file))
        ));
    }
    if callees.len() > SHOW || callees_omitted > 0 {
        let extra = callees.len().saturating_sub(SHOW) + callees_omitted;
        if extra > 0 {
            m.push_str(&format!("    enote[\"...and {extra} more callees\"]\n"));
        }
    }
    m
}

/// Build payload from a finished StructuredReport (tour complete).
pub fn payload_from_report(
    goal: &str,
    symbol: &str,
    graph_epoch: u64,
    seed_id: &str,
    report: &StructuredReport,
    callers_omitted: usize,
    callees_omitted: usize,
) -> Option<TraceMemoPayload> {
    let t = report.target.as_ref()?;
    Some(TraceMemoPayload {
        format: MEMO_FORMAT,
        graph_epoch,
        goal: goal.to_string(),
        symbol: symbol.to_string(),
        target: MemoTarget {
            name: t.name.clone(),
            file: t.file.clone(),
            line: t.line,
            definition: t.definition.clone(),
            lang: t.lang.clone(),
            cluster: t.cluster.clone(),
            seed_id: seed_id.to_string(),
        },
        callers: report.callers.iter().map(neighbor_from_cc).collect(),
        callees: report.callees.iter().map(neighbor_from_cc).collect(),
        caller_path: report.caller_path.iter().map(neighbor_from_cc).collect(),
        peer_callers: report.peer_callers.iter().map(neighbor_from_cc).collect(),
        bridge_callers: report.bridge_callers.iter().map(neighbor_from_cc).collect(),
        bridge_callees: report.bridge_callees.iter().map(neighbor_from_cc).collect(),
        blast_domain: report.blast_domain.clone(),
        seed_kind: report.seed_kind.clone(),
        locations: report
            .locations
            .as_ref()
            .map(|v| v.iter().map(location_from_sym).collect())
            .unwrap_or_default(),
        suggested_scopes: report.suggested_scopes.clone(),
        active_cluster: report.active_cluster.clone(),
        callers_omitted,
        callees_omitted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph::snooper::model::CodeGraph;

    #[test]
    fn epoch_changes_when_inventory_hash_changes() {
        let mut g = CodeGraph::new();
        g.file_hashes.insert("a.rs".into(), 1);
        let e1 = graph_epoch(&g);
        g.file_hashes.insert("a.rs".into(), 2);
        g.invalidate_trace_epoch();
        let e2 = graph_epoch(&g);
        assert_ne!(e1, e2);
    }

    #[test]
    fn epoch_is_cached_until_invalidate() {
        let mut g = CodeGraph::new();
        g.file_hashes.insert("a.rs".into(), 1);
        let e1 = g.current_trace_epoch();
        // Mutate without invalidate → still stale cached value (document contract).
        g.file_hashes.insert("a.rs".into(), 99);
        let e_stale = g.current_trace_epoch();
        assert_eq!(e1, e_stale);
        g.invalidate_trace_epoch();
        let e2 = g.current_trace_epoch();
        assert_ne!(e1, e2);
    }

    #[test]
    fn key_changes_with_symbol_or_epoch() {
        let k1 = make_trace_key(
            "/p", "TraceBlastRadius", "Foo", 1, 2, 50, 200, &None, &None, None, None, None, None,
            0, None, None,
        );
        let k2 = make_trace_key(
            "/p", "TraceBlastRadius", "Bar", 1, 2, 50, 200, &None, &None, None, None, None, None,
            0, None, None,
        );
        let k3 = make_trace_key(
            "/p", "TraceBlastRadius", "Foo", 2, 2, 50, 200, &None, &None, None, None, None, None,
            0, None, None,
        );
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
        let k_focus = make_trace_key(
            "/p",
            "TraceBlastRadius",
            "Foo",
            1,
            2,
            50,
            200,
            &None,
            &None,
            None,
            Some("Parent"),
            None,
            None,
            0,
            None,
            None,
        );
        assert_ne!(k1, k_focus, "focus_symbol must change TraceKey (Soft I4)");
        let k_off = make_trace_key(
            "/p", "TraceBlastRadius", "Foo", 1, 2, 50, 200, &None, &None, None, None, None, None,
            10, None, None,
        );
        assert_ne!(k1, k_off, "sample_offset must change TraceKey");
    }

    #[test]
    fn store_and_lookup_roundtrip_hot_ram() {
        let root = std::env::temp_dir().join(format!(
            "butler_trace_memo_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        let key = 42u64;
        let payload = TraceMemoPayload {
            format: MEMO_FORMAT,
            graph_epoch: 7,
            goal: "TraceBlastRadius".into(),
            symbol: "Foo".into(),
            target: MemoTarget {
                name: "Foo".into(),
                file: "src/foo.rs".into(),
                line: 1,
                definition: Some("fn Foo() {}".into()),
                lang: Some("rust".into()),
                cluster: Some("core:rs".into()),
                seed_id: "src/foo.rs:function_item:abcd1234".into(),
            },
            callers: vec![],
            callees: vec![MemoNeighbor {
                name: "bar".into(),
                file: "src/bar.rs".into(),
                line: 2,
                hop: 1,
                lang: Some("rust".into()),
                cluster: None,
                relation: None,
            }],
            caller_path: vec![],
            peer_callers: vec![],
            bridge_callers: vec![],
            bridge_callees: vec![],
            blast_domain: Some("call".into()),
            seed_kind: Some("function_item".into()),
            locations: vec![],
            suggested_scopes: vec!["src/".into()],
            active_cluster: Some("core:rs".into()),
            callers_omitted: 0,
            callees_omitted: 0,
        };
        store(&root, key, payload.clone());
        // Hot hit (no re-read required for correctness).
        let hit = lookup(&root, key, 7).expect("hit");
        assert_eq!(hit.symbol, "Foo");
        assert_eq!(hit.callees.len(), 1);
        assert!(lookup(&root, key, 8).is_none(), "epoch mismatch must miss");
        // Disk present for process restart.
        assert!(memo_path(&root).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
}
