// code_graph/src/snooper/lang/generic_edges.rs
//
// Call/usage edge collection. CALLS edges resolve only to **callee kinds**
// (functions/methods) — never to types/structs that merely appear in signatures.
//
// Config: caller_kinds (where call sites live) vs callee_kinds (valid CALL targets).

use crate::{BlockInfo, CodeGraph, Id};
use std::collections::HashMap;
use tree_sitter::{Query, QueryCursor, StreamingIterator};

/// How aggressive the contains-fallback is when resolving call names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FallbackStyle {
    /// Rust / Python: skip ultra-short + pure-lowercase tokens.
    Aggressive,
    /// Go / C: only skip tiny names; idiomatic lowercase / snake_case OK.
    GoIdiomatic,
    /// TS/JS: **no** body-scan fallback — only Tree-sitter call_expression captures.
    /// Aggressive fallback was name-soup on templates (t3 `Home` → CLI helpers).
    QueryOnly,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_call_edges(
    blocks: &[BlockInfo],
    source: &str,
    tree: &tree_sitter::Tree,
    graph: &mut CodeGraph,
    caller_kinds: &[&str],
    callee_kinds: &[&str],
    make_query: impl FnOnce() -> Result<Query, tree_sitter::QueryError>,
    get_name: impl Fn(&BlockInfo, &str) -> Option<String>,
    generic_names: &[&str],
    fallback: FallbackStyle,
) {
    let edges = collect_call_edges(
        blocks,
        source,
        tree,
        None,
        caller_kinds,
        callee_kinds,
        make_query,
        get_name,
        generic_names,
        fallback,
    );
    for (from, to) in edges {
        graph.add_edge(from, to);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_call_edges(
    blocks: &[BlockInfo],
    source: &str,
    tree: &tree_sitter::Tree,
    global_names: Option<&HashMap<String, Id>>,
    caller_kinds: &[&str],
    callee_kinds: &[&str],
    make_query: impl FnOnce() -> Result<Query, tree_sitter::QueryError>,
    get_name: impl Fn(&BlockInfo, &str) -> Option<String>,
    generic_names: &[&str],
    fallback: FallbackStyle,
) -> Vec<(Id, Id)> {
    let mut edges = Vec::new();

    // Local CALL targets only — never structs/types (signature tokens).
    // Same name: prefer function_definition over function_declaration.
    let mut local_name_to_id: HashMap<String, Id> = HashMap::new();
    let mut local_score: HashMap<String, i32> = HashMap::new();
    for b in blocks.iter().filter(|b| callee_kinds.contains(&b.kind.as_str())) {
        let Some(n) = get_name(b, source) else {
            continue;
        };
        if n.is_empty() {
            continue;
        }
        let sc = local_callee_preference(b);
        match local_score.get(&n) {
            Some(&prev) if prev >= sc => {}
            _ => {
                local_score.insert(n.clone(), sc);
                local_name_to_id.insert(n, b.id.clone());
            }
        }
    }

    // Id → kind for optional global target validation when ids come from the warehouse map.
    let id_is_local_callee: HashMap<&Id, bool> = blocks
        .iter()
        .map(|b| (&b.id, callee_kinds.contains(&b.kind.as_str())))
        .collect();

    let query = match make_query() {
        Ok(q) => q,
        Err(e) => {
            eprintln!("⚠️ Tree-sitter call query error: {}", e);
            return edges;
        }
    };

    let mut cursor = QueryCursor::new();
    let root = tree.root_node();

    let call_captures: Vec<_> = {
        let mut caps = Vec::new();
        let mut matches = cursor.matches(&query, root, source.as_bytes());
        while let Some(mat) = matches.next() {
            for c in mat.captures {
                if c.index == 0 {
                    caps.push(c.node);
                }
            }
        }
        caps
    };

    for block in blocks
        .iter()
        .filter(|b| caller_kinds.contains(&b.kind.as_str()))
    {
        let (bs, be) = (block.start_byte, block.end_byte);
        for call_node in call_captures
            .iter()
            .filter(|n| n.start_byte() >= bs && n.end_byte() <= be)
        {
            let raw = &source[call_node.start_byte()..call_node.end_byte()];
            let name = raw.trim();
            // JSX: only PascalCase / member components — never HTML/SVG intrinsics (`<main>`, `<div>`).
            if jsx_tag_is_dom_intrinsic(call_node, name) {
                continue;
            }
            if let Some(target_id) =
                resolve_target(name, &local_name_to_id, global_names, generic_names)
            {
                if target_id != &block.id
                    && global_or_local_ok(target_id, &local_name_to_id, &id_is_local_callee)
                {
                    edges.push((block.id.clone(), target_id.clone()));
                }
            }
        }
    }

    // Fallback: body-scan for known callees (skipped entirely for QueryOnly / TS).
    if !matches!(fallback, FallbackStyle::QueryOnly) {
        for block in blocks
            .iter()
            .filter(|b| caller_kinds.contains(&b.kind.as_str()))
        {
            if block.start_byte >= block.end_byte || block.end_byte > source.len() {
                continue;
            }
            let block_source = &source[block.start_byte..block.end_byte];
            let candidates = local_name_to_id.iter().map(|(n, t)| (n.as_str(), t)).chain(
                global_names
                    .iter()
                    .flat_map(|g| g.iter())
                    .filter(|(n, _)| !local_name_to_id.contains_key(*n))
                    .filter(|(n, _)| !generic_names.contains(&n.as_str()))
                    .map(|(n, t)| (n.as_str(), t)),
            );
            for (name, target_id) in candidates {
                if should_skip_fallback(name, fallback) {
                    continue;
                }
                if contains_word_boundary(block_source, name) && target_id != &block.id {
                    edges.push((block.id.clone(), target_id.clone()));
                }
            }
        }
    }

    edges
}

/// Local map always function-like. Global map is built as call-targets only (builder).
fn global_or_local_ok(
    target_id: &Id,
    local: &HashMap<String, Id>,
    id_is_local_callee: &HashMap<&Id, bool>,
) -> bool {
    if local.values().any(|id| id == target_id) {
        return true;
    }
    // Global warehouse ids: kind not on hand here; map is pre-filtered to call targets.
    // If id is local but not a callee kind, reject.
    match id_is_local_callee.get(target_id) {
        Some(false) => false,
        _ => true,
    }
}

/// Prefer bodies over prototypes when both exist in the same file.
fn local_callee_preference(b: &BlockInfo) -> i32 {
    let k = b.kind.to_ascii_lowercase();
    let mut s = 0i32;
    if k.contains("function_definition") {
        s += 30;
    } else if k.contains("function_item") || k.contains("method") {
        s += 25;
    } else if k.contains("function_declaration") || k.contains("arrow_function") {
        s += 10;
    } else {
        s += 5;
    }
    let f = b.file.to_string_lossy().to_ascii_lowercase();
    if f.contains("_test.") || f.contains("/test/") || f.contains("/tests/") {
        s -= 40;
    }
    s
}

fn resolve_target<'a>(
    name: &str,
    local: &'a HashMap<String, Id>,
    global: Option<&'a HashMap<String, Id>>,
    generic_names: &[&str],
) -> Option<&'a Id> {
    if let Some(id) = local.get(name) {
        return Some(id);
    }
    if let Some(g) = global {
        if !generic_names.contains(&name) {
            if let Some(id) = g.get(name) {
                return Some(id);
            }
        }
    }
    if let Some(short) = name.rsplit('.').next() {
        if short != name {
            if let Some(id) = local.get(short) {
                return Some(id);
            }
            if let Some(g) = global {
                if !generic_names.contains(&short) {
                    if let Some(id) = g.get(short) {
                        return Some(id);
                    }
                }
            }
        }
    }
    None
}

