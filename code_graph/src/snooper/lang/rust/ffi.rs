//! Rust-side FFI export discovery (PyO3 / similar).
//!
//! Lang drawer: only Rust attribute / export mechanics live here.
//! Linking into Python is orchestrated from the polyglot linker using
//! [`crate::snooper::lang::python::ffi`].

use crate::snooper::model::{BlockInfo, CodeGraph, Id};
use crate::snooper::project_paths::ProjectPaths;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;

/// Scan Rust `function_item` blocks for `#[pyfunction]` exports.
/// Returns **export name** (Python-visible) → block Id.
///
/// **Rayon read-map** — multi-core scan; no graph mutation.
pub fn collect_pyfunction_exports(
    graph: &CodeGraph,
    project_root: Option<&Path>,
) -> HashMap<String, Id> {
    let pp = project_root.map(ProjectPaths::new);
    let candidates: Vec<&BlockInfo> = graph
        .nodes
        .values()
        .filter(|b| b.lang == "rust" && b.kind == "function_item" && !b.name.is_empty())
        .collect();
    candidates
        .par_iter()
        .filter_map(|b| {
            let src = block_source_with_attrs(b, pp.as_ref())?;
            let export = parse_pyfunction_export_name(&src, &b.name)?;
            Some((export, b.id.clone()))
        })
        .collect()
}

