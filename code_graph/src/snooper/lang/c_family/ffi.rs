//! C/C++-side FFI export discovery (pybind11 `m.def`).
//!
//! Lang drawer: only native binding *export* mechanics live here.
//! Linking into Python is orchestrated from the polyglot linker via
//! [`crate::snooper::lang::python::ffi`].

use crate::snooper::model::{BlockInfo, CodeGraph, Id};
use crate::snooper::project_paths::ProjectPaths;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Scan C/C++ sources for `m.def("export", &cpp_fn)` / `.def("export", cpp_fn)`.
/// Returns **Python-visible export name** → preferred native function Id.
///
/// **Rayon read-map** over files — multi-core; no graph mutation.
pub fn collect_pybind_mdef_exports(
    graph: &CodeGraph,
    project_root: Option<&Path>,
) -> HashMap<String, Id> {
    let by_name = index_native_functions(graph);
    if by_name.is_empty() {
        return HashMap::new();
    }

    let pp = project_root.map(ProjectPaths::new);
    let mut by_file: HashMap<PathBuf, Vec<&BlockInfo>> = HashMap::new();
    for b in graph.nodes.values() {
        if is_c_or_cpp(b) {
            by_file.entry(b.file.clone()).or_default().push(b);
        }
    }

    let file_entries: Vec<(PathBuf, Vec<&BlockInfo>)> = by_file.into_iter().collect();
    let partial: Vec<Vec<(String, Id)>> = file_entries
        .par_iter()
        .map(|(file, blocks)| {
            let file_src: Option<String> = if let Some(ref paths) = pp {
                std::fs::read_to_string(paths.to_abs(file))
                    .ok()
                    .or_else(|| join_sources(blocks))
            } else {
                join_sources(blocks)
            };
            let Some(src) = file_src else {
                return Vec::new();
            };
            // Only bother when the file looks like a pybind module.
            let lower = src.to_ascii_lowercase();
            if !lower.contains("pybind") && !src.contains(".def(") && !src.contains("m.def") {
                return Vec::new();
            }
            let mut local = Vec::new();
            for (export, target_fn) in parse_mdef_bindings(&src) {
                if is_junk_native_export_host(&export) {
                    continue;
                }
                if let Some(ref tname) = target_fn {
                    if is_junk_native_export_host(tname) {
                        continue;
                    }
                    if let Some(id) = by_name.get(tname.as_str()) {
                        local.push((export, id.clone()));
                        continue;
                    }
                }
                // Lambda / unresolved: nearest enclosing def — never TEST_SUBMODULE /
                // PYBIND11_MODULE shells (silence > invent; pytypes test_bytes hole).
                if let Some(host) = enclosing_def_block(blocks, &src, &export) {
                    if is_junk_native_export_host(&host.name) {
                        continue;
                    }
                    local.push((export, host.id.clone()));
                }
            }
            local
        })
        .collect();

    // First-writer wins (stable-ish: par order is non-deterministic — acceptable for exports).
    let mut exports: HashMap<String, Id> = HashMap::new();
    for batch in partial {
        for (export, id) in batch {
            exports.entry(export).or_insert(id);
        }
    }
    exports
}

/// Safe tail slice at a byte index — only when `at` is a UTF-8 char boundary.
fn str_at(src: &str, at: usize) -> &str {
    if at <= src.len() && src.is_char_boundary(at) {
        &src[at..]
    } else {
        ""
    }
}