/// True when `name_node` is a JSX tag name that is a DOM intrinsic (lowercase, no member).
fn jsx_tag_is_dom_intrinsic(name_node: &tree_sitter::Node, name: &str) -> bool {
    let Some(parent) = name_node.parent() else {
        return false;
    };
    let pk = parent.kind();
    if pk != "jsx_opening_element" && pk != "jsx_self_closing_element" {
        // Capture may be nested; check grandparent.
        if let Some(gp) = parent.parent() {
            let gk = gp.kind();
            if gk != "jsx_opening_element" && gk != "jsx_self_closing_element" {
                return false;
            }
        } else {
            return false;
        }
    }
    // Member expressions (Form.Item, motion.div) — keep; only bare lowercase tags are DOM.
    if name.contains('.') {
        return false;
    }
    match name.chars().next() {
        Some(c) if c.is_ascii_uppercase() => false, // React component
        Some(c) if c.is_ascii_lowercase() => true,  // html/svg intrinsic
        _ => false,
    }
}

fn should_skip_fallback(name: &str, style: FallbackStyle) -> bool {
    const UNIVERSAL_STOP: &[&str] = &["With", "Query", "Result", "Option", "String", "Self"];
    if UNIVERSAL_STOP.contains(&name) {
        return true;
    }
    // Common C type tokens that slip past kind filters if mis-indexed.
    const TYPEISH: &[&str] = &[
        "void", "int", "char", "long", "short", "float", "double", "unsigned", "signed", "const",
        "size_t", "ssize_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "int8_t", "int16_t",
        "int32_t", "int64_t", "bool", "BOOL", "TRUE", "FALSE", "NULL", "u_int", "u_char",
    ];
    if TYPEISH.iter().any(|t| t.eq_ignore_ascii_case(name)) {
        return true;
    }
    match style {
        FallbackStyle::QueryOnly => true, // unused — fallback loop skipped
        FallbackStyle::GoIdiomatic => name.len() <= 2,
        FallbackStyle::Aggressive => {
            name.len() <= 3
                || (name.chars().all(|c| c.is_ascii_lowercase()) && !name.contains('_'))
        }
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    #[test]
    fn go_fallback_keeps_idiomatic_lowercase() {
        assert!(!should_skip_fallback("reloader", FallbackStyle::GoIdiomatic));
        assert!(!should_skip_fallback("reload", FallbackStyle::GoIdiomatic));
        assert!(!should_skip_fallback("newScrapePool", FallbackStyle::GoIdiomatic));
        assert!(should_skip_fallback("x", FallbackStyle::GoIdiomatic));
    }

    #[test]
    fn aggressive_fallback_still_drops_pure_lower_temps() {
        assert!(should_skip_fallback("reloader", FallbackStyle::Aggressive));
        assert!(should_skip_fallback("foo", FallbackStyle::Aggressive));
        assert!(!should_skip_fallback("server_start", FallbackStyle::Aggressive));
        assert!(!should_skip_fallback("newScrapePool", FallbackStyle::Aggressive));
    }

    #[test]
    fn typeish_names_skipped_in_fallback() {
        assert!(should_skip_fallback("void", FallbackStyle::GoIdiomatic));
        assert!(should_skip_fallback("u_int", FallbackStyle::GoIdiomatic));
        assert!(should_skip_fallback("GLFWwindow", FallbackStyle::GoIdiomatic) == false);
        // GLFWwindow is not in TYPEISH — kind filter must drop it as struct
    }
}

pub(crate) fn build_usage_edges(
    blocks: &[BlockInfo],
    _source: &str,
    _tree: &tree_sitter::Tree,
    graph: &mut CodeGraph,
) {
    let edges = collect_usage_edges(blocks, _source, _tree);
    for (from, to) in edges {
        graph.add_edge(from, to);
    }
}

pub(crate) fn collect_usage_edges(
    blocks: &[BlockInfo],
    _source: &str,
    _tree: &tree_sitter::Tree,
) -> Vec<(Id, Id)> {
    const TYPE_KINDS: &[&str] = &[
        "struct_item",
        "enum_item",
        "union_item",
        "trait_item",
        "type_item",
        "class_definition",
        "class_declaration",
        "interface_declaration",
        "type_spec",
        "struct_specifier",
        "class_specifier",
    ];

    let type_name_to_id: HashMap<String, Id> = blocks
        .iter()
        .filter(|b| TYPE_KINDS.contains(&b.kind.as_str()) && !b.name.is_empty())
        .map(|b| (b.name.clone(), b.id.clone()))
        .collect();

    blocks
        .iter()
        .filter(|b| !TYPE_KINDS.contains(&b.kind.as_str()))
        .flat_map(|b| {
            type_name_to_id
                .iter()
                .filter(|(tname, tid)| contains_word_boundary(&b.source, tname) && *tid != &b.id)
                .map(|(_, tid)| (b.id.clone(), tid.clone()))
        })
        .collect()
}

pub(crate) fn contains_word_boundary(text: &str, word: &str) -> bool {
    if word.is_empty() || !text.contains(word) {
        return false;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let word_len = word.len();
    let mut start = 0usize;
    while let Some(pos) = text[start..].find(word) {
        let abs = start + pos;
        if !text.is_char_boundary(abs) {
            start = abs + 1;
            if start >= text.len() {
                break;
            }
            continue;
        }
        let before_ok = abs == 0 || {
            let mut prev = abs;
            while prev > 0 && !text.is_char_boundary(prev - 1) {
                prev -= 1;
            }
            if prev == 0 {
                true
            } else {
                let prev_char = text[prev - 1..abs].chars().last().unwrap_or(' ');
                !is_word(prev_char)
            }
        };
        let after_pos = abs + word_len;
        let after_ok = after_pos == text.len() || {
            if !text.is_char_boundary(after_pos) {
                false
            } else {
                let next_char = text[after_pos..].chars().next().unwrap_or(' ');
                !is_word(next_char)
            }
        };
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
        if start >= text.len() {
            break;
        }
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::contains_word_boundary;

    #[test]
    fn test_contains_word_boundary_unicode() {
        let source =
            "pub fn 中文名称的加法API(left: usize, right: usize) -> usize {\n    left + right\n}";
        assert!(contains_word_boundary(source, "中文名称的加法API"));
        assert!(!contains_word_boundary(source, "加法API"));
        assert!(!contains_word_boundary(source, "名称的加"));
        assert!(contains_word_boundary("fn foo_bar() {}", "foo_bar"));
        assert!(!contains_word_boundary("fn foobar() {}", "foo_bar"));
        assert!(contains_word_boundary("fn add(left, right)", "add"));
        assert!(!contains_word_boundary("fn adding", "add"));
        let src2 = "fn 加法(left: i32) {}";
        assert!(contains_word_boundary(src2, "加法"));
    }
}
