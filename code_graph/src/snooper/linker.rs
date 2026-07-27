//! Cross-language linker: C decl↔def + **thin** re-exports of interconnect.
//!
//! Default post-edge path:
//! 1. C/C++ decl↔def implements (this module)
//! 2. Interconnect steward (`interconnect::passes`) — Export / Ipc / Twin + TS CALL imports
//!
//! **Aho–Corasick Twin bridges are OFF by default** — opt-in `BUTLER_POLYGLOT_AC=1`.

use crate::snooper::ipc_engine::{self, IpcRule};
use crate::snooper::model::{BlockInfo, CodeGraph, Id};
use crate::snooper::project_paths::ProjectPaths;

use aho_corasick::AhoCorasick;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── Interconnect re-exports (P.2: single owner for bridge orchestration) ──
pub use crate::snooper::interconnect::{
    apply_post_edge_maps, graph_has_c_family, graph_has_python, graph_has_rust, graph_has_ts_js,
    map_without_decl_def as map_post_edge_passes_without_decl_def, run_without_decl_def,
    LangPresence, PostEdgeMaps,
};

/// Cross-language post passes after per-file edge collection.
pub fn run_post_edge_passes(
    graph: &mut CodeGraph,
    rules: Option<&[IpcRule]>,
    project_root: Option<&Path>,
) {
    run_post_edge_passes_chunked(graph, rules, project_root, None);
}

/// Chunked whole-program post-pass (compiler LTO-ish, **not** per translation unit).
/// `between_chunks` yields to WarehousePolice JIT so Trace is not frozen for minutes.
pub fn run_post_edge_passes_chunked(
    graph: &mut CodeGraph,
    rules: Option<&[IpcRule]>,
    project_root: Option<&Path>,
    mut between_chunks: Option<&mut dyn FnMut()>,
) {
    let mut default_rules = Vec::new();
    let rules = rules.unwrap_or_else(|| {
        default_rules = ipc_engine::default_ipc_rules();
        &default_rules[..]
    });
    let mut yield_lane = || {
        if let Some(h) = between_chunks.as_mut() {
            h();
        }
    };

    // C/C++ header prototype ↔ definition (same-name implements edges).
    build_c_decl_def_edges(graph);
    yield_lane();
    run_post_edge_passes_without_decl_def(graph, Some(rules), project_root);
    yield_lane();
}

/// Interconnect map+reduce (Export/Ipc/Twin + TS imports). After C decl↔def.
pub fn run_post_edge_passes_without_decl_def(
    graph: &mut CodeGraph,
    rules: Option<&[IpcRule]>,
    project_root: Option<&Path>,
) {
    run_without_decl_def(graph, rules, project_root);
}

#[cfg(test)]
mod map_reduce_tests {
    use super::*;
    use crate::snooper::model::{BlockInfo, CodeGraph};
    use std::collections::HashSet;

    fn blk(name: &str, kind: &str, lang: &str, file: &str, source: &str) -> BlockInfo {
        BlockInfo::new(
            file,
            kind,
            lang,
            1,
            3,
            0,
            source.len(),
            source.to_string(),
            name,
            HashSet::new(),
        )
    }

    #[test]
    fn map_c_decl_def_does_not_require_mut_graph() {
        let mut g = CodeGraph::new();
        let decl = blk(
            "foo",
            "function_declaration",
            "c",
            "inc/foo.h",
            "int foo(void);",
        );
        let def = blk(
            "foo",
            "function_definition",
            "c",
            "src/foo.c",
            "int foo(void) { return 1; }",
        );
        g.nodes.insert(decl.id.clone(), decl);
        g.nodes.insert(def.id.clone(), def);
        let edges = map_c_decl_def_edges(&g);
        assert!(!edges.is_empty(), "expected implements edge");
        assert!(g.edges.is_empty(), "map must not mutate");
    }

    #[test]
    fn ts_gate_false_on_c_only_graph() {
        let mut g = CodeGraph::new();
        let def = blk(
            "foo",
            "function_definition",
            "c",
            "src/foo.c",
            "int foo(void) { return 0; }",
        );
        g.nodes.insert(def.id.clone(), def);
        assert!(!graph_has_ts_js(&g));
        assert!(graph_has_c_family(&g));
    }

    #[test]
    fn ffi_gate_skips_without_python() {
        let mut g = CodeGraph::new();
        let def = blk(
            "foo",
            "function_definition",
            "c",
            "src/foo.c",
            "int foo(void) { return 0; }",
        );
        g.nodes.insert(def.id.clone(), def);
        let p = LangPresence::scan(&g);
        assert!(!p.wants_ffi_export_map());
        assert!(map_ffi_export_edges_gated(&g, None, &p).is_empty());
    }