/// `m.def("name", &fn)` / `.def("name", fn)` → (export, optional C++ target bare name).
///
/// Scans on **bytes** only for the `.def` needle — never `&src[i..i+n]` while walking
/// every byte index (that panics mid multi-byte UTF-8 like `—` / `→` in pybind headers).
pub fn parse_mdef_bindings(src: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0usize;
    while i + 4 < bytes.len() {
        // Match `.def` via ASCII bytes only (covers `m.def`, `mod.def`, chained `.def`).
        // Do **not** use `src[i+1..i+4]` — end index can land inside a multi-byte char.
        if bytes[i] == b'.'
            && bytes[i + 1] == b'd'
            && bytes[i + 2] == b'e'
            && bytes[i + 3] == b'f'
            && matches!(
                bytes.get(i + 4),
                Some(b'(') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
            )
        {
            let mut j = i + 4;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'(' {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                // After ASCII `.def(`, `j` is at a char boundary (only ASCII skipped).
                if let Some((export, after_str)) = parse_c_string(str_at(src, j)) {
                    j += after_str;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    let mut target: Option<String> = None;
                    if j < bytes.len() && bytes[j] == b',' {
                        j += 1;
                        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                            j += 1;
                        }
                        // Skip address-of
                        if j < bytes.len() && bytes[j] == b'&' {
                            j += 1;
                        }
                        // Lambda / cast → no resolvable free function
                        if j < bytes.len() && bytes[j] != b'[' {
                            if let Some((ident, _)) = parse_cpp_ident_path(str_at(src, j)) {
                                // Bare name or last segment of Qualified::name
                                let bare = ident
                                    .rsplit("::")
                                    .next()
                                    .unwrap_or(ident.as_str())
                                    .to_string();
                                if is_ident(&bare) && bare != "py" && bare != "std" {
                                    target = Some(bare);
                                }
                            }
                        }
                    }
                    if is_ident(&export) {
                        out.push((export, target));
                    }
                }
            }
        }
        i += 1;
    }
    out
}

fn is_c_or_cpp(b: &BlockInfo) -> bool {
    matches!(b.lang.as_str(), "c" | "cpp" | "c++" | "cxx")
}

/// Macro / harness shells that must never be Export endpoints (`TEST_SUBMODULE`, etc.).
///
/// Tree-sitter often forms these as function-like nodes; using them as m.def hosts
/// makes Python `test_*` Trace to junk under high|bridge-export (silence > invent).
pub fn is_junk_native_export_host(name: &str) -> bool {
    if name.is_empty() || name == "unknown" {
        return true;
    }
    if name.starts_with("TEST_") || name.starts_with("PYBIND11_") {
        return true;
    }
    // SCREAMING_SNAKE macros (len≥6, has `_`)
    let mut has_us = false;
    let mut all_macro = true;
    let mut n = 0usize;
    for c in name.chars() {
        n += 1;
        if c == '_' {
            has_us = true;
        } else if !(c.is_ascii_uppercase() || c.is_ascii_digit()) {
            all_macro = false;
            break;
        }
    }
    all_macro && has_us && n >= 6
}

fn index_native_functions(graph: &CodeGraph) -> HashMap<String, Id> {
    let candidates: Vec<&BlockInfo> = graph
        .nodes
        .values()
        .filter(|b| {
            if !is_c_or_cpp(b) || b.name.is_empty() || is_junk_native_export_host(&b.name) {
                return false;
            }
            let k = b.kind.to_ascii_lowercase();
            k.contains("function_definition") || k.contains("method_definition")
        })
        .collect();

    // Parallel fold → best (Id, score) per name — no per-node Mutex.
    let best = candidates
        .par_iter()
        .fold(HashMap::<String, (Id, i32)>::new, |mut acc, b| {
            let score = native_fn_score(b);
            match acc.get(b.name.as_str()) {
                Some((_, prev)) if *prev >= score => {}
                _ => {
                    acc.insert(b.name.clone(), (b.id.clone(), score));
                }
            }
            acc
        })
        .reduce(HashMap::new, |mut a, b| {
            for (name, (id, score)) in b {
                match a.get(&name) {
                    Some((_, prev)) if *prev >= score => {}
                    _ => {
                        a.insert(name, (id, score));
                    }
                }
            }
            a
        });
    best.into_iter().map(|(n, (id, _))| (n, id)).collect()
}

fn native_fn_score(b: &BlockInfo) -> i32 {
    let f = b.file.to_string_lossy().to_ascii_lowercase();
    let mut s = 0i32;
    if f.contains("/tests/") || f.contains("/test/") {
        s -= 5;
    }
    if f.contains("/include/") || f.contains("/src/") {
        s += 10;
    }
    s += (b.name.len() as i32).min(30);
    s
}

fn enclosing_def_block<'a>(
    blocks: &[&'a BlockInfo],
    src: &str,
    export: &str,
) -> Option<&'a BlockInfo> {
    let needle = format!("\"{export}\"");
    let pos = src.find(&needle)?;
    let mut best: Option<&BlockInfo> = None;
    let mut best_span = usize::MAX;
    for b in blocks {
        if b.start_byte < b.end_byte && pos >= b.start_byte && pos < b.end_byte {
            let span = b.end_byte.saturating_sub(b.start_byte);
            let k = b.kind.to_ascii_lowercase();
            if !(k.contains("function") || k.contains("method") || k.contains("class")) {
                continue;
            }
            if span < best_span {
                best_span = span;
                best = Some(*b);
            }
        }
    }
    best
}

