// code_graph/src/snooper/lang/python/parser.rs
//
// Block extraction / Phase 1 parsing for Python.
// Moved from monolithic python.rs via directory split to match Rust (lang/rust/parser.rs).
//
// Responsible for Tree-sitter parse + visit_node to collect interesting structural blocks
// (functions, classes, async functions). Edge building is deliberately deferred.

use crate::snooper::parser::ParseError;
use std::path::PathBuf;
use tree_sitter::{Node, Parser};

/// Parse phase: Run Tree-sitter parse + visit_node to collect interesting blocks.
/// Edge building (build_call_edges / build_usage_edges) is deliberately deferred
/// to a later phase so the whole project can be parsed first, then connections curated
/// (potentially in parallel).
pub fn parse(
    path: PathBuf,
    source: &str,
) -> Result<crate::snooper::parser::ParsedFile, ParseError> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser
        .set_language(&language.into())
        .map_err(|e| ParseError::GrammarLoad(e.to_string()))?;

    let tree = parser.parse(source, None).ok_or(ParseError::ParseFailed)?;
    let root = tree.root_node();

    let mut blocks = Vec::new();
    let config = super::super::generic_parser::VisitConfig {
        interesting_kinds: &[
            "function_definition",
            "class_definition",
            "async_function_definition",
            // Richer for WL/edges:
            "if_statement",
            "for_statement",
            "while_statement",
            "call",
            "return_statement",
            "assignment",
            "expression_statement",
        ],
        lang: "python",
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
        "unknown",
    );

    Ok(crate::snooper::parser::ParsedFile {
        path,
        source: source.to_string(),
        blocks,
        tree: Some(tree),
    })
}

fn extract_name(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return Some(source[child.start_byte()..child.end_byte()].to_string());
        }
    }
    None
}
