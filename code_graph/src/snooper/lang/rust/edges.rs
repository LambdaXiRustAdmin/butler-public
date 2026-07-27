// code_graph/src/snooper/lang/rust/edges.rs
//
// Call/usage edge collection for Rust (both "collect" style for new graphs and
// "build" style that mutates an existing &mut CodeGraph).
// Moved from monolithic rust.rs via Strangler Fig.

use super::{CALL_QUERY, GENERIC_NAMES};
use std::collections::HashMap;
use tree_sitter::Query;

use crate::{BlockInfo, Id};

// Thin shims supply only language config (kinds + query ctor + name getter).
// All algorithm compressed into generic_edges.rs (iterator chains, unified resolve+blacklist).
// build_call_edges shim killed (HIT 9) -- 4-arg API now provided by mod.rs forwarder.

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
        &["function_item", "impl_item", "trait_item"],
        &["function_item"],
        || Query::new(&tree_sitter_rust::LANGUAGE.into(), CALL_QUERY),
        |b, src| {
            if !b.name.is_empty() {
                Some(b.name.clone())
            } else {
                super::parser::extract_name_from_block(b, src)
            }
        },
        GENERIC_NAMES,
        // QueryOnly: body-scan matched comments/strings (e.g. "handle_orchestrate will miss")
        // as false CALL edges. Real calls come from Tree-sitter call_expression + global map.
        super::super::generic_edges::FallbackStyle::QueryOnly,
    )
}

pub(crate) use super::super::generic_edges::{build_usage_edges, collect_usage_edges};

#[cfg(test)]
mod call_edge_tests {
    use super::collect_call_edges;
    use crate::snooper::parser::parse_file;
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn rust_comment_does_not_create_false_call_edge() {
        // Repro: normalize_goal body contains the token only in a comment.
        let src = r#"
fn handle_orchestrate() {}

fn normalize_goal(raw: &str) -> String {
    // Non-orchestrate modes: leave raw (handle_orchestrate will miss/error as before).
    raw.to_string()
}
"#;
        let parsed = parse_file(Path::new("mod.rs"), src).expect("parse");
        let tree = parsed.tree.as_ref().expect("tree");
        let blocks = &parsed.blocks;
        let edges = collect_call_edges(blocks, src, tree, None);
        let caller = blocks
            .iter()
            .find(|b| b.name == "normalize_goal")
            .expect("normalize_goal");
        let target = blocks
            .iter()
            .find(|b| b.name == "handle_orchestrate")
            .expect("handle_orchestrate");
        assert!(
            !edges
                .iter()
                .any(|(f, t)| f == &caller.id && t == &target.id),
            "comment mention must not be a CALL edge; got {edges:?}"
        );
    }

    #[test]
    fn rust_cross_file_call_resolves_via_global_map() {
        // Repro: dispatch_tool in context_engine.rs → handle_orchestrate in mod.rs
        let callee_src = r#"
pub fn handle_orchestrate() {}
"#;
        let caller_src = r#"
fn dispatch_tool() {
    handle_orchestrate();
}
"#;
        let callee = parse_file(Path::new("orchestrate/mod.rs"), callee_src).expect("parse callee");
        let caller = parse_file(Path::new("context_engine.rs"), caller_src).expect("parse caller");
        let target = callee
            .blocks
            .iter()
            .find(|b| b.name == "handle_orchestrate")
            .expect("def");
        let mut global = HashMap::new();
        global.insert("handle_orchestrate".into(), target.id.clone());

        let tree = caller.tree.as_ref().expect("tree");
        let edges = collect_call_edges(&caller.blocks, caller_src, tree, Some(&global));
        let from = caller
            .blocks
            .iter()
            .find(|b| b.name == "dispatch_tool")
            .expect("caller");
        assert!(
            edges
                .iter()
                .any(|(f, t)| f == &from.id && t == &target.id),
            "dispatch_tool must CALL handle_orchestrate via global map; edges={edges:?}"
        );
    }