    #[test]
    fn ffi_gate_wants_python_plus_c() {
        let mut g = CodeGraph::new();
        let c = blk("foo", "function_definition", "c", "src/foo.c", "int foo();");
        let py = blk(
            "bar",
            "function_definition",
            "python",
            "mod.py",
            "def bar():\n  pass",
        );
        g.nodes.insert(c.id.clone(), c);
        g.nodes.insert(py.id.clone(), py);
        let p = LangPresence::scan(&g);
        assert!(p.python && p.c_family);
        assert!(p.wants_ffi_export_map());
    }
}

/// Link C/C++ `function_definition` → matching `function_declaration` (implements).
///
/// Matching is by symbol name with path heuristics (same stem `.h`/`.c`, `/include/`).
/// `static` defs only link same-file prototypes so private helpers stay local.
pub fn build_c_decl_def_edges(graph: &mut CodeGraph) {
    let new_edges = map_c_decl_def_edges(graph);
    if !new_edges.is_empty() {
        println!(
            "⚡ C decl↔def: {} implements edges (defs with prototypes)",
            new_edges.len()
        );
        graph.add_edges_batch(new_edges);
    }
}

/// Read-only C decl↔def edge map (map phase of map-reduce LTO).
pub fn map_c_decl_def_edges(graph: &CodeGraph) -> Vec<(Id, Id)> {
    if !graph_has_c_family(graph) {
        return Vec::new();
    }
    let mut decls: HashMap<String, Vec<Id>> = HashMap::new();
    let mut defs: HashMap<String, Vec<Id>> = HashMap::new();

    for b in graph.nodes.values() {
        if !crate::snooper::lang::c_family::is_c_family_block(b) {
            continue;
        }
        match b.kind.as_str() {
            "function_declaration" if !b.name.is_empty() => {
                decls.entry(b.name.clone()).or_default().push(b.id.clone());
            }
            "function_definition" if !b.name.is_empty() => {
                defs.entry(b.name.clone()).or_default().push(b.id.clone());
            }
            _ => {}
        }
    }

    if decls.is_empty() || defs.is_empty() {
        return Vec::new();
    }

    // Parallel per-name def groups (shared decl index is read-only).
    let name_keys: Vec<&String> = defs.keys().collect();
    let partial: Vec<Vec<(Id, Id)>> = name_keys
        .par_iter()
        .map(|name| {
            let def_ids = match defs.get(*name) {
                Some(d) => d,
                None => return Vec::new(),
            };
            let Some(decl_ids) = decls.get(*name) else {
                return Vec::new();
            };
            let mut local = Vec::new();
            for def_id in def_ids {
                let Some(def) = graph.nodes.get(def_id) else {
                    continue;
                };
                let static_def = crate::snooper::lang::c_family::looks_static_c_block(def);
                if static_def {
                    for decl_id in decl_ids {
                        if let Some(decl) = graph.nodes.get(decl_id) {
                            if same_repo_file(&def.file, &decl.file) {
                                local.push((def_id.clone(), decl_id.clone()));
                            }
                        }
                    }
                    continue;
                }
                if let Some(decl_id) = pick_best_c_decl(def, decl_ids, graph) {
                    local.push((def_id.clone(), decl_id));
                }
            }
            local
        })
        .collect();
    partial.into_iter().flatten().collect()
}

fn same_repo_file(a: &Path, b: &Path) -> bool {
    let na = a.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    let nb = b.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    na == nb || na.ends_with(&nb) || nb.ends_with(&na)
}

