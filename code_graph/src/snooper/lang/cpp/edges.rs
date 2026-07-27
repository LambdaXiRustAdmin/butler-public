//! Call/usage edges for C++ — query language must be `tree_sitter_cpp`.

use super::{CALL_QUERY, GENERIC_NAMES};
use std::collections::HashMap;
use tree_sitter::Query;

use crate::{BlockInfo, Id};

pub(crate) fn collect_call_edges(
    blocks: &[BlockInfo],
    source: &str,
    tree: &tree_sitter::Tree,
    global_names: Option<&HashMap<String, Id>>,
) -> Vec<(Id, Id)> {
    super::super::generic_edges::collect_call_edges(
        blocks,
        source,
        tree,
        global_names,
        // Methods are function_definition under class; class_specifier is not a call site span.
        &["function_definition"],
        &["function_definition", "function_declaration"],
        || Query::new(&tree_sitter_cpp::LANGUAGE.into(), CALL_QUERY),
        |b, _| (!b.name.is_empty()).then(|| b.name.clone()),
        GENERIC_NAMES,
        super::super::generic_edges::FallbackStyle::Aggressive,
    )
}

pub(crate) use super::super::generic_edges::{build_usage_edges, collect_usage_edges};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::parser::ParsedFile;
    use std::path::PathBuf;

    #[test]
    fn test_cpp_call_edges() {
        let source = r#"#include <memory>

class Foo {
public:
    void bar() { baz(); }
};

void baz() { }

void quux() {
    Foo f;
    f.bar();
    auto p = std::make_shared<Foo>();
    p->bar();
    ::baz();
}
"#
        .to_string();

        let path = PathBuf::from("test.cpp");
        let parsed: ParsedFile =
            crate::snooper::lang::cpp::parse(path, &source).expect("parse C++");

        let blocks = &parsed.blocks;
        let tree = parsed.tree.as_ref().expect("tree");

        let def = |name: &str| {
            blocks
                .iter()
                .find(|b| {
                    b.name == name
                        && (b.kind == "function_definition"
                            || b.kind == "class_specifier"
                            || b.kind == "struct_specifier")
                })
                .unwrap_or_else(|| panic!("should have def {name}"))
        };
        let bar_block = def("bar");
        let baz_block = def("baz");
        let quux_block = def("quux");

        let edges = collect_call_edges(blocks, &source, tree, None);
        let edge_names: Vec<(String, String)> = edges
            .iter()
            .map(|(f, t)| {
                let from_n = blocks
                    .iter()
                    .find(|b| &b.id == f)
                    .map(|b| b.name.clone())
                    .unwrap_or_default();
                let to_n = blocks
                    .iter()
                    .find(|b| &b.id == t)
                    .map(|b| b.name.clone())
                    .unwrap_or_default();
                (from_n, to_n)
            })
            .collect();

        assert!(
            edges
                .iter()
                .any(|(f, t)| f == &bar_block.id && t == &baz_block.id),
            "bar→baz; {edge_names:?}"
        );
        assert!(
            edges
                .iter()
                .any(|(f, t)| f == &quux_block.id && t == &bar_block.id),
            "quux→bar; {edge_names:?}"
        );
        assert!(
            edges
                .iter()
                .any(|(f, t)| f == &quux_block.id && t == &baz_block.id),
            "quux→baz; {edge_names:?}"
        );
    }
}