    #[test]
    fn rust_multiline_call_with_args_resolves_via_global_map() {
        // Live hole: dispatch_tool → handle_orchestrate( multi-line args )
        let callee_src = r#"
pub fn handle_orchestrate(a: u32, b: u32) -> u32 { a + b }
"#;
        let caller_src = r#"
fn dispatch_tool() {
    let orchestrate_out = handle_orchestrate(
        1,
        2,
    );
    let _ = orchestrate_out;
}
"#;
        let callee = parse_file(Path::new("orchestrate/mod.rs"), callee_src).expect("parse callee");
        let caller = parse_file(Path::new("context_engine.rs"), caller_src).expect("parse caller");
        let target = callee
            .blocks
            .iter()
            .find(|b| b.name == "handle_orchestrate")
            .expect("def");
        let mut global = HashMap::new();
        global.insert("handle_orchestrate".into(), target.id.clone());
        let tree = caller.tree.as_ref().expect("tree");
        let edges = collect_call_edges(&caller.blocks, caller_src, tree, Some(&global));
        let from = caller
            .blocks
            .iter()
            .find(|b| b.name == "dispatch_tool")
            .expect("caller");
        assert!(
            edges
                .iter()
                .any(|(f, t)| f == &from.id && t == &target.id),
            "multiline call must CALL handle_orchestrate; edges={edges:?}"
        );
    }

    #[test]
    fn rust_test_string_name_does_not_edge_to_def() {
        let src = r#"
fn handle_orchestrate() {}

#[test]
fn arch_compact_map_lists_skeleton_hubs_and_next() {
    let name = "handle_orchestrate";
    assert!(!name.is_empty());
}
"#;
        let parsed = parse_file(Path::new("mod.rs"), src).expect("parse");
        let tree = parsed.tree.as_ref().expect("tree");
        let edges = collect_call_edges(&parsed.blocks, src, tree, None);
        let test_fn = parsed
            .blocks
            .iter()
            .find(|b| b.name == "arch_compact_map_lists_skeleton_hubs_and_next")
            .expect("test");
        let target = parsed
            .blocks
            .iter()
            .find(|b| b.name == "handle_orchestrate")
            .expect("def");
        assert!(
            !edges
                .iter()
                .any(|(f, t)| f == &test_fn.id && t == &target.id),
            "string literal in test must not CALL the real def"
        );
    }

    #[test]
    fn parse_spine_file_indexes_function_item_not_only_call() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../cli/src/server/orchestrate/spine.rs");
        let src = std::fs::read_to_string(&path).expect("read spine.rs");
        let parsed = parse_file(&path, &src).expect("parse spine");
        let fns: Vec<(String, usize)> = parsed
            .blocks
            .iter()
            .filter(|b| b.kind == "function_item")
            .map(|b| (b.name.clone(), b.start_line))
            .collect();
        let by_name: Vec<_> = parsed
            .blocks
            .iter()
            .filter(|b| b.name == "reverse_call_spine")
            .map(|b| (b.kind.as_str(), b.start_line))
            .collect();
        assert!(
            by_name.iter().any(|(k, _)| k.contains("function")),
            "function_item must exist for reverse_call_spine; by_name={by_name:?} all_fns={fns:?}"
        );
    }

    #[test]
    fn pub_crate_fn_name_is_not_crate() {
        let src = r#"
pub(crate) fn reverse_call_spine() {}
pub fn other() {}
"#;
        let parsed = parse_file(Path::new("spine.rs"), src).expect("parse");
        let names: Vec<_> = parsed
            .blocks
            .iter()
            .filter(|b| b.kind.contains("function"))
            .map(|b| b.name.as_str())
            .collect();
        assert!(
            names.contains(&"reverse_call_spine"),
            "pub(crate) fn must name reverse_call_spine not crate; names={names:?}"
        );
        assert!(!names.contains(&"crate"), "must not name fn crate; names={names:?}");
    }
}