fn file_stem_lower(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn is_header_path(p: &Path) -> bool {
    let f = p.to_string_lossy().to_ascii_lowercase();
    f.ends_with(".h")
        || f.ends_with(".hpp")
        || f.ends_with(".hh")
        || f.ends_with(".hxx")
        || f.contains("/include/")
}

/// Score a candidate prototype for a definition (higher = better match).
fn c_decl_match_score(def: &BlockInfo, decl: &BlockInfo) -> i32 {
    let mut s = 0i32;
    let def_stem = file_stem_lower(&def.file);
    let decl_stem = file_stem_lower(&decl.file);
    if !def_stem.is_empty() && def_stem == decl_stem {
        s += 100; // foo.c ↔ foo.h
    }
    if is_header_path(&decl.file) {
        s += 40;
    }
    let df = def.file.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    let hf = decl.file.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    // Same directory pair (src/x.c + src/x.h)
    if let (Some(dp), Some(hp)) = (
        def.file.parent().map(|p| p.to_string_lossy().to_ascii_lowercase()),
        decl.file.parent().map(|p| p.to_string_lossy().to_ascii_lowercase()),
    ) {
        if dp == hp {
            s += 30;
        }
    }
    if hf.contains("/include/") {
        s += 20;
    }
    if df.contains("/test") || hf.contains("/test") {
        s -= 50;
    }
    // Prefer non-static prototypes for public defs
    if crate::snooper::lang::c_family::looks_static_c_block(decl) {
        s -= 80;
    }
    s
}

fn pick_best_c_decl(def: &BlockInfo, decl_ids: &[Id], graph: &CodeGraph) -> Option<Id> {
    decl_ids
        .iter()
        .filter_map(|id| graph.nodes.get(id).map(|b| (id, b)))
        .max_by_key(|(_, b)| c_decl_match_score(def, b))
        .filter(|(_, b)| c_decl_match_score(def, b) > 0 || same_repo_file(&def.file, &b.file))
        .map(|(id, _)| id.clone())
        .or_else(|| {
            // Fallback: any same-name non-static header-ish prototype
            decl_ids.iter().find_map(|id| {
                let b = graph.nodes.get(id)?;
                if crate::snooper::lang::c_family::looks_static_c_block(b) {
                    return None;
                }
                Some(id.clone())
            })
        })
}

/// Structural FFI bridges: orchestrates lang drawers (native export table → Python link).
///
/// Lang-specific mechanics live in:
/// - [`crate::snooper::lang::rust::ffi`] — `#[pyfunction]` export discovery
/// - [`crate::snooper::lang::c_family::ffi`] — pybind11 `m.def` export discovery
/// - [`crate::snooper::lang::python::ffi`] — import/call/`*_py` twin resolution
///
/// Unlike Aho–Corasick (string coincidence), this only links when a native side marks an
/// export and Python imports or invokes that **export name**.
pub fn build_ffi_export_edges(graph: &mut CodeGraph, project_root: Option<&Path>) {
    let new_edges = map_ffi_export_edges(graph, project_root);
    if !new_edges.is_empty() {
        graph.add_bridge_edges_batch(
            new_edges
                .into_iter()
                .map(|(a, b)| (a, b, crate::snooper::interconnect::BridgeKind::Export)),
        );
    }
}

/// Read-only FFI export → Python link map.
pub fn map_ffi_export_edges(graph: &CodeGraph, project_root: Option<&Path>) -> Vec<(Id, Id)> {
    let presence = LangPresence::scan(graph);
    map_ffi_export_edges_gated(graph, project_root, &presence)
}

/// FFI map with precomputed language presence (skip whole O(n) walks when pointless).
pub fn map_ffi_export_edges_gated(
    graph: &CodeGraph,
    project_root: Option<&Path>,
    presence: &LangPresence,
) -> Vec<(Id, Id)> {
    if !presence.wants_ffi_export_map() {
        println!(
            "⚡ FFI map skipped (lang gate: python={} rust={} c_family={})",
            presence.python, presence.rust, presence.c_family
        );
        return Vec::new();
    }
    let t0 = std::time::Instant::now();
    let mut exports = if presence.rust {
        crate::snooper::lang::rust::ffi::collect_pyfunction_exports(graph, project_root)
    } else {
        HashMap::new()
    };
    let py_n = exports.len();
    let pybind = if presence.c_family {
        crate::snooper::lang::c_family::ffi::collect_pybind_mdef_exports(graph, project_root)
    } else {
        HashMap::new()
    };
    let pybind_n = pybind.len();
    for (k, v) in pybind {
        // Prefer first writer (pyfunction) on name collision; pybind fills gaps.
        exports.entry(k).or_insert(v);
    }
    if exports.is_empty() {
        println!(
            "⚡ FFI map: 0 exports in {:.2?} (scanned rust={} c={})",
            t0.elapsed(),
            presence.rust,
            presence.c_family
        );
        return Vec::new();
    }
    let new_edges =
        crate::snooper::lang::python::ffi::link_to_ffi_exports(graph, project_root, &exports);
    if !new_edges.is_empty() {
        println!(
            "⚡ FFI export bridges: {} edges ({} exports: {} pyfunction + {} pybind m.def) in {:.2?}",
            new_edges.len(),
            exports.len(),
            py_n,
            pybind_n,
            t0.elapsed()
        );
    } else {
        println!(
            "⚡ FFI export table: {} export(s) ({} pyfunction + {} pybind), 0 py→native bridges in {:.2?}",
            exports.len(),
            py_n,
            pybind_n,
            t0.elapsed()
        );
    }
    new_edges
}

/// Builds cross-language edges. Prefer calling with `project_root` so empty
/// `BlockInfo.source` (slim cache) still participates via on-disk re-read.
///
/// **Weak heuristic** (Aho–Corasick name coincidence). Structural FFI exports are
/// handled by [`build_ffi_export_edges`]. AC skips nox/docs/guide noise paths and
/// short/common names so it helps distinctive dual-stack IDs without flooding bridges.
pub fn build_polyglot_edges(graph: &mut CodeGraph, project_root: Option<&Path>) {
    if !crate::snooper::interconnect::polyglot_ac_enabled() {
        return;
    }
    let new_edges = map_polyglot_edges(graph, project_root);
    if !new_edges.is_empty() {
        graph.add_bridge_edges_batch(
            new_edges
                .into_iter()
                .map(|(a, b)| (a, b, crate::snooper::interconnect::BridgeKind::Twin)),
        );
    }
}

/// Read-only polyglot AC map (caller gates with `polyglot_ac_enabled`).
pub fn map_polyglot_edges(graph: &CodeGraph, project_root: Option<&Path>) -> Vec<(Id, Id)> {
    let n_nodes = graph.nodes.len();
    if n_nodes == 0 {
        return Vec::new();
    }

    // No hard node skip: pattern set is capped (MAX_PATTERNS); scan is rayon-parallel.
    // Huge monorepos (emscripten) still need interconnects for Rust/C + shell layers.

    let type_kinds: &[&str] = &[
        "function_definition",
        "function_declaration",
        "function_item",
        "method_definition",
        "method_declaration",
        "class_declaration",
        "class_definition",
        "class_specifier",
        "struct_specifier",
        "type_spec",
        "interface_declaration",
        "struct_item",
        "enum_item",
        "trait_item",
        "type_item",
    ];

    // Dedup by name — prefer production defs (src/lib over tests/examples).
    // Prefer distinctive identifiers (phase_link, server_start) over English noise (from, find).
    let mut by_name: HashMap<&str, &BlockInfo> = HashMap::new();
    for b in graph.nodes.values() {
        if !type_kinds.contains(&b.kind.as_str()) {
            continue;
        }
        if !is_polyglot_worthy_name(&b.name) {
            continue;
        }
        let f = b.file.to_string_lossy().to_ascii_lowercase();
        if f.contains("_test.") || f.contains("/test/") || f.contains("/tests/") {
            continue;
        }
        let score = polyglot_target_score(b);
        match by_name.get(b.name.as_str()) {
            Some(prev) if polyglot_target_score(prev) >= score => {}
            _ => {
                by_name.insert(b.name.as_str(), b);
            }
        }
    }
    let mut dict: Vec<&BlockInfo> = by_name.into_values().collect();

    if dict.is_empty() {
        return Vec::new();
    }

    // Prefer longer / more distinctive names when capping.
    const MAX_PATTERNS: usize = 8_000;
    dict.sort_by(|a, b| {
        polyglot_target_score(b)
            .cmp(&polyglot_target_score(a))
            .then_with(|| b.name.len().cmp(&a.name.len()))
            .then_with(|| a.name.cmp(&b.name))
    });
    if dict.len() > MAX_PATTERNS {
        dict.truncate(MAX_PATTERNS);
        println!(
            "⚡ Polyglot pattern cap: using {} of unique defs (nodes={})",
            MAX_PATTERNS, n_nodes
        );
    }

    let patterns: Vec<&str> = dict.iter().map(|b| b.name.as_str()).collect();
    let ac_start = std::time::Instant::now();
    let ac = match AhoCorasick::new(&patterns) {
        Ok(ac) => ac,
        Err(_) => return Vec::new(),
    };
    println!(
        "⚡ Polyglot AC built: {} patterns in {:.2?}",
        patterns.len(),
        ac_start.elapsed()
    );

    let metas: Vec<(Id, String)> = dict
        .iter()
        .map(|b| (b.id.clone(), b.lang.clone()))
        .collect();

    // Group blocks by file for one read per path when sources stripped.
    let mut by_file: HashMap<PathBuf, Vec<&BlockInfo>> = HashMap::new();
    for b in graph.nodes.values() {
        by_file.entry(b.file.clone()).or_default().push(b);
    }

    let pp = project_root.map(ProjectPaths::new);
    let scan_start = std::time::Instant::now();

    let file_entries: Vec<(PathBuf, Vec<&BlockInfo>)> = by_file.into_iter().collect();
    let new_edges: Vec<(Id, Id)> = file_entries
        .par_iter()
        .flat_map_iter(|(file, blocks_in_file)| {
            // AC is weak: skip known noise shells (config/docs), not production FFI.
            let fl = file.to_string_lossy().to_ascii_lowercase();
            if polyglot_ac_skip_path(&fl) {
                return Vec::new();
            }
            // Slim warehouse: prefer full file from disk (byte spans stay valid).
            // Fallback: join non-empty in-memory sources (partial; spans may miss).
            let file_src: Option<String> = if let Some(ref paths) = pp {
                let abs = paths.to_abs(file);
                std::fs::read_to_string(&abs)
                    .ok()
                    .or_else(|| join_block_sources(blocks_in_file))
            } else {
                join_block_sources(blocks_in_file)
            };
            let Some(src) = file_src else {
                return Vec::new();
            };

            let mut local = Vec::new();
            for mat in ac.find_iter(&src) {
                let pat_idx = mat.pattern().as_usize();
                let (target_id, target_lang) = &metas[pat_idx];

                let start = mat.start();
                let end = mat.end();
                let before = if start > 0 {
                    src.as_bytes()[start - 1] as char
                } else {
                    '\0'
                };
                let after = if end < src.len() {
                    src.as_bytes()[end] as char
                } else {
                    '\0'
                };
                let is_word = |c: char| c.is_alphanumeric() || c == '_';
                if is_word(before) || is_word(after) {
                    continue;
                }

                // Attribute match to a containing block with *different* language.
                // Prefer function/method/class so Trace partition_trace_cores keeps them.
                let mut best: Option<(&BlockInfo, i32)> = None;
                for b in blocks_in_file {
                    if &b.lang == target_lang || b.id == *target_id {
                        continue;
                    }
                    let in_span = b.start_byte < b.end_byte
                        && start >= b.start_byte
                        && end <= b.end_byte;
                    if !in_span {
                        continue;
                    }
                    let score = polyglot_attr_score(b);
                    if best.map(|(_, s)| score > s).unwrap_or(true) {
                        best = Some((b, score));
                    }
                }
                if let Some((b, _)) = best {
                    local.push((b.id.clone(), target_id.clone()));
                } else {
                    // File-level fallback: best def-like block of different lang
                    let mut fb: Option<(&BlockInfo, i32)> = None;
                    for b in blocks_in_file {
                        if &b.lang == target_lang || b.id == *target_id {
                            continue;
                        }
                        let score = polyglot_attr_score(b);
                        if fb.map(|(_, s)| score > s).unwrap_or(true) {
                            fb = Some((b, score));
                        }
                    }
                    if let Some((b, _)) = fb {
                        local.push((b.id.clone(), target_id.clone()));
                    }
                }
            }
            local
        })
        .collect();

    println!(
        "⚡ Polyglot scan: {} cross-lang edges in {:.2?} (root={})",
        new_edges.len(),
        scan_start.elapsed(),
        project_root
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "none".into())
    );
    new_edges
}

