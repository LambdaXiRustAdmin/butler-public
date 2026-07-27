// code_graph/src/snooper/lang/generic_parser.rs
//
// Brutal compression of the 5x visit_node AST traversals.
// Zero-alloc child strategy using TreeCursor + goto_previous_sibling (no Vec<Node> allocation
// or rev/into_iter/drop per node). Eliminates the allocator spaghetti from HIT 11.
// Language specifics (kinds, name extraction, start adjustment for attrs, externals) passed via
// lightweight Copy config struct + fn pointers (no traits, no builders, no shells, zero cost).
// Mathemajical: explicit stack only for parent tracking + depth, cursor sibling walk for children.

use crate::{BlockInfo, Id};
use std::collections::HashSet;
use std::path::PathBuf;
use tree_sitter::Node;

#[derive(Copy, Clone)]
pub struct VisitConfig {
    pub interesting_kinds: &'static [&'static str],
    pub lang: &'static str,
    pub extract_name: fn(&Node, &str) -> Option<String>,
    pub get_start: fn(&Node, &str) -> (usize, usize),
    pub extract_externals: fn(Node, &str) -> HashSet<String>,
}

pub fn default_get_start(node: &Node, _source: &str) -> (usize, usize) {
    (node.start_position().row + 1, node.start_byte())
}

pub fn no_external_crates(_: Node, _: &str) -> HashSet<String> {
    HashSet::new()
}

pub fn visit_node(
    root: Node,
    file: PathBuf,
    source: &str,
    parent_id: Option<Id>,
    blocks: &mut Vec<BlockInfo>,
    config: VisitConfig,
    fallback_name: &str,
) {
    let mut stack: Vec<(Node, Option<Id>)> = vec![(root, parent_id)];

    while let Some((node, parent_id)) = stack.pop() {
        let kind = node.kind();

        if config.interesting_kinds.contains(&kind) {
            let (start_line, start_byte) = (config.get_start)(&node, source);
            let end_line = node.end_position().row + 1;
            let end_byte = node.end_byte();

            let name =
                (config.extract_name)(&node, source).unwrap_or_else(|| fallback_name.to_string());

            let external_crates = (config.extract_externals)(node, source);

            let mut block = BlockInfo::new(
                file.clone(),
                kind,
                config.lang,
                start_line,
                end_line,
                start_byte,
                end_byte,
                source[start_byte..end_byte].to_string(),
                &name,
                external_crates,
            );

            block.parent_id = parent_id.clone();
            let current_id = block.id.clone();
            blocks.push(block);

            push_children_zero_alloc(&mut stack, &node, Some(current_id));
        } else {
            push_children_zero_alloc(&mut stack, &node, parent_id.clone());
        }
    }
}

/// Zero-alloc reverse children push via TreeCursor sibling navigation.
/// Walks to last sibling then previous, pushing so leftmost child is popped first (correct DFS order).
/// No per-node Vec, no rev, no transient allocs/drops. Scales on deep ASTs.
fn push_children_zero_alloc<'a>(
    stack: &mut Vec<(Node<'a>, Option<Id>)>,
    node: &Node<'a>,
    child_parent: Option<Id>,
) {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        while cursor.goto_next_sibling() {}
        loop {
            let child = cursor.node();
            stack.push((child, child_parent.clone()));
            if !cursor.goto_previous_sibling() {
                break;
            }
        }
    }
}