fn parse_c_string(s: &str) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let quote = bytes[0];
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let mut i = 1usize;
    let mut out = String::new();
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            out.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return Some((out, i + 1));
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    None
}

fn parse_cpp_ident_path(s: &str) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    if i < bytes.len() && bytes[i] == b'(' {
        // cast form (Type)fn — skip
        return None;
    }
    let start = i;
    // Allow Namespace::name and trailing
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphanumeric() || c == '_' || c == ':' {
            i += 1;
        } else {
            break;
        }
    }
    if i == start {
        return None;
    }
    let ident = s[start..i].to_string();
    if ident.chars().all(|c| c == ':') {
        return None;
    }
    Some((ident, i))
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

fn join_sources(blocks: &[&BlockInfo]) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_host_names_detected() {
        assert!(is_junk_native_export_host("TEST_SUBMODULE"));
        assert!(is_junk_native_export_host("PYBIND11_MODULE"));
        assert!(is_junk_native_export_host("TEST_CASE"));
        assert!(!is_junk_native_export_host("pass_cptr_base"));
        assert!(!is_junk_native_export_host("test_function1"));
        assert!(!is_junk_native_export_host("log_operation"));
    }

    #[test]
    fn lambda_mdef_does_not_export_to_test_submodule_shell() {
        use crate::snooper::model::{BlockInfo, CodeGraph, Id};
        use std::collections::HashSet;
        use std::path::PathBuf;

        let mut g = CodeGraph::new();
        let src =
            "TEST_SUBMODULE(pytypes, m) { m.def(\"get_bool\", []{ return false; }); }".to_string();
        let shell = BlockInfo {
            id: Id::new("tests/test_pytypes.cpp", "function_definition", "shellhash"),
            name: "TEST_SUBMODULE".into(),
            file: PathBuf::from("tests/test_pytypes.cpp"),
            kind: "function_definition".into(),
            lang: "cpp".into(),
            start_line: 1,
            end_line: 3,
            start_byte: 0,
            end_byte: src.len(),
            parent_id: None,
            children: vec![],
            content_hash: "shellhash".into(),
            sig_hash: "sig".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: src,
            score: 0.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        };
        g.add_block(shell);
        let exports = collect_pybind_mdef_exports(&g, None);
        assert!(
            !exports.values().any(|id| {
                g.nodes
                    .get(id)
                    .map(|b| b.name == "TEST_SUBMODULE")
                    .unwrap_or(false)
            }),
            "exports must not target TEST_SUBMODULE: {exports:?}"
        );
        // get_bool lambda has no free-fn target and only junk host → no entry
        assert!(
            !exports.contains_key("get_bool"),
            "lambda-only m.def must silence rather than invent host: {exports:?}"
        );
    }

    #[test]
    fn mdef_export_and_target() {
        let src = r#"
TEST_SUBMODULE(mod, m) {
    m.def("pass_cptr_base", pass_cptr_base);
    m.def("rtrn_mptr", &rtrn_mptr_drvd, rvto);
    m.def("lambda_only", [](int x) { return x; });
}
"#;
        let pairs = parse_mdef_bindings(src);
        assert!(
            pairs
                .iter()
                .any(|(e, t)| e == "pass_cptr_base" && t.as_deref() == Some("pass_cptr_base")),
            "{pairs:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(e, t)| e == "rtrn_mptr" && t.as_deref() == Some("rtrn_mptr_drvd")),
            "{pairs:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(e, t)| e == "lambda_only" && t.is_none()),
            "{pairs:?}"
        );
    }

    #[test]
    fn mdef_scan_survives_multibyte_utf8_near_dot() {
        // Regression: byte-walk + `src[i+1..i+4]` panicked inside '—' / '→' (pybind headers).
        let src = "\
// Copyright — pybind Community → conduit
// .def not a call: x.—y and z.→w noise
m.def(\"safe_export\", &safe_fn);
/* cast.h: C++ → Python */
";
        let pairs = parse_mdef_bindings(src);
        assert!(
            pairs
                .iter()
                .any(|(e, t)| e == "safe_export" && t.as_deref() == Some("safe_fn")),
            "{pairs:?}"
        );
    }
}
