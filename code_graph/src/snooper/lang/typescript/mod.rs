// code_graph/src/snooper/lang/typescript/mod.rs
//
// Facade for the TypeScript/TSX/JS/JSX language module.
// Mirrors the structure of lang/python/mod.rs and lang/rust/mod.rs exactly:
//
// - parser.rs: Phase 1 block extraction (parse, visit_node, name extractors)
// - edges.rs: call/usage edge collection (collect_* and build_* variants)
//
// Shared constants (GENERIC_NAMES, CALL_QUERY) live here. Re-exports preserve
// the public/crate API.

pub mod edges;
pub mod exports;
pub mod imports;
pub mod names;
pub mod parser;

pub(crate) use edges::{build_usage_edges, collect_call_edges, collect_usage_edges};
pub use imports::link_relative_imports;
pub(crate) use names::prefer_ambiguous_typescript_names;
pub use parser::parse;

// Re-export ParseError for consistency
pub use super::super::parser::ParseError;

use crate::{BlockInfo, CodeGraph};
use tree_sitter::Query;

// 4-arg build_call_edges (HIT 9) -- shim killed from edges.rs.
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
        &[
            "class_declaration",
            "interface_declaration",
            "function_declaration",
            "method_definition",
            "arrow_function",
        ],
        &[
            "function_declaration",
            "method_definition",
            "arrow_function",
        ],
        || Query::new(&tree_sitter_typescript::LANGUAGE_TSX.into(), CALL_QUERY),
        |b, _| (!b.name.is_empty()).then(|| b.name.clone()),
        GENERIC_NAMES,
        super::generic_edges::FallbackStyle::QueryOnly,
    );
}

// Blacklist of common JS/TS globals, React hooks, and **HTML/SVG intrinsics**.
// JSX `<main>` / `<div>` must not resolve to a global function named `main` (t3 Home bug).
const GENERIC_NAMES: &[&str] = &[
    "console",
    "require",
    "module",
    "exports",
    "process",
    "global",
    "window",
    "document",
    "setTimeout",
    "clearTimeout",
    "setInterval",
    "clearInterval",
    "Promise",
    "Array",
    "Object",
    "String",
    "Number",
    "Boolean",
    "Function",
    "React",
    "useState",
    "useEffect",
    "useContext",
    "useReducer",
    "useMemo",
    "useCallback",
    "useRef",
    "useLayoutEffect",
    "map",
    "filter",
    "reduce",
    "forEach",
    "find",
    "some",
    "every",
    "includes",
    "push",
    "pop",
    "shift",
    "unshift",
    "slice",
    "splice",
    "join",
    "split",
    "toString",
    "valueOf",
    "hasOwnProperty",
    "toFixed",
    "toPrecision",
    // HTML / SVG intrinsics (lowercase JSX tags)
    "a",
    "abbr",
    "address",
    "area",
    "article",
    "aside",
    "audio",
    "b",
    "base",
    "bdi",
    "bdo",
    "blockquote",
    "body",
    "br",
    "button",
    "canvas",
    "caption",
    "cite",
    "code",
    "col",
    "colgroup",
    "data",
    "datalist",
    "dd",
    "del",
    "details",
    "dfn",
    "dialog",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "i",
    "iframe",
    "img",
    "input",
    "ins",
    "kbd",
    "label",
    "legend",
    "li",
    "link",
    "main",
    "map",
    "mark",
    "menu",
    "meta",
    "meter",
    "nav",
    "noscript",
    "object",
    "ol",
    "optgroup",
    "option",
    "output",
    "p",
    "picture",
    "pre",
    "progress",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "script",
    "search",
    "section",
    "select",
    "slot",
    "small",
    "source",
    "span",
    "strong",
    "style",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "time",
    "title",
    "tr",
    "track",
    "u",
    "ul",
    "var",
    "video",
    "wbr",
    "svg",
    "path",
    "g",
    "circle",
    "rect",
    "line",
    "polyline",
    "polygon",
    "text",
    "defs",
    "clipPath",
    "use",
    "symbol",
    "fragment",
];

/// Tree-sitter query for TypeScript/JSX call and new expressions, plus JSX elements as calls.
/// Captures:
/// - Direct calls: foo(), bar(baz); also works for calls inside await expr / other wrappers
///   because the inner (call_expression) node is still present and matched structurally.
/// - Method calls: obj.method(), this.foo(), pkg.mod.func()
/// - New expressions: new Foo(), new pkg.Bar()
/// - JSX elements (including member like Form.Item for shadcn/radix etc) treated as
///   component "calls" for React/TSX architecture mapping. Captures either bare identifier
///   or full member_expression (text becomes e.g. "Form.Item" for lookup/fallback).
const CALL_QUERY: &str = "
(call_expression
  function: (identifier) @name
)
(call_expression
  function: (member_expression) @name
)
(new_expression
  constructor: (identifier) @name
)
(new_expression
  constructor: (member_expression) @name
)
(jsx_element
  open_tag: (jsx_opening_element
    name: [(identifier) (member_expression)] @name
  )
)
(jsx_self_closing_element
  name: [(identifier) (member_expression)] @name
)
";
