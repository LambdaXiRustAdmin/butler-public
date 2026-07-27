//! Per-language CALL name maps (Thesis B2 peel).
//!
//! Unique + production-preferring maps for JIT / FullEdge / watcher re-edge.
//! Zero intentional behavior change — no ranking or family-bucket retunes.

use crate::snooper::model::{BlockInfo, CallNameMaps, CodeGraph, Id};
use std::time::Instant;

/// True for real defs that may be **CALL** edge targets.
/// Types/structs/classes are never callees — they appear in signatures and must not
/// become `CALLS` edges (e.g. glfwCreateWindow ↛ GLFWwindow).
pub(crate) fn is_call_target_kind(kind: &str) -> bool {
    let k = kind.to_ascii_lowercase();
    if k.contains("call") {
        return false;
    }
    // Explicit type drop (even if name also contains "function" somehow).
    if k.contains("struct")
        || k.contains("class")
        || k.contains("enum")
        || k.contains("trait")
        || k.contains("interface")
        || k.contains("type_spec")
        || k.contains("type_item")
        || k.contains("union")
        || k.contains("impl")
    {
        return false;
    }
    k.contains("function") || k.contains("method") || k.contains("arrow_function")
}

/// Call-edge language family for an on-disk extension.
/// Cross-lang links belong to structural FFI / IPC — never this map.
pub(crate) fn call_edge_family_for_block_lang(lang: &str) -> &'static str {
    let l = lang.to_ascii_lowercase();
    match l.as_str() {
        "python" | "py" => "python",
        "rust" | "rs" => "rust",
        "c" | "cpp" | "c++" | "cxx" => "c_family",
        "go" => "go",
        "typescript" | "javascript" | "ts" | "tsx" | "js" | "jsx" => "typescript",
        _ => "other",
    }
}

/// All call-target ids per short name (Go package-qualified resolve needs multi-def).
pub(crate) fn build_all_name_ids_blocks(blocks: &[&BlockInfo]) -> std::collections::HashMap<String, Vec<Id>> {
    let mut map: std::collections::HashMap<String, Vec<Id>> =
        std::collections::HashMap::with_capacity(blocks.len() / 4);
    for b in blocks {
        if b.name.len() >= 2 && is_call_target_kind(&b.kind) {
            map.entry(b.name.clone()).or_default().push(b.id.clone());
        }
    }
    map
}

/// Unique-name map from a pre-filtered block slice (defs only; no family re-scan).
/// Call-expression shells used to inflate name counts — only call-target kinds count.
pub(crate) fn build_unique_name_map_blocks(blocks: &[&BlockInfo]) -> std::collections::HashMap<String, Id> {
    let mut name_count: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::with_capacity(blocks.len() / 4);
    for b in blocks {
        if b.name.len() >= 3 && is_call_target_kind(&b.kind) {
            *name_count.entry(b.name.as_str()).or_insert(0) += 1;
        }
    }
    blocks
        .iter()
        .filter(|b| {
            b.name.len() >= 3
                && is_call_target_kind(&b.kind)
                && *name_count.get(b.name.as_str()).unwrap_or(&0) == 1
        })
        .map(|b| (b.name.clone(), b.id.clone()))
        .collect()
}

/// Family-scoped map from a pre-bucketed slice (used after one partition pass).
///
/// Unique production defs first; then lang specialists + production-over-test.
/// **Must be language-scoped** (polyglot mega-map caused cross-lang CALL noise).
pub(crate) fn build_global_name_map_blocks(
    nodes: &std::collections::HashMap<Id, BlockInfo>,
    blocks: &[&BlockInfo],
    family: Option<&str>,
) -> std::collections::HashMap<String, Id> {
    let mut map = build_unique_name_map_blocks(blocks);
    // Language-specific: prefer tools/ production defs over test mains for ambiguous Python.
    // Still walk full graph (lang filter inside) — only when this family is python/ts.
    if family.is_none() || family == Some("python") {
        super::lang::python::prefer_ambiguous_python_names(nodes, &mut map);
    }
    if family.is_none() || family == Some("typescript") {
        super::lang::typescript::prefer_ambiguous_typescript_names(nodes, &mut map);
    }
    prefer_ambiguous_production_defs_blocks(blocks, &mut map);
    map
}

