//! Pure **C** language module (`tree-sitter-c`).
//! Handles `.c` and C-shaped `.h` (see [`super::c_family::dialect_for_file`]).

pub mod edges;
pub mod parser;

pub(crate) use edges::{build_usage_edges, collect_call_edges, collect_usage_edges};
pub use parser::parse;

pub use super::super::parser::ParseError;

use crate::{BlockInfo, CodeGraph};
use tree_sitter::Query;

/// Call sites in pure C: bare `foo()` and `obj->method()` / `obj.field` when present.
/// Must compile against `tree_sitter_c` (no `qualified_identifier`).
pub(crate) const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @name
)
(call_expression
  function: (field_expression
    field: (field_identifier) @name
  )
)
"#;

/// Stdlib / noise — do not global-resolve these as project callees.
pub(crate) const GENERIC_NAMES: &[&str] = &[
    "printf", "fprintf", "sprintf", "snprintf", "scanf", "fscanf", "sscanf", "malloc", "calloc",
    "realloc", "free", "memcpy", "memmove", "memset", "memcmp", "strlen", "strcpy", "strncpy",
    "strcat", "strcmp", "strncmp", "assert", "abort", "exit", "sizeof", "offsetof", "alignof",
    "open", "close", "read", "write", "printf", "perror", "fprintf", "snprintf", "vsnprintf",
    "qsort", "bsearch", "abs", "labs", "fabs", "sin", "cos", "sqrt", "pow", "log", "exp",
    "time", "clock", "rand", "srand", "atoi", "atol", "atof", "strtol", "strtoul", "strtod",
    "getpid", "fork", "wait", "signal", "raise", "kill", "pthread_create", "pthread_join",
    "pthread_mutex_lock", "pthread_mutex_unlock",
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
        || Query::new(&tree_sitter_c::LANGUAGE.into(), CALL_QUERY),
        |b, _| (!b.name.is_empty()).then(|| b.name.clone()),
        GENERIC_NAMES,
        // C: snake_case is idiomatic — don't drop pure-lower like Aggressive does for temps.
        super::generic_edges::FallbackStyle::GoIdiomatic,
    );
}
