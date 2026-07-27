//! Python-side FFI import / call resolution against an export table.
//!
//! Lang drawer: Python import syntax and call-site heuristics only.
//! Export discovery lives in lang drawers:
//! - [`crate::snooper::lang::rust::ffi`] (`#[pyfunction]`)
//! - [`crate::snooper::lang::c_family::ffi`] (pybind11 `m.def`)

use crate::snooper::model::{BlockInfo, CodeGraph, Id};
use crate::snooper::project_paths::ProjectPaths;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Build py→rs edges: Python import/call/`*_py` twin → Rust export Id.
///
/// **Rayon read-map** over Python files.
pub fn link_to_ffi_exports(
    graph: &CodeGraph,
    project_root: Option<&Path>,
    exports: &HashMap<String, Id>,
) -> Vec<(Id, Id)> {
    if exports.is_empty() {
        return Vec::new();
    }
    let pp = project_root.map(ProjectPaths::new);
    // Resolve export Id → native file once (co-location twin for monorepo dual-stack).
    let export_files: HashMap<&str, PathBuf> = exports
        .iter()
        .filter_map(|(name, id)| {
            graph
                .nodes
                .get(id)
                .map(|b| (name.as_str(), b.file.clone()))
        })
        .collect();

    let mut by_file: HashMap<PathBuf, Vec<&BlockInfo>> = HashMap::new();
    for b in graph.nodes.values() {
        if b.lang == "python" {
            // Guide/docs/glossary: never structural FFI (re.search / docs noise).
            if ffi_python_noise_path(&b.file) {
                continue;
            }
            by_file.entry(b.file.clone()).or_default().push(b);
        }
    }

    let file_entries: Vec<(PathBuf, Vec<&BlockInfo>)> = by_file.into_iter().collect();
    let mut new_edges: Vec<(Id, Id)> = file_entries
        .par_iter()
        .flat_map_iter(|(file, blocks)| {
            if ffi_python_noise_path(file) {
                return Vec::new();
            }
            let file_src: Option<String> = if let Some(ref paths) = pp {
                std::fs::read_to_string(paths.to_abs(file))
                    .ok()
                    .or_else(|| join_block_sources(blocks))
            } else {
                join_block_sources(blocks)
            };
            let Some(src) = file_src else {
                return Vec::new();
            };

            let imported_exports = parse_import_export_names(&src, exports);

            // Twin names (`search_py` → `search`) are relevant even when the body-only
            // fallback lacks the import line (stripped warehouse / failed disk read).
            let mut twin_export_names: HashSet<String> = HashSet::new();
            for b in blocks.iter() {
                if !(b.kind.contains("function") || b.kind.contains("method")) {
                    continue;
                }
                if exports.contains_key(&b.name) {
                    twin_export_names.insert(b.name.clone());
                }
                if let Some(base) = b.name.strip_suffix("_py") {
                    if !base.is_empty() && exports.contains_key(base) {
                        twin_export_names.insert(base.to_string());
                    }
                }
            }

            if imported_exports.is_empty()
                && twin_export_names.is_empty()
                && !src_mentions_any_export(&src, exports)
            {
                return Vec::new();
            }

            // Only exports that appear in this file (import / mention) or twin name.
            // Avoids O(blocks × all_exports) on torch-scale export tables (thousands of m.def).
            let relevant: Vec<(&String, &Id)> = exports
                .iter()
                .filter(|(name, _)| {
                    imported_exports.contains(*name)
                        || twin_export_names.contains(*name)
                        || src.contains(name.as_str())
                })
                .map(|(n, id)| (n, id))
                .collect();
            if relevant.is_empty() {
                return Vec::new();
            }

            let mut local = Vec::new();
            for b in blocks {
                // Never fall back to the whole file for a function body — that pulls in
                // sibling imports and over-fans Trace callees.
                let is_fn = b.kind.contains("function") || b.kind.contains("method");
                let body: Option<&str> = if !b.source.is_empty() {
                    Some(b.source.as_str())
                } else if b.start_byte < b.end_byte && b.end_byte <= src.len() {
                    Some(&src[b.start_byte..b.end_byte])
                } else if !is_fn {
                    Some(src.as_str())
                } else {
                    None
                };
                for (export_name, rust_id) in &relevant {
                    let twin = b.name == **export_name
                        || b.name == format!("{export_name}_py");
                    let body_hit = body
                        .map(|bd| {
                            if is_fn {
                                // Free call `export(` (PyO3 word-count style) **or**
                                // module attr `m.export(` for long structural m.def names
                                // (pybind tests). Attribute form is gated — never re.search.
                                body_calls_name(bd, export_name)
                                    || (export_allows_module_attr_call(export_name)
                                        && body_module_attr_calls_name(bd, export_name))
                            } else {
                                body_calls_or_imports_name(bd, export_name)
                            }
                        })
                        .unwrap_or(false);
                    let reexport_twin =
                        imported_exports.contains(*export_name) && twin && is_fn;
                    // L2.2: dual-stack package co-location (`examples/word-count/` py↔rs)
                    // so pure `*_py` twins link without relying on import-line recovery.
                    let colocated_twin = twin
                        && is_fn
                        && export_files
                            .get(export_name.as_str())
                            .map(|rs| dual_stack_colocated(file, rs))
                            .unwrap_or(false);
                    if (body_hit || reexport_twin || colocated_twin) && b.lang == "python" {
                        // Belt: never point Export at TEST_SUBMODULE / macro shells.
                        if graph
                            .nodes
                            .get(*rust_id)
                            .map(|nb| {
                                crate::snooper::lang::c_family::ffi::is_junk_native_export_host(
                                    &nb.name,
                                )
                            })
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        local.push((b.id.clone(), (*rust_id).clone()));
                    }
                }
            }
            local
        })
        .collect();

    new_edges.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then(a.1.as_str().cmp(b.1.as_str())));
    new_edges.dedup();
    new_edges
}

