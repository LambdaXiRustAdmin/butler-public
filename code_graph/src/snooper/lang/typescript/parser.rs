// code_graph/src/snooper/lang/typescript/parser.rs
//
// Block extraction / Phase 1 parsing for TypeScript/TSX/JS/JSX.
// Mirrors the structure of lang/python/parser.rs and lang/rust/parser.rs.
//
// Responsible for Tree-sitter parse + visit_node to collect interesting structural blocks
// (classes, interfaces, functions, methods, arrows). Edge building is deliberately deferred.

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
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let language = if ext == "tsx" || ext == "jsx" {
        tree_sitter_typescript::LANGUAGE_TSX
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT
    };
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
    // Product warehouse definition-tier only (Hop A). Keep variable_declarator
    // for named const/let export surfaces (import/export edges); drop statement
    // and bare call_expression inventory nodes.
    let config = super::super::generic_parser::VisitConfig {
        interesting_kinds: &[
            "class_declaration",
            "interface_declaration",
            "function_declaration",
            "method_definition",
            "arrow_function",
            // Named const/let bindings (`const Form = FormProvider`) — export / import surface.
            "variable_declarator",
        ],
        lang: "typescript",
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
    // class_declaration, interface, variable_declarator, etc.).
    if let Some(name_node) = node.child_by_field_name("name") {
        let k = name_node.kind();
        if k == "identifier" || k == "property_identifier" || k == "type_identifier" {
            return Some(source[name_node.start_byte()..name_node.end_byte()].to_string());
        }
    }

    // `const Form = FormProvider` — declarator name is the export surface for shadcn-style UI.
    if node.kind() == "variable_declarator" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let k = name_node.kind();
            if k == "identifier" || k == "property_identifier" {
                return Some(source[name_node.start_byte()..name_node.end_byte()].to_string());
            }
        }
    }

    // Direct children fallback (covers some declaration forms)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let k = child.kind();
        if k == "identifier" || k == "property_identifier" || k == "type_identifier" {
            return Some(source[child.start_byte()..child.end_byte()].to_string());
        }
    }

    // Robust special handling for arrow_function assigned to variable/let/const:
    // The syntactic parent may be variable_declarator; climb if needed (e.g. in
    // case of export or parenthesized forms). Use field "name" on declarator when
    // available, with children scan fallback. This ensures "const DataTable = () => {}"
    // produces name "DataTable" (not "unknown").
    if node.kind() == "arrow_function" {
        let mut current = node.parent();
        while let Some(parent) = current {
            if parent.kind() == "variable_declarator" || parent.kind() == "assignment_expression" {
                if let Some(name_node) = parent.child_by_field_name("name") {
                    let k = name_node.kind();
                    if k == "identifier" || k == "property_identifier" {
                        return Some(
                            source[name_node.start_byte()..name_node.end_byte()].to_string(),
                        );
                    }
                }
                let mut c = parent.walk();
                for ch in parent.children(&mut c) {
                    let k = ch.kind();
                    if k == "identifier" || k == "property_identifier" {
                        return Some(source[ch.start_byte()..ch.end_byte()].to_string());
                    }
                }
                break;
            }
            current = parent.parent();
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn const_alias_form_provider_named() {
        let source = r#"
import { FormProvider } from "react-hook-form"
const Form = FormProvider
const FormField = () => null
export { Form, FormField }
"#;
        let path = PathBuf::from("components/ui/form.tsx");
        let parsed = parse(path, source).expect("parse");
        let names: Vec<&str> = parsed.blocks.iter().map(|b| b.name.as_str()).collect();
        assert!(
            names.iter().any(|n| *n == "Form"),
            "expected Form const alias block, got {names:?}"
        );
        assert!(
            names.iter().any(|n| *n == "FormField"),
            "expected FormField, got {names:?}"
        );
    }
}
