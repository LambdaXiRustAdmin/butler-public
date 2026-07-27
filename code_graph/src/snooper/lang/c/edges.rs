//! Call/usage edges for pure C — query language must be `tree_sitter_c`.

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
        // Call sites live in function bodies only (not types).
        &["function_definition"],
        // CALL targets: functions only — never struct/type names from signatures.
        &["function_definition", "function_declaration"],
        || Query::new(&tree_sitter_c::LANGUAGE.into(), CALL_QUERY),
        |b, _| (!b.name.is_empty()).then(|| b.name.clone()),
        GENERIC_NAMES,
        super::super::generic_edges::FallbackStyle::GoIdiomatic,
    )
}

pub(crate) use super::super::generic_edges::{build_usage_edges, collect_usage_edges};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn c_same_file_calls_via_c_query() {
        let source = r#"
static int openDatabase(int x) { return x; }
int sqlite3_open(int x) {
  return openDatabase(x);
}
int helper(void) { return sqlite3_open(1); }
"#
        .to_string();
        let parsed =
            crate::snooper::lang::c::parse(PathBuf::from("main.c"), &source).expect("parse");
        let tree = parsed.tree.as_ref().unwrap();
        let edges = collect_call_edges(&parsed.blocks, &source, tree, None);

        // Count AST captures: C query on C tree must be non-zero
        let q = Query::new(&tree_sitter_c::LANGUAGE.into(), CALL_QUERY).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut caps = 0usize;
        let mut matches = cursor.matches(&q, tree.root_node(), source.as_bytes());
        use tree_sitter::StreamingIterator;
        while let Some(m) = matches.next() {
            caps += m.captures.len();
        }
        assert!(caps >= 2, "C query must capture calls on C tree; got {caps}");

        let def = |name: &str| {
            parsed
                .blocks
                .iter()
                .find(|b| b.name == name && b.kind == "function_definition")
                .unwrap_or_else(|| panic!("missing {name}"))
        };
        let open = def("sqlite3_open");
        let db = def("openDatabase");
        let helper = def("helper");
        assert!(
            edges.iter().any(|(f, t)| f == &open.id && t == &db.id),
            "sqlite3_open → openDatabase; edges={edges:?}"
        );
        assert!(
            edges.iter().any(|(f, t)| f == &helper.id && t == &open.id),
            "helper → sqlite3_open"
        );
    }

    #[test]
    fn type_names_in_signature_are_not_callees() {
        // Parameter/return types must not become CALLS edges.
        let source = r#"
typedef struct GLFWwindow GLFWwindow;
typedef struct GLFWmonitor GLFWmonitor;
void _glfwInputError(int code, const char *msg) { (void)code; (void)msg; }
GLFWwindow *glfwCreateWindow(int w, int h, GLFWmonitor *mon) {
  if (w <= 0) {
    _glfwInputError(1, "bad");
    return 0;
  }
  (void)mon;
  return 0;
}
"#
        .to_string();
        let parsed =
            crate::snooper::lang::c::parse(PathBuf::from("window.c"), &source).expect("parse");
        let tree = parsed.tree.as_ref().unwrap();
        let edges = collect_call_edges(&parsed.blocks, &source, tree, None);
        let names: Vec<(String, String)> = edges
            .iter()
            .map(|(f, t)| {
                let from = parsed
                    .blocks
                    .iter()
                    .find(|b| &b.id == f)
                    .map(|b| b.name.clone())
                    .unwrap_or_default();
                let to = parsed
                    .blocks
                    .iter()
                    .find(|b| &b.id == t)
                    .map(|b| b.name.clone())
                    .unwrap_or_default();
                (from, to)
            })
            .collect();
        assert!(
            !names.iter().any(|(_, t)| t == "GLFWwindow" || t == "GLFWmonitor"),
            "types must not be callees: {names:?}"
        );
        assert!(
            names
                .iter()
                .any(|(f, t)| f == "glfwCreateWindow" && t == "_glfwInputError"),
            "expected call to _glfwInputError: {names:?}"
        );
    }

    #[test]
    fn cpp_query_on_c_tree_is_zero_captures_regression() {
        // Documents the bug we fixed: wrong language object → silent 0 matches.
        let source = "int f(void) { g(); return 0; }\nint g(void) { return 1; }\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let q_cpp = Query::new(
            &tree_sitter_cpp::LANGUAGE.into(),
            crate::snooper::lang::cpp::CALL_QUERY_FOR_TEST,
        );
        assert!(q_cpp.is_ok());
        let q = q_cpp.unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut caps = 0usize;
        let mut matches = cursor.matches(&q, tree.root_node(), source.as_bytes());
        use tree_sitter::StreamingIterator;
        while let Some(m) = matches.next() {
            caps += m.captures.len();
        }
        assert_eq!(
            caps, 0,
            "C++ query language on C tree must yield 0 (the bug)"
        );
    }
}