/// Docs / mdbook glue must not invent Export bridges (`re.search(` ≠ `search(`).
fn ffi_python_noise_path(file: &Path) -> bool {
    let fl = file.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    fl.contains("/guide/")
        || fl.contains("/docs/")
        || fl.contains("/site/")
        || fl.contains("glossary")
        || fl.contains("noxfile")
        || fl.contains("/.github/")
        || fl.ends_with("/conftest.py")
}

/// True when Python and native export live in the same dual-stack package tree.
///
/// Monorepo: `examples/word-count/word_count/__init__.py` ↔ `examples/word-count/src/lib.rs`
/// Package root: `word_count/__init__.py` ↔ `src/lib.rs`
fn dual_stack_colocated(py_file: &Path, rs_file: &Path) -> bool {
    let a = py_file.to_string_lossy().replace('\\', "/");
    let b = rs_file.to_string_lossy().replace('\\', "/");
    let a = a.trim_start_matches("./");
    let b = b.trim_start_matches("./");
    if ffi_python_noise_path(Path::new(a)) {
        return false;
    }
    let common = common_path_prefix_segments(a, b);
    // Shared `examples/<pkg>/…` (or deeper package prefix).
    if common >= 2 {
        return true;
    }
    // Package-root warehouse: one side under `src/`, other is shallow non-noise.
    let a_src = a == "src" || a.starts_with("src/") || a.contains("/src/");
    let b_src = b == "src" || b.starts_with("src/") || b.contains("/src/");
    if a_src == b_src {
        return false;
    }
    let depth = |p: &str| p.split('/').filter(|s| !s.is_empty()).count();
    depth(a) <= 4 && depth(b) <= 4
}

fn common_path_prefix_segments(a: &str, b: &str) -> usize {
    let ap: Vec<&str> = a.split('/').filter(|s| !s.is_empty()).collect();
    let bp: Vec<&str> = b.split('/').filter(|s| !s.is_empty()).collect();
    ap.iter().zip(bp.iter()).take_while(|(x, y)| x == y).count()
}

/// `from .word_count import search, search_sequential`
pub fn parse_import_export_names(src: &str, exports: &HashMap<String, Id>) -> HashSet<String> {
    let mut out = HashSet::new();
    for line in src.lines() {
        let t = line.trim();
        if !t.starts_with("from ") || !t.contains(" import ") {
            continue;
        }
        let Some(imp) = t.split(" import ").nth(1) else {
            continue;
        };
        let imp = imp.split('#').next().unwrap_or(imp);
        for part in imp.split(',') {
            let name = part
                .trim()
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim();
            if exports.contains_key(name) {
                out.insert(name.to_string());
            }
        }
    }
    out
}

