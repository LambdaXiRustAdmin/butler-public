//! C++ AST parsing via **tree-sitter-cpp** only.

use crate::snooper::lang::c_family::ast_shared;
use crate::snooper::parser::ParseError;
use std::path::PathBuf;
use tree_sitter::Parser;

pub fn parse(
    path: PathBuf,
    source: &str,
) -> Result<crate::snooper::parser::ParsedFile, ParseError> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
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
            "class_specifier",
            "if_statement",
            "for_statement",
            "while_statement",
            "call_expression",
            "return_statement",
            "expression_statement",
        ],
        lang: "cpp",
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

    // Export-macro mangling: tree-sitter may emit function_definition for whole classes.
    ast_shared::recover_macro_mangled_type_blocks(&mut blocks);

    ast_shared::collect_function_prototypes(root, &path, source, "cpp", &mut blocks);

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
    fn cpp_class_and_lang_tag() {
        let source = r#"
namespace N {
class Foo {
public:
  void bar();
};
}
void Foo::bar() {}
"#;
        let parsed = parse(PathBuf::from("foo.cpp"), source).unwrap();
        assert!(parsed.blocks.iter().all(|b| b.lang == "cpp"));
        assert!(parsed.blocks.iter().any(|b| b.name == "Foo"));
    }

    #[test]
    fn recovers_export_macro_struct_as_type_not_function() {
        // Universal shape: struct EXPORT_MACRO TypeName : public Base { … }
        // (tree-sitter-cpp mis-tags as function_definition without recovery).
        let source = r#"
struct DLL_EXPORT Widget : public Base {
  Widget() = delete;
  int64_t n_{0};
};
"#;
        let parsed = parse(PathBuf::from("include/widget.h"), source).unwrap();
        let hub = parsed
            .blocks
            .iter()
            .find(|b| b.name == "Widget" && b.kind.contains("struct"))
            .expect("struct Widget type hub");
        assert!(hub.source.contains('{'));
        assert!(hub.end_line > hub.start_line);
        // Ctor remains a function, not the type.
        assert!(parsed.blocks.iter().any(|b| {
            b.kind == "function_definition" && b.name == "Widget" && b.source.contains("delete")
        }));
    }

    #[test]
    fn derived_type_keeps_own_name_not_base_class() {
        // Universal: `struct Derived : public Base` must index as Derived.
        let source = r#"
struct MYLIB_API DerivedWidget : public Widget {
  explicit DerivedWidget(int x);
  int y;
};
"#;
        let parsed = parse(PathBuf::from("derived_widget.h"), source).unwrap();
        let hub = parsed
            .blocks
            .iter()
            .find(|b| b.kind.contains("struct") && b.source.contains('{'))
            .expect("type body");
        assert_eq!(
            hub.name, "DerivedWidget",
            "must not steal base class name Widget"
        );
    }
}