fn join_block_sources(blocks: &[&BlockInfo]) -> Option<String> {
    let joined: String = blocks
        .iter()
        .filter(|b| !b.source.is_empty())
        .map(|b| b.source.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// Cross-lang AC patterns must be distinctive — short English tokens create junk edges.
/// Cross-lang AC patterns must be distinctive — monorepo Go/TS share Run/parseTime/etc.
/// Prefer structural uniqueness over "same English word both sides".
fn is_polyglot_worthy_name(name: &str) -> bool {
    // Explicit dual-stack collision stopwords (even if camelCase).
    const STOP: &[&str] = &[
        "Default",
        "Config",
        "Options",
        "Context",
        "Request",
        "Response",
        "Handler",
        "Client",
        "Server",
        "Manager",
        "Status",
        "Result",
        "Value",
        "Type",
        "Data",
        "Item",
        "List",
        "Map",
        "Set",
        "Get",
        "Run",
        "Start",
        "Stop",
        "Init",
        "Open",
        "Close",
        "Read",
        "Write",
        "Parse",
        "Format",
        "Update",
        "Delete",
        "Create",
        "Remove",
        "Clear",
        "Reset",
        "Load",
        "Save",
        "Check",
        "Validate",
        "Process",
        "Handle",
        "Execute",
        "Apply",
        "Build",
        "New",
        "parseTime",
        "parseDuration",
        "parseOption",
        "Annotations",
        "Labels",
        "Metric",
        "Histogram",
        "Notification",
        "Error",
        "String",
        "Content",
        "Duration",
        "Count",
        "File",
        "Keep",
        "Label",
        "Pool",
        "Target",
        "VectorSelector",
    ];
    if STOP.iter().any(|s| s.eq_ignore_ascii_case(name)) {
        return false;
    }
    if name.len() < 8 {
        return false;
    }
    // Pure lowercase without snake: Go method-ish — do not use as cross-lang AC pattern
    if name.chars().all(|c| c.is_ascii_lowercase()) {
        return name.contains('_') && name.len() >= 12;
    }
    let has_underscore = name.contains('_');
    let has_inner_upper = name.chars().skip(1).any(|c| c.is_ascii_uppercase());
    if has_underscore || has_inner_upper {
        return name.len() >= 8;
    }
    name.len() >= 14
}

/// Higher = better attribution source (function/class over call shells / modules).
fn polyglot_attr_score(b: &BlockInfo) -> i32 {
    let k = b.kind.to_ascii_lowercase();
    let mut s = 0i32;
    if k.contains("function") || k.contains("method") {
        s += 50;
    } else if k.contains("class")
        || k.contains("struct")
        || k.contains("impl")
        || k.contains("interface")
        || k.contains("trait")
    {
        s += 40;
    } else if k.contains("call") {
        s -= 20;
    }
    // Prefer named defs
    if b.name.len() >= 3 && b.name != "test_default" {
        s += 10;
    }
    s
}

/// Paths where AC name-coincidence does more harm than good (still allow structural FFI).
fn polyglot_ac_skip_path(fl: &str) -> bool {
    fl.contains("noxfile")
        || fl.ends_with("/noxfile.py")
        || fl.contains("/docs/")
        || fl.contains("/guide/")
        || fl.contains("/site/")
        || fl.contains("glossary")
        || fl.contains("/.github/")
        || fl.contains("conftest.py")
}

/// Higher = better polyglot target (production cores over fixtures/shells).
fn polyglot_target_score(b: &BlockInfo) -> i32 {
    let f = b.file.to_string_lossy().to_ascii_lowercase();
    let mut s = 0i32;
    // Examples can be gold FFI — don't bury them for AC targets when they are defs.
    if f.contains("/fixtures/") || f.contains("/bench") {
        s -= 40;
    }
    if f.contains("/examples/") && (b.kind.contains("function") || b.name.contains('_')) {
        s += 15; // word-count style demos
    } else if f.contains("/examples/") {
        s -= 10;
    }
    if f.contains("/src/") || f.contains("/lib/") || f.contains("/include/") {
        s += 25;
    }
    // Prefer longer distinctive names slightly (less false positive AC noise later via cap)
    s += (b.name.len() as i32).min(40);
    if b.kind.contains("function") || b.kind.contains("method") {
        s += 10;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::model::{BlockInfo, CodeGraph};
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn ffi_export_links_python_import_to_rust_pyfunction() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("butler_ffi_{stamp}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("word_count")).unwrap();
        let rs = root.join("src/lib.rs");
        let py = root.join("word_count/__init__.py");
        fs::write(
            &rs,
            r#"
use pyo3::prelude::*;
#[pyfunction]
fn search(contents: &str, needle: &str) -> usize { 0 }
#[pymodule]
fn word_count(m: &Bound<'_, PyModule>) -> PyResult<()> { Ok(()) }
"#,
        )
        .unwrap();
        fs::write(
            &py,
            r#"
from .word_count import search, search_sequential

def search_py(contents: str, needle: str) -> int:
    return search(contents, needle)
"#,
        )
        .unwrap();

        let mut g = CodeGraph::new();
        let rust_src = fs::read_to_string(&rs).unwrap();
        let py_src = fs::read_to_string(&py).unwrap();
        // Minimal blocks (warehouse-relative paths)
        let mut rblock = BlockInfo::new(
            PathBuf::from("src/lib.rs"),
            "function_item",
            "rust",
            3,
            4,
            rust_src.find("fn search").unwrap(),
            rust_src.len(),
            rust_src[rust_src.find("#[pyfunction]").unwrap()..].to_string(),
            "search",
            HashSet::new(),
        );
        // include attribute in source for detector
        rblock.source = "#[pyfunction]\nfn search(contents: &str, needle: &str) -> usize { 0 }\n"
            .into();
        let pblock = BlockInfo::new(
            PathBuf::from("word_count/__init__.py"),
            "function_definition",
            "python",
            4,
            6,
            py_src.find("def search_py").unwrap_or(0),
            py_src.len(),
            "def search_py(contents: str, needle: str) -> int:\n    return search(contents, needle)\n"
                .into(),
            "search_py",
            HashSet::new(),
        );
        g.nodes.insert(rblock.id.clone(), rblock.clone());
        g.nodes.insert(pblock.id.clone(), pblock.clone());

        build_ffi_export_edges(&mut g, Some(&root));
        let rid = rblock.id.clone();
        let pid = pblock.id.clone();
        let linked = g
            .bridge_kind_between(&pid, &rid)
            .map(|k| k == crate::snooper::interconnect::BridgeKind::Export)
            .unwrap_or(false);
        assert!(
            linked,
            "expected search_py → search Export bridge; bridges={:?}",
            g.bridge_fwd
        );
        assert!(
            g.edges.get(&pid).map(|v| v.is_empty()).unwrap_or(true),
            "Export must not land on CALL adjacency"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ffi_export_links_python_to_pybind_mdef() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("butler_pybind_{stamp}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("pkg")).unwrap();
        let cpp = root.join("src/bind.cpp");
        let py = root.join("pkg/__init__.py");
        fs::write(
            &cpp,
            r#"
#include <pybind11/pybind11.h>
namespace py = pybind11;
int pass_cptr_base(int x) { return x + 1; }
PYBIND11_MODULE(pkg, m) {
    m.def("pass_cptr_base", pass_cptr_base);
}
"#,
        )
        .unwrap();
        fs::write(
            &py,
            r#"
from .pkg import pass_cptr_base

def wrap_base(x: int) -> int:
    return pass_cptr_base(x)
"#,
        )
        .unwrap();

        let mut g = CodeGraph::new();
        let cpp_src = fs::read_to_string(&cpp).unwrap();
        let py_src = fs::read_to_string(&py).unwrap();
        let c_fn = BlockInfo::new(
            PathBuf::from("src/bind.cpp"),
            "function_definition",
            "cpp",
            4,
            4,
            cpp_src.find("int pass_cptr_base").unwrap_or(0),
            cpp_src.find("PYBIND11").unwrap_or(cpp_src.len()),
            "int pass_cptr_base(int x) { return x + 1; }\n".into(),
            "pass_cptr_base",
            HashSet::new(),
        );
        let p_fn = BlockInfo::new(
            PathBuf::from("pkg/__init__.py"),
            "function_definition",
            "python",
            4,
            5,
            py_src.find("def wrap_base").unwrap_or(0),
            py_src.len(),
            "def wrap_base(x: int) -> int:\n    return pass_cptr_base(x)\n".into(),
            "wrap_base",
            HashSet::new(),
        );
        g.nodes.insert(c_fn.id.clone(), c_fn.clone());
        g.nodes.insert(p_fn.id.clone(), p_fn.clone());

        build_ffi_export_edges(&mut g, Some(&root));
        let linked = g
            .bridge_kind_between(&p_fn.id, &c_fn.id)
            .map(|k| k == crate::snooper::interconnect::BridgeKind::Export)
            .unwrap_or(false);
        assert!(
            linked,
            "expected wrap_base → pass_cptr_base Export bridge; bridges={:?}",
            g.bridge_fwd
        );
        let _ = fs::remove_dir_all(&root);
    }


    #[test]
    fn word_count_search_py_edges_precise() {
        use crate::snooper::parser;
        let Some(root) = crate::resolve_optional_test_repo("pyo3/examples/word-count") else {
            return;
        };
        if !root.exists() {
            return;
        }
        let mut g = CodeGraph::new();
        for rel in ["src/lib.rs", "word_count/__init__.py"] {
            let abs = root.join(rel);
            let src = fs::read_to_string(&abs).unwrap();
            let parsed = parser::parse_file(Path::new(rel), &src).unwrap();
            for b in parsed.blocks {
                g.file_hashes
                    .insert(rel.to_string(), CodeGraph::content_hash(&src));
                g.nodes.insert(b.id.clone(), b);
            }
        }
        // Full edge path (same-lang maps + structural Export bridges)
        g.ensure_call_graph(&root, &[], None);
        let py = g.nodes.values().find(|b| b.name == "search_py").expect("search_py");
        let outs = g.bridge_children(&py.id);
        let names: Vec<_> = outs
            .iter()
            .filter_map(|(id, k)| {
                g.nodes
                    .get(id)
                    .map(|b| format!("{}:{}:{}", b.lang, b.name, k.as_relation_label()))
            })
            .collect();
        println!("search_py bridge callees: {:?}", names);
        assert_eq!(
            names,
            vec!["rust:search:export".to_string()],
            "expected only rust:search Export, got {:?}",
            names
        );
        assert!(
            g.children(&py.id).is_empty()
                || g.children(&py.id)
                    .iter()
                    .filter_map(|id| g.nodes.get(id))
                    .all(|b| b.lang == "python"),
            "Export must not appear as cross-lang CALL"
        );
    }

    #[test]
    fn polyglot_rereads_slim_sources_from_disk() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("butler_polyglot_{stamp}"));
        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        // Distinctive C symbol referenced from Python shell.
        let c_src = "int core_magic_dispatch(int x) { return x + 1; }\n";
        let py_src = "def wrap_core():\n    return core_magic_dispatch(1)\n";
        fs::write(src_dir.join("core.c"), c_src).unwrap();
        fs::write(src_dir.join("shell.py"), py_src).unwrap();

        let c_block = BlockInfo::new(
            PathBuf::from("src/core.c"),
            "function_definition",
            "c",
            1,
            1,
            0,
            c_src.len(),
            String::new(), // slim: empty warehouse source
            "core_magic_dispatch",
            HashSet::new(),
        );
        let py_block = BlockInfo::new(
            PathBuf::from("src/shell.py"),
            "function_definition",
            "python",
            1,
            2,
            0,
            py_src.len(),
            String::new(),
            "wrap_core",
            HashSet::new(),
        );
        let c_id = c_block.id.clone();
        let py_id = py_block.id.clone();

        let mut g = CodeGraph::new();
        g.nodes.insert(c_id.clone(), c_block);
        g.nodes.insert(py_id.clone(), py_block);

        // Twin AC is opt-in (BUTLER_POLYGLOT_AC).
        std::env::set_var("BUTLER_POLYGLOT_AC", "1");
        // Without root: no sources → no bridges
        build_polyglot_edges(&mut g, None);
        assert_eq!(
            g.total_bridge_edges(),
            0,
            "empty sources without root should yield 0 bridges"
        );

        // With root: disk re-read → Twin bridge
        build_polyglot_edges(&mut g, Some(&root));
        let has = g
            .bridge_kind_between(&py_id, &c_id)
            .map(|k| k == crate::snooper::interconnect::BridgeKind::Twin)
            .unwrap_or(false);
        std::env::remove_var("BUTLER_POLYGLOT_AC");
        let _ = fs::remove_dir_all(&root);
        assert!(
            has,
            "python wrap_core should Twin-bridge to c core_magic_dispatch; bridges={:?}",
            g.bridge_fwd
        );
    }

    #[test]
    fn c_decl_def_links_header_to_impl() {
        let h_src = "int core_magic_dispatch(int x);\n";
        let c_src = "int core_magic_dispatch(int x) { return x + 1; }\n";
        let decl = BlockInfo::new(
            PathBuf::from("include/core.h"),
            "function_declaration",
            "cpp",
            1,
            1,
            0,
            h_src.len(),
            h_src.to_string(),
            "core_magic_dispatch",
            HashSet::new(),
        );
        let def = BlockInfo::new(
            PathBuf::from("src/core.c"),
            "function_definition",
            "cpp",
            1,
            1,
            0,
            c_src.len(),
            c_src.to_string(),
            "core_magic_dispatch",
            HashSet::new(),
        );
        let decl_id = decl.id.clone();
        let def_id = def.id.clone();
        let mut g = CodeGraph::new();
        g.nodes.insert(decl_id.clone(), decl);
        g.nodes.insert(def_id.clone(), def);

        build_c_decl_def_edges(&mut g);
        let has = g
            .edges
            .get(&def_id)
            .map(|ts| ts.contains(&decl_id))
            .unwrap_or(false);
        assert!(
            has,
            "def should implement (edge to) header decl; edges={:?}",
            g.edges
        );
    }

    #[test]
    fn c_decl_def_skips_static_cross_file() {
        let h_src = "static int private_helper(void);\n";
        let c_src = "static int private_helper(void) { return 0; }\n";
        let decl = BlockInfo::new(
            PathBuf::from("include/core.h"),
            "function_declaration",
            "cpp",
            1,
            1,
            0,
            h_src.len(),
            h_src.to_string(),
            "private_helper",
            HashSet::new(),
        );
        let def = BlockInfo::new(
            PathBuf::from("src/core.c"),
            "function_definition",
            "cpp",
            1,
            1,
            0,
            c_src.len(),
            c_src.to_string(),
            "private_helper",
            HashSet::new(),
        );
        let mut g = CodeGraph::new();
        g.nodes.insert(decl.id.clone(), decl);
        g.nodes.insert(def.id.clone(), def);
        build_c_decl_def_edges(&mut g);
        assert_eq!(
            g.total_edges(),
            0,
            "static cross-file must not link; edges={:?}",
            g.edges
        );
    }
}
