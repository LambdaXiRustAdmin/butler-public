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
    // Product warehouse definition-tier only (Hop A). Statement/expression AST
    // kinds are training-grain — FullEdge still sees call sites via Tree-sitter
    // queries on the file tree, not as permanent warehouse nodes.
    let config = super::super::generic_parser::VisitConfig {
        interesting_kinds: &[
            "function_definition",
            "class_definition",
            "async_function_definition",
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

#[cfg(test)]
mod definition_tier_tests {
    use super::*;
    use std::path::PathBuf;

    /// Product inventory must not materialize statement/expression AST as warehouse nodes.
    #[test]
    fn python_parse_emits_definition_kinds_only() {
        let src = r#"
def foo():
    x = 1
    bar()
    if x:
        return x

class C:
    def meth(self):
        pass
"#;
        let parsed = parse(PathBuf::from("mod.py"), src).expect("parse");
        assert!(!parsed.blocks.is_empty());
        for b in &parsed.blocks {
            assert!(
                matches!(
                    b.kind.as_str(),
                    "function_definition" | "async_function_definition" | "class_definition"
                ),
                "non-definition product node: kind={} name={}",
                b.kind,
                b.name
            );
        }
        let names: Vec<_> = parsed.blocks.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"foo"), "{names:?}");
        assert!(names.contains(&"C"), "{names:?}");
        assert!(names.contains(&"meth"), "{names:?}");
        // Statement grain must not appear.
        assert!(!parsed.blocks.iter().any(|b| b.kind == "expression_statement"));
        assert!(!parsed.blocks.iter().any(|b| b.kind == "call"));
        assert!(!parsed.blocks.iter().any(|b| b.kind == "assignment"));
    }

    #[test]
    fn python_call_edges_without_statement_nodes() {
        let src = r#"
def bar():
    pass

def foo():
    bar()
"#;
        let parsed = parse(PathBuf::from("calls.py"), src).expect("parse");
        let tree = parsed.tree.as_ref().expect("tree");
        let edges = crate::snooper::lang::python::edges::collect_call_edges(
            &parsed.blocks,
            &parsed.source,
            tree,
            None,
        );
        let foo = parsed
            .blocks
            .iter()
            .find(|b| b.name == "foo")
            .expect("foo");
        let bar = parsed
            .blocks
            .iter()
            .find(|b| b.name == "bar")
            .expect("bar");
        assert!(
            edges.iter().any(|(f, t)| f == &foo.id && t == &bar.id),
            "foo→bar CALL expected without statement warehouse nodes: {edges:?}"
        );
    }
}
