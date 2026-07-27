// code_graph/src/snooper/lang/python/mod.rs
//
// Facade for the Python language module after directory promotion (Strangler Fig).
// Mirrors the structure of lang/rust/mod.rs exactly:
//
// - parser.rs: Phase 1 block extraction (parse, visit_node, name extractors)
// - edges.rs: call/usage edge collection (collect_* and build_* variants)
//
// Shared constants (GENERIC_NAMES, CALL_QUERY) live here. Re-exports preserve
// the public/crate API for snooper::parser, builder, scanner/cache, etc. so that
// lang::python::parse, lang::python::collect_call_edges, lang::python::build_call_edges
// (and ::parser:: / ::edges:: paths) continue to work without touching call sites.

pub mod edges;
pub mod ffi;
pub mod imports;
pub mod names;
pub mod parser;

pub(crate) use edges::{build_usage_edges, collect_call_edges, collect_usage_edges};
pub(crate) use names::prefer_ambiguous_python_names;
pub use parser::parse;
pub use ffi::link_to_ffi_exports;

// Re-export ParseError for consistency (was in original)
pub use super::super::parser::ParseError;

use crate::CodeGraph;

// 4-arg build_call_edges — routes through import-bound collect (not generic bare-name).
pub(crate) fn build_call_edges(
    blocks: &[crate::BlockInfo],
    source: &str,
    tree: &tree_sitter::Tree,
    graph: &mut CodeGraph,
) {
    for (from, to) in edges::collect_call_edges(blocks, source, tree, None) {
        graph.add_edge(from, to);
    }
}

// Blacklist of extremely common Python builtin / dunder / container names.
// Prevents polluting the graph with thousands of false-positive cross-file edges
// when falling back to global_names (e.g. every file calls "print", "len", "str").
// Distinctive project function names (e.g. "process_request", "render_skeleton")
// are NOT listed and will correctly resolve via the global map when no local definition
// exists in the caller's file.
const GENERIC_NAMES: &[&str] = &[
    "print",
    "len",
    "str",
    "int",
    "float",
    "bool",
    "list",
    "dict",
    "set",
    "tuple",
    "range",
    "open",
    "super",
    "isinstance",
    "hasattr",
    "getattr",
    "setattr",
    "callable",
    "append",
    "extend",
    "pop",
    "insert",
    "remove",
    "update",
    "clear",
    "copy",
    "keys",
    "values",
    "items",
    "get",
    "join",
    "split",
    "strip",
    "replace",
    "format",
    "read",
    "write",
    "close",
    "seek",
    "tell",
    "sorted",
    "sum",
    "min",
    "max",
    "abs",
    "round",
    "enumerate",
    "zip",
    "map",
    "filter",
    "any",
    "all",
    "input",
    "exit",
    "quit",
    "help",
    "__init__",
    "__call__",
    "__str__",
    "__repr__",
    "__enter__",
    "__exit__",
];

/// Tree-sitter query for Python call expressions.
/// Captures the callee **name** identifier only; [`edges`] classifies bare vs attribute
/// and applies the import-bound honest edge rule (see `imports.rs`).
///
/// - Direct function calls: `foo()`
/// - Method / attribute calls: `obj.method()`, `self.foo()`, `pkg.mod.func()`
///   (final attribute identifier captured; root object recovered by walking the AST)
/// - Nested under await / decorators: still a `call` node
pub(crate) const CALL_QUERY: &str = "
(call
  function: (identifier) @name
)
(call
  function: (attribute
    attribute: (identifier) @name
  )
)
";