/// `#[pyfunction]` / `#[pyfunction(name = "foo")]` **immediately before** this `fn`.
///
/// Must not treat a previous neighbor's `#[pyfunction]` (from a wide byte window) as
/// attaching to `count_line` / helpers — that polluted the export table.
pub fn parse_pyfunction_export_name(fn_source: &str, rust_name: &str) -> Option<String> {
    let lines: Vec<&str> = fn_source.lines().collect();
    // Locate the `fn name` line for this item (word-boundary on name).
    let fn_line_idx = lines.iter().position(|l| line_declares_fn(l, rust_name))?;

    // Walk upward: only attrs / empty / comments / doc belong to this fn.
    let mut attr_blob = String::new();
    let mut i = fn_line_idx;
    while i > 0 {
        i -= 1;
        let t = lines[i].trim();
        if t.is_empty() || t.starts_with("//") || t.starts_with("///") || t.starts_with("#!") {
            continue;
        }
        if t.starts_with("#[") || t.starts_with("#!") {
            attr_blob.insert_str(0, &format!("{}\n", lines[i]));
            continue;
        }
        // Hit prior item (another fn/struct/use) — stop. Do not inherit its attrs.
        break;
    }

    let lower = attr_blob.to_ascii_lowercase();
    // Only real export attrs — not `#[pymodule]` / random pyo3 noise from neighbors.
    if !lower.contains("pyfunction") {
        return None;
    }
    for key in ["name = \"", "name=\"", "name = '", "name='"] {
        if let Some(idx) = attr_blob.find(key) {
            let rest = &attr_blob[idx + key.len()..];
            let quote = if key.ends_with('\'') { '\'' } else { '"' };
            if let Some(end) = rest.find(quote) {
                let n = rest[..end].trim();
                if !n.is_empty() && is_ident(n) {
                    return Some(n.to_string());
                }
            }
        }
    }
    Some(rust_name.to_string())
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

fn line_declares_fn(line: &str, rust_name: &str) -> bool {
    let t = line.trim_start();
    let rest = if let Some(r) = t.strip_prefix("pub(crate) fn ") {
        r
    } else if let Some(r) = t.strip_prefix("pub fn ") {
        r
    } else if let Some(r) = t.strip_prefix("fn ") {
        r
    } else {
        return false;
    };
    if !rest.starts_with(rust_name) {
        return false;
    }
    match rest[rust_name.len()..].chars().next() {
        None => true,
        Some(c) => !c.is_ascii_alphanumeric() && c != '_',
    }
}

fn block_source_with_attrs(b: &BlockInfo, pp: Option<&ProjectPaths>) -> Option<String> {
    if !b.source.is_empty() {
        // Still may lack attrs if Tree-sitter span starts at `fn` — extend upward carefully.
        if b.source.contains("#[") || b.source.contains("pyfunction") {
            return Some(b.source.clone());
        }
    }
    let paths = pp?;
    let abs = paths.to_abs(&b.file);
    let file = std::fs::read_to_string(abs).ok()?;
    if b.end_byte > file.len() || b.start_byte >= b.end_byte {
        return if b.source.is_empty() {
            None
        } else {
            Some(b.source.clone())
        };
    }
    // Immediate attribute run only (not a fixed 256-byte rewind into previous items).
    let start = attr_run_start(&file, b.start_byte);
    let end = ceil_char_boundary(&file, b.end_byte.min(file.len())).max(start);
    let slice = file[start..end].to_string();
    if !b.source.is_empty() && !slice.contains(&b.source[..b.source.len().min(40)]) {
        // Prefer richer slice when available.
        return Some(slice);
    }
    if !slice.is_empty() {
        Some(slice)
    } else if !b.source.is_empty() {
        Some(b.source.clone())
    } else {
        None
    }
}

/// Walk back from `start_byte` over blank/comment/attribute lines only.
fn attr_run_start(file: &str, start_byte: usize) -> usize {
    let start_byte = floor_char_boundary(file, start_byte.min(file.len()));
    // Beginning of the line that contains start_byte.
    let mut line_start = file[..start_byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
    loop {
        if line_start == 0 {
            return 0;
        }
        let prev_start = file[..line_start.saturating_sub(1)]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prev_line = file[prev_start..line_start].trim();
        if prev_line.is_empty()
            || prev_line.starts_with("//")
            || prev_line.starts_with("///")
            || prev_line.starts_with("#[")
        {
            line_start = prev_start;
            continue;
        }
        break;
    }
    floor_char_boundary(file, line_start)
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pyfunction_export_name_default_and_override() {
        assert_eq!(
            parse_pyfunction_export_name("#[pyfunction]\nfn search(x: &str) {}", "search")
                .as_deref(),
            Some("search")
        );
        assert_eq!(
            parse_pyfunction_export_name(
                "#[pyfunction(name = \"search_py\")]\nfn search(x: &str) {}",
                "search"
            )
            .as_deref(),
            Some("search_py")
        );
        assert!(parse_pyfunction_export_name("fn search(x: &str) {}", "search").is_none());
    }

    #[test]
    fn neighbor_pyfunction_does_not_attach_to_helper() {
        // Simulate 256-byte-style pollution: previous export attrs sit above count_line.
        let polluted = r#"
#[pyfunction]
fn search_sequential_detached(py: Python<'_>, contents: &str, needle: &str) -> usize {
    0
}

/// helper
fn count_line(line: &str, needle: &str) -> usize {
    0
}
"#;
        assert!(
            parse_pyfunction_export_name(polluted, "count_line").is_none(),
            "helper must not inherit neighbor #[pyfunction]"
        );
        assert_eq!(
            parse_pyfunction_export_name(polluted, "search_sequential_detached").as_deref(),
            Some("search_sequential_detached")
        );
    }

    #[test]
    fn attr_prefix_slice_survives_multibyte_utf8() {
        let mut prefix = String::new();
        while prefix.len() < 200 {
            prefix.push_str("// note — box ═ drawing\n");
        }
        let body = "#[pyfunction]\nfn search(x: &str) {}\n";
        let file = format!("{prefix}{body}");
        let fn_start = file.find("fn search").unwrap();
        let start = attr_run_start(&file, fn_start);
        let end = ceil_char_boundary(&file, file.len());
        let slice = &file[start..end];
        assert!(slice.contains("pyfunction"));
        assert_eq!(
            parse_pyfunction_export_name(slice, "search").as_deref(),
            Some("search")
        );
    }
}