fn src_mentions_any_export(src: &str, exports: &HashMap<String, Id>) -> bool {
    exports.keys().any(|e| src.contains(e.as_str()))
}

/// Call site only: bare `name(` (never bare tokens / import lists / method calls).
///
/// Rejects `re.search(`, `obj.search(`, `::search(` — those are not the FFI export name.
fn body_calls_name(body: &str, name: &str) -> bool {
    let call = format!("{name}(");
    let mut start = 0usize;
    while let Some(rel) = body[start..].find(&call) {
        let i = start + rel;
        let prev = if i == 0 {
            None
        } else {
            body.as_bytes().get(i - 1).copied()
        };
        // Ident continue or attribute/path separator → not a free call of `name`.
        let ok_before = match prev {
            None => true,
            Some(b) if b.is_ascii_alphanumeric() || b == b'_' => false,
            Some(b'.') | Some(b':') => false, // re.search( / Foo::search(
            _ => true,
        };
        if ok_before {
            return true;
        }
        start = i + 1;
        if start >= body.len() {
            break;
        }
    }
    false
}

/// Allow `m.export(` only for long structural export names (pybind `m.def` tables).
///
/// Short/common names (`search`, `get`, …) stay free-call only so `re.search(` never
/// invents Export bridges (L2.2).
fn export_allows_module_attr_call(export: &str) -> bool {
    const BLOCK: &[&str] = &[
        "search", "match", "find", "get", "set", "load", "open", "read", "write", "close",
        "format", "join", "split", "replace", "pop", "push", "keys", "items", "values", "copy",
        "update", "clear", "sort", "count", "index", "append", "extend", "remove", "insert",
        "type", "str", "int", "list", "dict", "len", "print", "range", "map", "filter", "zip",
        "any", "all", "min", "max", "sum", "id", "hash", "iter", "next", "call", "init", "new",
        "del", "enter", "exit", "repr", "bytes", "bool", "float", "name", "path", "data", "value",
        "text", "info", "debug", "error", "warn", "log", "run", "start", "stop", "main",
    ];
    if BLOCK.iter().any(|b| *b == export) {
        return false;
    }
    // Structural m.def names: pass_cptr_base, test_function, overload_order, …
    export.len() >= 6
        && export
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `m.export(` / `mod.export(` — simple module attribute call (pybind test style).
fn body_module_attr_calls_name(body: &str, name: &str) -> bool {
    let call = format!(".{name}(");
    let mut start = 0usize;
    while let Some(rel) = body[start..].find(&call) {
        let i = start + rel;
        // Char before `.` must end a simple identifier (module alias), not `::` / another `.`.
        if i == 0 {
            start = i + 1;
            continue;
        }
        let before_dot = body.as_bytes()[i - 1];
        if !(before_dot.is_ascii_alphanumeric() || before_dot == b'_') {
            start = i + 1;
            continue;
        }
        // Walk back the receiver ident; reject nested `foo.bar.export(` (only one segment).
        let mut j = i - 1;
        while j > 0 {
            let c = body.as_bytes()[j - 1];
            if c.is_ascii_alphanumeric() || c == b'_' {
                j -= 1;
            } else {
                break;
            }
        }
        let recv_prev = if j == 0 {
            None
        } else {
            Some(body.as_bytes()[j - 1])
        };
        let simple_recv = match recv_prev {
            None | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') | Some(b'(')
            | Some(b'=') | Some(b',') | Some(b'[') | Some(b'{') | Some(b';') => true,
            Some(b'.') | Some(b':') => false, // foo.bar.export / Foo::export
            _ => true,
        };
        if simple_recv {
            return true;
        }
        start = i + 1;
        if start >= body.len() {
            break;
        }
    }
    false
}

fn body_calls_or_imports_name(body: &str, name: &str) -> bool {
    // Token equality only — never substring (`search` must not hit `search_sequential`).
    if body.contains(&format!("import {name}")) || body.contains(&format!("import {name},")) {
        if body
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|t| t == name)
        {
            return true;
        }
    }
    body_calls_name(body, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_token_does_not_match_search_sequential() {
        let body = "def search_py(c, n):\n    return search(c, n)\n";
        assert!(body_calls_name(body, "search"));
        assert!(!body_calls_name(body, "search_sequential"));
        assert!(!body_calls_name(body, "count_line"));
    }

    #[test]
    fn method_call_search_is_not_ffi_export_call() {
        // glossary_linker: link_block = re.search(…  must NOT invent Export → search
        assert!(!body_calls_name("link_block = re.search(\n", "search"));
        assert!(!body_calls_name("obj.search(x)\n", "search"));
        assert!(!body_calls_name("Foo::search(x)\n", "search"));
        assert!(body_calls_name("return search(c, n)\n", "search"));
        assert!(body_calls_name("search(c)\n", "search"));
        // Attr form blocked for high-FP short export names even if table has them.
        assert!(!export_allows_module_attr_call("search"));
    }

    #[test]
    fn pybind_module_attr_export_call_is_allowed_for_long_names() {
        assert!(export_allows_module_attr_call("test_function"));
        assert!(export_allows_module_attr_call("pass_cptr_base"));
        assert!(body_module_attr_calls_name(
            "assert m.test_function() == \"ok\"\n",
            "test_function"
        ));
        assert!(body_module_attr_calls_name(
            "return mod.pass_cptr_base(x)\n",
            "pass_cptr_base"
        ));
        // Nested attribute still rejected (foo.bar.export).
        assert!(!body_module_attr_calls_name(
            "return foo.bar.test_function()\n",
            "test_function"
        ));
    }

    #[test]
    fn pure_twin_colocated_monorepo_links_without_import_line() {
        // Stripped body-only warehouse: no import line recovered → co-location twin.
        use crate::snooper::model::{BlockInfo, CodeGraph};
        use std::collections::HashSet;
        use std::path::PathBuf;

        let mut g = CodeGraph::new();
        let mut exports = HashMap::new();
        let rs = BlockInfo::new(
            PathBuf::from("examples/word-count/src/lib.rs"),
            "function_item",
            "rust",
            5,
            11,
            0,
            10,
            "#[pyfunction]\nfn search() {}".into(),
            "search",
            HashSet::new(),
        );
        exports.insert("search".into(), rs.id.clone());
        g.nodes.insert(rs.id.clone(), rs);
        let py = BlockInfo::new(
            PathBuf::from("examples/word-count/word_count/__init__.py"),
            "function_definition",
            "python",
            11,
            17,
            0,
            50,
            "def search_py(contents: str, needle: str) -> int:\n    total = 0\n    return total\n"
                .into(),
            "search_py",
            HashSet::new(),
        );
        g.nodes.insert(py.id.clone(), py.clone());

        let edges = link_to_ffi_exports(&g, None, &exports);
        assert!(
            edges.iter().any(|(f, t)| f == &py.id && exports.get("search") == Some(t)),
            "colocated search_py → search: {edges:?}"
        );
    }

    #[test]
    fn guide_glossary_noise_path_never_links() {
        use crate::snooper::model::{BlockInfo, CodeGraph};
        use std::collections::HashSet;
        use std::path::PathBuf;

        let mut g = CodeGraph::new();
        let mut exports = HashMap::new();
        let rs = BlockInfo::new(
            PathBuf::from("examples/word-count/src/lib.rs"),
            "function_item",
            "rust",
            5,
            11,
            0,
            10,
            "fn search() {}".into(),
            "search",
            HashSet::new(),
        );
        exports.insert("search".into(), rs.id.clone());
        g.nodes.insert(rs.id.clone(), rs);
        let py = BlockInfo::new(
            PathBuf::from("guide/glossary_linker.py"),
            "function_definition",
            "python",
            46,
            50,
            0,
            80,
            // Would match pre-fix body_calls_name via re.search(
            "def link_block(content):\n    return re.search(r'x', content)\n".into(),
            "link_block",
            HashSet::new(),
        );
        g.nodes.insert(py.id.clone(), py.clone());
        let edges = link_to_ffi_exports(&g, None, &exports);
        assert!(
            edges.is_empty(),
            "glossary must not Export-link to search: {edges:?}"
        );
    }

    #[test]
    fn pure_py_twin_does_not_fan_to_import_siblings() {
        // Real word-count shape: search_py body never calls rust; only twin → search.
        use crate::snooper::model::{BlockInfo, CodeGraph};
        use std::collections::HashSet;
        use std::path::PathBuf;

        let mut g = CodeGraph::new();
        let mut exports = HashMap::new();
        for (name, line) in [
            ("search", 1usize),
            ("search_sequential", 2),
            ("search_sequential_detached", 3),
            ("count_line", 4),
        ] {
            let b = BlockInfo::new(
                PathBuf::from("src/lib.rs"),
                "function_item",
                "rust",
                line,
                line,
                0,
                10,
                format!("fn {name}() {{}}"),
                name,
                HashSet::new(),
            );
            exports.insert(name.to_string(), b.id.clone());
            g.nodes.insert(b.id.clone(), b);
        }
        let py = BlockInfo::new(
            PathBuf::from("word_count/__init__.py"),
            "function_definition",
            "python",
            11,
            17,
            0,
            50,
            // Pure python twin — no call to search().
            "def search_py(contents: str, needle: str) -> int:\n    total = 0\n    return total\n"
                .into(),
            "search_py",
            HashSet::new(),
        );
        // File-level imports exist but must not fan the function.
        let _file = "from .word_count import search, search_sequential, search_sequential_detached\n";
        g.nodes.insert(py.id.clone(), py.clone());

        // Without disk, twin still links via name; imports not readable → twin only if we
        // pass empty imports path. Use link with None root; twin needs imported_exports —
        // reexport_twin requires import. So only body_hit; pure body → 0 edges.
        let edges = link_to_ffi_exports(&g, None, &exports);
        assert!(
            edges.iter().all(|(f, _)| f != &py.id)
                || edges
                    .iter()
                    .filter(|(f, _)| f == &py.id)
                    .all(|(_, t)| exports.get("search") == Some(t)),
            "search_py must not fan to sequential/count_line: {edges:?}"
        );
    }

    #[test]
    fn link_prefers_called_export_not_import_list_siblings() {
        use crate::snooper::model::{BlockInfo, CodeGraph};
        use std::collections::HashSet;
        use std::path::PathBuf;

        let mut g = CodeGraph::new();
        let mut exports = HashMap::new();
        let r_search = BlockInfo::new(
            PathBuf::from("src/lib.rs"),
            "function_item",
            "rust",
            1,
            1,
            0,
            10,
            "fn search() {}".into(),
            "search",
            HashSet::new(),
        );
        let r_seq = BlockInfo::new(
            PathBuf::from("src/lib.rs"),
            "function_item",
            "rust",
            2,
            2,
            20,
            30,
            "fn search_sequential() {}".into(),
            "search_sequential",
            HashSet::new(),
        );
        exports.insert("search".into(), r_search.id.clone());
        exports.insert("search_sequential".into(), r_seq.id.clone());
        let py = BlockInfo::new(
            PathBuf::from("word_count/__init__.py"),
            "function_definition",
            "python",
            4,
            5,
            0,
            50,
            "def search_py(c, n):\n    return search(c, n)\n".into(),
            "search_py",
            HashSet::new(),
        );
        // Module source with both imports — must not over-link via file-level contains.
        g.nodes.insert(r_search.id.clone(), r_search.clone());
        g.nodes.insert(r_seq.id.clone(), r_seq.clone());
        g.nodes.insert(py.id.clone(), py.clone());

        let edges = link_to_ffi_exports(&g, None, &exports);
        assert!(
            edges.iter().any(|(f, t)| f == &py.id && t == &r_search.id),
            "search_py → search: {edges:?}"
        );
        assert!(
            !edges.iter().any(|(f, t)| f == &py.id && t == &r_seq.id),
            "must not link search_py → search_sequential: {edges:?}"
        );
        // Twin reexport still works
        assert!(
            edges.iter().any(|(f, t)| f == &py.id && t == &r_search.id && py.name == "search_py"),
            "{edges:?}"
        );
    }
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
