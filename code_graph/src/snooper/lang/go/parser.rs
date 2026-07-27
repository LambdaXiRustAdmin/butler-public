// code_graph/src/snooper/lang/go/parser.rs
//
// Block extraction / Phase 1 parsing for Go.
// Mirrors the structure of lang/typescript/parser.rs and lang/python/parser.rs.
//
// Responsible for Tree-sitter parse + visit_node to collect interesting structural blocks
// (functions, methods, type specs for structs/interfaces). Edge building is deliberately deferred.

use crate::snooper::parser::ParseError;
use std::path::PathBuf;
use tree_sitter::{Node, Parser};

/// Parse phase: Run Tree-sitter parse + visit_node to collect interesting blocks.
/// Edge building is deliberately deferred to a later phase.
pub fn parse(
    path: PathBuf,
    source: &str,
) -> Result<crate::snooper::parser::ParsedFile, ParseError> {
    let mut parser = Parser::new();
    let language = tree_sitter_go::LANGUAGE;
    parser
        .set_language(&language.into())
        .map_err(|e| ParseError::GrammarLoad(e.to_string()))?;

    let tree = parser.parse(source, None).ok_or(ParseError::ParseFailed)?;
    let root = tree.root_node();

    let fallback_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| format!("{}_default", s))
        .unwrap_or_else(|| "unknown".to_string());

    let mut blocks = Vec::new();
    let config = super::super::generic_parser::VisitConfig {
        interesting_kinds: &[
            "function_declaration",
            "method_declaration",
            "type_spec", // covers struct and interface types
            // Richer structural:
            "if_statement",
            "for_statement",
            "range_clause",
            "call_expression",
            "return_statement",
            "short_var_declaration",
            "assignment_statement",
        ],
        lang: "go",
        extract_name,
        get_start: super::super::generic_parser::default_get_start,
        extract_externals: super::super::generic_parser::no_external_crates,
    };
    super::super::generic_parser::visit_node(
        root,
        path.clone(),
        source,
        None,
        &mut blocks,
        config,
        &fallback_name,
    );

    Ok(crate::snooper::parser::ParsedFile {
        path,
        source: source.to_string(),
        blocks,
        tree: Some(tree),
    })
}

fn extract_name(node: &Node, source: &str) -> Option<String> {
    // Prefer explicit "name" field when the grammar provides it (function_declaration,
    // method_declaration, type_spec all declare a named child via the "name" field).
    if let Some(name_node) = node.child_by_field_name("name") {
        let k = name_node.kind();
        if k == "identifier" || k == "field_identifier" || k == "type_identifier" {
            return Some(source[name_node.start_byte()..name_node.end_byte()].to_string());
        }
    }

    // Direct children fallback (covers some declaration forms)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let k = child.kind();
        if k == "identifier" || k == "field_identifier" || k == "type_identifier" {
            return Some(source[child.start_byte()..child.end_byte()].to_string());
        }
    }

    None
}
