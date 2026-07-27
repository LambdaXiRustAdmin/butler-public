//! Pure **C++** language module (`tree-sitter-cpp`).
//! Handles `.cpp`/`.hpp`/… and C++-shaped `.h` (see [`super::c_family::dialect_for_file`]).

pub mod edges;
pub mod parser;

pub(crate) use edges::{build_usage_edges, collect_call_edges, collect_usage_edges};
pub use parser::parse;

pub use super::super::parser::ParseError;

use crate::{BlockInfo, CodeGraph};
use tree_sitter::Query;

/// C++ call query — must use `tree_sitter_cpp` language object.
pub(crate) const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @name
)
(call_expression
  function: (field_expression
    field: (field_identifier) @name
  )
)
(call_expression
  function: (qualified_identifier
    name: (identifier) @name
  )
)
"#;

/// Visible to regression tests in `lang/c` (wrong-language capture = 0).
#[cfg(test)]
pub(crate) const CALL_QUERY_FOR_TEST: &str = CALL_QUERY;

pub(crate) const GENERIC_NAMES: &[&str] = &[
    "printf",
    "fprintf",
    "sprintf",
    "snprintf",
    "scanf",
    "malloc",
    "calloc",
    "realloc",
    "free",
    "memcpy",
    "memset",
    "strlen",
    "strcpy",
    "strcmp",
    "assert",
    "abort",
    "exit",
    "sizeof",
    "new",
    "delete",
    "this",
    "std::cout",
    "std::cin",
    "std::cerr",
    "std::endl",
    "std::flush",
    "std::string",
    "std::vector",
    "std::map",
    "std::set",
    "std::list",
    "std::shared_ptr",
    "std::unique_ptr",
    "std::make_shared",
    "std::make_unique",
    "push_back",
    "emplace_back",
    "insert",
    "erase",
    "clear",
    "size",
    "empty",
    "begin",
    "end",
    "std::move",
    "std::forward",
    "std::swap",
    "std::thread",
    "std::mutex",
    "std::lock_guard",
    "detail",
    "impl",
    "internal",
    "using",
];

pub(crate) fn build_call_edges(
    blocks: &[BlockInfo],
    source: &str,
    tree: &tree_sitter::Tree,
    graph: &mut CodeGraph,
) {
    super::generic_edges::build_call_edges(
        blocks,
        source,
        tree,
        graph,
        &["function_definition"],
        &["function_definition", "function_declaration"],
        || Query::new(&tree_sitter_cpp::LANGUAGE.into(), CALL_QUERY),
        |b, _| (!b.name.is_empty()).then(|| b.name.clone()),
        GENERIC_NAMES,
        super::generic_edges::FallbackStyle::Aggressive,
    );
}
