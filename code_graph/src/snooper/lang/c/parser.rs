//! C AST parsing via **tree-sitter-c** only.
//! Prototypes → `function_declaration`; bodies → `function_definition`.

use crate::snooper::lang::c_family::{self, ast_shared};
use crate::snooper::parser::ParseError;
use std::path::PathBuf;
use tree_sitter::Parser;

pub fn parse(
    path: PathBuf,
    source: &str,
) -> Result<crate::snooper::parser::ParsedFile, ParseError> {
    let dialect = c_family::dialect_for_file(&path, source);
    // Caller should only route C here; if sniff says C++ (shouldn't for .c), still parse as C
    // when this module is invoked — path-based .c always C.
    let _ = dialect;

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
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
            "function_definition",
            "struct_specifier",
            "if_statement",
            "for_statement",
            "while_statement",
            "call_expression",
            "return_statement",
            "expression_statement",
        ],
        lang: "c",
        extract_name: ast_shared::extract_name,
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

    ast_shared::recover_macro_mangled_type_blocks(&mut blocks);

    ast_shared::collect_function_prototypes(root, &path, source, "c", &mut blocks);

    Ok(crate::snooper::parser::ParsedFile {
        path,
        source: source.to_string(),
        blocks,
        tree: Some(tree),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn indexes_c_header_prototypes() {
        let source = r#"
int glfwInit(void);
static int helper(int x);
extern void foo(const char *s);
typedef int (*cb)(void);
struct Bar { int x; };
"#;
        let parsed = parse(PathBuf::from("include/glfw3.h"), source).expect("parse");
        assert!(parsed.blocks.iter().all(|b| b.lang == "c"));
        let names: Vec<_> = parsed
            .blocks
            .iter()
            .filter(|b| b.kind == "function_declaration")
            .map(|b| b.name.as_str())
            .collect();
        assert!(names.contains(&"glfwInit"));
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"foo"));
        assert!(!names.contains(&"cb"));
    }

    #[test]
    fn lang_tag_is_c_not_cpp() {
        let source = "int api(void) { return 1; }\n";
        let parsed = parse(PathBuf::from("api.c"), source).unwrap();
        assert!(parsed.blocks.iter().all(|b| b.lang == "c"));
    }
}