/// Build / reuse per-lang CALL maps on the graph (remember work between JIT traces).
///
/// **Was single-threaded 6× full walk** (~10s on 1.3M nodes). Now: one O(N) partition
/// then **rayon** per-family map build on each bucket (c_family-heavy monorepos win).
pub(crate) fn ensure_call_name_maps(graph: &mut CodeGraph) -> &std::sync::Arc<CallNameMaps> {
    if graph.call_name_maps.is_some() {
        return graph.call_name_maps.as_ref().unwrap();
    }
    let t0 = Instant::now();
    let n = graph.nodes.len();

    // One partition pass (not 6 serial family filters).
    let mut python: Vec<&BlockInfo> = Vec::new();
    let mut rust: Vec<&BlockInfo> = Vec::new();
    let mut c_family: Vec<&BlockInfo> = Vec::new();
    let mut go: Vec<&BlockInfo> = Vec::new();
    let mut typescript: Vec<&BlockInfo> = Vec::new();
    let mut other: Vec<&BlockInfo> = Vec::new();
    for b in graph.nodes.values() {
        match call_edge_family_for_block_lang(&b.lang) {
            "python" => python.push(b),
            "rust" => rust.push(b),
            "c_family" => c_family.push(b),
            "go" => go.push(b),
            "typescript" => typescript.push(b),
            _ => other.push(b),
        }
    }
    let sizes = (
        python.len(),
        rust.len(),
        c_family.len(),
        go.len(),
        typescript.len(),
        other.len(),
    );

    // Parallel family builds — each only scans its bucket (not full N again).
    let nodes = &graph.nodes;
    let ((python_m, rust_m), ((c_family_m, (go_m, go_all)), (typescript_m, other_m))) = rayon::join(
        || {
            rayon::join(
                || build_global_name_map_blocks(nodes, &python, Some("python")),
                || build_global_name_map_blocks(nodes, &rust, Some("rust")),
            )
        },
        || {
            rayon::join(
                || {
                    rayon::join(
                        || build_global_name_map_blocks(nodes, &c_family, Some("c_family")),
                        || {
                            let go_m = build_global_name_map_blocks(nodes, &go, Some("go"));
                            let go_all = build_all_name_ids_blocks(&go);
                            (go_m, go_all)
                        },
                    )
                },
                || {
                    rayon::join(
                        || build_global_name_map_blocks(nodes, &typescript, Some("typescript")),
                        || build_global_name_map_blocks(nodes, &other, Some("other")),
                    )
                },
            )
        },
    );

    let maps = CallNameMaps {
        python: python_m,
        rust: rust_m,
        c_family: c_family_m,
        go: go_m,
        go_all,
        typescript: typescript_m,
        other: other_m,
    };
    println!(
        "⚡ Call name maps built once for {} nodes in {:.2?} (rayon families; buckets py/rs/c/go/ts/other={:?})",
        n,
        t0.elapsed(),
        sizes
    );
    graph.call_name_maps = Some(std::sync::Arc::new(maps));
    graph.call_name_maps.as_ref().unwrap()
}

/// Snapshot for rayon — **O(1) Arc clone**, never deep-copy multi‑M maps (gecko OOM root cause).
pub(crate) fn call_name_maps_snapshot(graph: &mut CodeGraph) -> std::sync::Arc<CallNameMaps> {
    std::sync::Arc::clone(ensure_call_name_maps(graph))
}

/// Fill remaining names missing from the map with a production-leaning def.
/// Also handles the single-def case when unique-map skipped (should not after
/// call-shell fix, but keeps one production target for multi-def names).
pub(crate) fn prefer_ambiguous_production_defs_blocks(
    blocks: &[&BlockInfo],
    map: &mut std::collections::HashMap<String, Id>,
) {
    let mut by_name: std::collections::HashMap<&str, Vec<&BlockInfo>> =
        std::collections::HashMap::new();
    for b in blocks {
        if b.name.is_empty() || b.name.len() < 3 {
            continue;
        }
        if !is_call_target_kind(&b.kind) {
            continue;
        }
        by_name.entry(b.name.as_str()).or_default().push(b);
    }
    for (name, cands) in by_name {
        if map.contains_key(name) || cands.is_empty() {
            continue;
        }
        // Single production def → always map (call shells no longer block uniqueness).
        // Multiple → pick production-leaning path.
        let best = cands.into_iter().max_by_key(|b| {
            let f = b.file.to_string_lossy().to_ascii_lowercase();
            let mut s = 0i32;
            if f.contains("_test.") || f.contains("/test/") || f.contains("/tests/") {
                s -= 100;
            }
            if f.contains("/examples/") || f.contains("/fixtures/") {
                s -= 50;
            }
            if f.contains("/src/") || f.contains("/lib/") || f.contains("/tools/") {
                s += 20;
            }
            if f.contains("/include/") {
                s += 10;
            }
            // Prefer bodies over header prototypes for call-target resolution.
            if b.kind.contains("function_definition") {
                s += 35;
            } else if b.kind.contains("function_declaration") {
                s += 8;
            } else if b.kind.contains("function") || b.kind.contains("method") {
                s += 15;
            }
            // .c/.cpp impl files beat .h for the global name map.
            if f.ends_with(".c") || f.ends_with(".cc") || f.ends_with(".cpp") || f.ends_with(".cxx")
            {
                s += 12;
            } else if f.ends_with(".h")
                || f.ends_with(".hpp")
                || f.ends_with(".hh")
                || f.ends_with(".hxx")
            {
                s -= 8;
            }
            s
        });
        if let Some(b) = best {
            map.insert(name.to_string(), b.id.clone());
        }
    }
}

