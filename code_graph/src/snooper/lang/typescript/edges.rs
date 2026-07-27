// code_graph/src/snooper/lang/typescript/edges.rs
//
// Call/usage edge collection for TypeScript/TSX/JS/JSX.
// Mirrors lang/python/edges.rs and lang/rust/edges.rs exactly.

use super::{CALL_QUERY, GENERIC_NAMES};
use std::collections::HashMap;
use tree_sitter::Query;

use crate::{BlockInfo, Id};

// Thin shims supply only language config (kinds + query ctor + name getter).
// All algorithm compressed into generic_edges.rs (iterator chains, unified resolve+blacklist).
// Dotted member resolution (old resolve_call_target) is now unified in the generic via rsplit.
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
        super::super::generic_edges::FallbackStyle::QueryOnly,
    )
}

pub(crate) use super::super::generic_edges::{build_usage_edges, collect_usage_edges};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::parser::ParsedFile;
    use std::path::PathBuf;

    #[test]
    fn test_tsx_call_edges_for_arrow_and_jsx() {
        // Mock TSX exercising:
        // - arrow_function assigned to const (needs extract_name fix)
        // - function_declaration
        // - local same-file call: scaffoldProject()
        // - JSX self-closing as call: <DataTable />
        let source = r#"const DataTable = () => { return <div />; }
function createProject() { scaffoldProject(); obj.run(); return <DataTable />; }
function scaffoldProject() { }
const obj = { run: () => {} };"#
            .to_string();

        let path = PathBuf::from("test.tsx");
        let parsed: ParsedFile = crate::snooper::lang::typescript::parse(path, &source)
            .expect("parse should succeed for TSX");

        let blocks = &parsed.blocks;
        let tree = parsed.tree.as_ref().expect("tree present for TS parse");

        let block_names: Vec<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
        eprintln!("Block names extracted: {:?}", block_names);

        let create_block = blocks
            .iter()
            .find(|b| b.name == "createProject" && b.kind.contains("function"))
            .expect("should have createProject function block");
        let _ = blocks
            .iter()
            .find(|b| b.name == "scaffoldProject" && b.kind.contains("function"))
            .expect("should have scaffoldProject function block");
        let _ = blocks
            .iter()
            .find(|b| b.name == "DataTable")
            .expect("should have DataTable block (arrow named, not 'unknown')");

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
        eprintln!("Collected call edges (from->to): {:?}", edge_names);

        // Match by name — multiple blocks can share a name (call shells vs defs).
        assert!(
            edge_names
                .iter()
                .any(|(f, t)| f == "createProject" && t == "scaffoldProject"),
            "createProject should call scaffoldProject; edges were {:?}",
            edge_names
        );
        assert!(
            edge_names
                .iter()
                .any(|(f, t)| f == "createProject" && t == "DataTable"),
            "createProject should edge to DataTable (JSX); edges were {:?}",
            edge_names
        );
        assert!(
            edges.iter().any(|(f, _)| f == &create_block.id)
                || edge_names.iter().any(|(f, _)| f == "createProject"),
            "createProject should have at least one callee; edges were {:?}",
            edge_names
        );
    }

    #[test]
    fn jsx_html_main_does_not_link_to_function_main() {
        // t3 Home: <main className=...> must not CALL-edge to export function main()
        let source = r#"
export function main() { return 0; }
export default function Home() {
  return <main className="x"><div /></main>;
}
"#
        .to_string();
        let path = PathBuf::from("app/page.tsx");
        let parsed = crate::snooper::lang::typescript::parse(path, &source).expect("parse");
        let blocks = &parsed.blocks;
        let tree = parsed.tree.as_ref().unwrap();
        let mut gmap = HashMap::new();
        for b in blocks {
            if b.name == "main" && b.kind.contains("function") {
                gmap.insert("main".into(), b.id.clone());
            }
        }
        let home = blocks.iter().find(|b| b.name == "Home").expect("Home");
        let edges = collect_call_edges(blocks, &source, tree, Some(&gmap));
        let bad = edges
            .iter()
            .any(|(f, t)| f == &home.id && gmap.get("main").is_some_and(|m| m == t));
        assert!(
            !bad,
            "Home must not call function main via <main>; edges={edges:?}"
        );
    }
}

