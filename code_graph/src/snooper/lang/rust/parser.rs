// code_graph/src/snooper/lang/rust/parser.rs
//
// Block extraction / Phase 1 parsing for Rust.
// Moved from monolithic rust.rs via Strangler Fig.
// Responsible for Tree-sitter parse + visit_node to collect interesting structural blocks
// (functions, structs, impls, etc.). Edge building is deliberately deferred.

use crate::snooper::parser::ParseError;
use crate::BlockInfo;
use std::collections::HashSet;
use std::path::PathBuf;
use tree_sitter::{Node, Parser};

// (no longer needs GENERIC_NAMES or CALL_QUERY; those are edges concerns)

/// Parse phase: Run Tree-sitter parse + visit_node to collect interesting blocks.
/// Edge building (build_call_edges / build_usage_edges) is deliberately deferred
/// to a later phase so the whole project can be parsed first, then connections curated
/// (potentially in parallel).
pub fn parse(
    path: PathBuf,
    source: &str,
) -> Result<crate::snooper::parser::ParsedFile, ParseError> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser
        .set_language(&language.into())
        .map_err(|e| ParseError::GrammarLoad(e.to_string()))?;

    let tree = parser.parse(source, None).ok_or(ParseError::ParseFailed)?;

    let root = tree.root_node();

    let mut blocks = Vec::new();
    let config = super::super::generic_parser::VisitConfig {
        interesting_kinds: &[
            "function_item",
            "struct_item",
            "enum_item",
            "union_item",
            "trait_item",
            "impl_item",
            "mod_item",
            "type_item",
            "const_item",
            "static_item",
            // More structural for richer WL / edges in training:
            "if_expression",
            "if_let_expression",
            "for_expression",
            "while_expression",
            "loop_expression",
            "match_expression",
            "match_arm",
            "call_expression",
            "return_expression",
            "let_declaration",
            "let_statement",
            "assignment_expression",
        ],
        lang: "rust",
        extract_name,
        get_start: rust_get_start,
        extract_externals: extract_external_crates,
    };
    super::super::generic_parser::visit_node(
        root,
        path.clone(),
        source,
        None,
        &mut blocks,
        config,
        "unknown",
    );

    // Edge building is no longer done here — see scanner for the collecting + edge phases.

    Ok(crate::snooper::parser::ParsedFile {
        path,
        source: source.to_string(),
        blocks,
        tree: Some(tree),
    })
}

fn rust_get_start(node: &Node, _source: &str) -> (usize, usize) {
    let mut start_line = node.start_position().row + 1;
    let mut start_byte = node.start_byte();

    if let Some(prev) = node.prev_sibling() {
        if prev.kind() == "attribute_item" {
            start_line = prev.start_position().row + 1;
            start_byte = prev.start_byte();
        }
    }
    (start_line, start_byte)
}

fn extract_name(node: &Node, source: &str) -> Option<String> {
    // Prefer grammar `name` field — walking children hits `crate` inside `pub(crate)`.
    if let Some(name_node) = node.child_by_field_name("name") {
        let s = source[name_node.start_byte()..name_node.end_byte()].to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Skip visibility (`pub(crate)` contains identifier "crate").
        if child.kind() == "visibility_modifier" {
            continue;
        }
        if child.kind() == "identifier" || child.kind() == "type_identifier" {
            return Some(source[child.start_byte()..child.end_byte()].to_string());
        }
    }
    None
}

// FIXED: Now extracts names for structs, impls, traits (not just functions)
pub(crate) fn extract_name_from_block(block: &BlockInfo, source: &str) -> Option<String> {
    let block_source = &source[block.start_byte..block.end_byte];

    // 1. Try function pattern
    if let Some(fn_pos) = block_source.find("fn ") {
        let after = &block_source[fn_pos + 3..];
        return after.split_whitespace().next().map(|s| {
            s.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        });
    }

    // 2. Try struct / enum / union pattern
    if let Some(struct_pos) = block_source.find("struct ") {
        let after = &block_source[struct_pos + 7..];
        return after.split_whitespace().next().map(|s| {
            s.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        });
    }
    if let Some(enum_pos) = block_source.find("enum ") {
        let after = &block_source[enum_pos + 5..];
        return after.split_whitespace().next().map(|s| {
            s.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        });
    }
    if let Some(union_pos) = block_source.find("union ") {
        let after = &block_source[union_pos + 6..];
        return after.split_whitespace().next().map(|s| {
            s.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        });
    }

    // 3. Try impl / trait pattern (common for methods)
    if let Some(impl_pos) = block_source.find("impl ") {
        let after = &block_source[impl_pos + 5..];
        return after.split_whitespace().next().map(|s| {
            s.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        });
    }
    if let Some(trait_pos) = block_source.find("trait ") {
        let after = &block_source[trait_pos + 6..];
        return after.split_whitespace().next().map(|s| {
            s.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        });
    }

    None
}

fn extract_external_crates(node: Node, source: &str) -> HashSet<String> {
    let mut crates = HashSet::new();

    fn scan_attribute(attr_text: &str, crates: &mut HashSet<String>) {
        if !attr_text.contains("derive") {
            return;
        }
        if let Some(start) = attr_text.find('(') {
            if let Some(end) = attr_text.rfind(')') {
                let inside = &attr_text[start + 1..end];
                for token in inside.split(',') {
                    let token = token.trim();
                    match token {
                        "Serialize" | "Deserialize" => {
                            let _ = crates.insert("serde".to_string());
                        }
                        "Parser" | "Subcommand" | "Args" | "ValueEnum" => {
                            let _ = crates.insert("clap".to_string());
                        }
                        "Error" => {
                            let _ = crates.insert("thiserror".to_string());
                        }
                        "async_trait" => {
                            let _ = crates.insert("async_trait".to_string());
                        }
                        "Message" => {
                            let _ = crates.insert("prost".to_string());
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let mut cursor = node.walk();

    // 1. Scan direct children (use statements + inline attributes)
    for child in node.children(&mut cursor) {
        match child.kind() {
            "use_declaration" => {
                if let Some(use_text) = source.get(child.start_byte()..child.end_byte()) {
                    if let Some(after_use) = use_text.strip_prefix("use ") {
                        if let Some(first) = after_use.split("::").next() {
                            let first = first.trim();
                            if !first.is_empty()
                                && !["std", "core", "alloc", "crate", "super", "self"]
                                    .contains(&first)
                            {
                                crates.insert(first.to_string());
                            }
                        }
                    }
                }
            }
            "attribute_item" => {
                if let Some(attr_text) = source.get(child.start_byte()..child.end_byte()) {
                    scan_attribute(attr_text, &mut crates);
                }
            }
            _ => {}
        }
    }

    // 2. CRITICAL FIX: Scan sibling attributes (#[derive] before/after struct/enum)
    if let Some(prev) = node.prev_sibling() {
        if prev.kind() == "attribute_item" {
            if let Some(attr_text) = source.get(prev.start_byte()..prev.end_byte()) {
                scan_attribute(attr_text, &mut crates);
            }
        }
    }
    if let Some(next) = node.next_sibling() {
        if next.kind() == "attribute_item" {
            if let Some(attr_text) = source.get(next.start_byte()..next.end_byte()) {
                scan_attribute(attr_text, &mut crates);
            }
        }
    }

    crates
}
