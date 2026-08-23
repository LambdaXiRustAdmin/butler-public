// code_graph/src/snooper/lang/python/edges.rs
//
// Call/usage edge collection for Python.
//
// Same-lang CALL resolution with the **import-bound attribute** honest edge rule:
// - bare `foo()` → local / global (existing behaviour)
// - `from models import create as mk; mk()` → resolve export `create` under `models`
// - `import utils; utils.clean()` → resolve `clean` (import-bound)
// - `user.save()` when `user` is not an import → local-only (no global bare-name fan-out)

use super::imports::{parse_import_map, path_affinity, ImportMap};
use super::{CALL_QUERY, GENERIC_NAMES};
use std::collections::HashMap;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

use crate::{BlockInfo, Id};

const CALLER_KINDS: &[&str] = &[
    "function_definition",
    "async_function_definition",
    "class_definition",
];
const CALLEE_KINDS: &[&str] = &["function_definition", "async_function_definition"];

pub(crate) fn collect_call_edges(
    blocks: &[BlockInfo],
    source: &str,
    tree: &tree_sitter::Tree,
    global_names: Option<&HashMap<String, Id>>,
) -> Vec<(Id, Id)> {
    let mut edges = Vec::new();
    let import_map = parse_import_map(source);

    let mut local_name_to_id: HashMap<String, Id> = HashMap::new();
    let mut local_score: HashMap<String, i32> = HashMap::new();
    for b in blocks
        .iter()
        .filter(|b| CALLEE_KINDS.contains(&b.kind.as_str()))
    {
        if b.name.is_empty() {
            continue;
        }
        let sc = local_callee_preference(b);
        match local_score.get(&b.name) {
            Some(&prev) if prev >= sc => {}
            _ => {
                local_score.insert(b.name.clone(), sc);
                local_name_to_id.insert(b.name.clone(), b.id.clone());
            }
        }
    }

    let id_is_local_callee: HashMap<&Id, bool> = blocks
        .iter()
        .map(|b| (&b.id, CALLEE_KINDS.contains(&b.kind.as_str())))
        .collect();

    let query = match Query::new(&tree_sitter_python::LANGUAGE.into(), CALL_QUERY) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("⚠️ Tree-sitter Python call query error: {e}");
            return edges;
        }
    };

    let mut cursor = QueryCursor::new();
    let root = tree.root_node();
    let call_captures: Vec<Node> = {
        let mut caps = Vec::new();
        let mut matches = cursor.matches(&query, root, source.as_bytes());
        while let Some(mat) = matches.next() {
            for c in mat.captures {
                if c.index == 0 {
                    caps.push(c.node);
                }
            }
        }
        caps
    };

    for block in blocks
        .iter()
        .filter(|b| CALLER_KINDS.contains(&b.kind.as_str()))
    {
        let (bs, be) = (block.start_byte, block.end_byte);
        for name_node in call_captures
            .iter()
            .filter(|n| n.start_byte() >= bs && n.end_byte() <= be)
        {
            let shape = classify_call_capture(*name_node, source);
            if let Some(target_id) =
                resolve_python_call(&shape, &import_map, &local_name_to_id, global_names)
            {
                if target_id != &block.id
                    && global_or_local_ok(target_id, &local_name_to_id, &id_is_local_callee)
                {
                    edges.push((block.id.clone(), target_id.clone()));
                }
            }
        }
    }

    // Body-scan fallback (Aggressive): local + non-generic global bare names.
    // Does not invent attribute edges for dynamic receivers.
    for block in blocks
        .iter()
        .filter(|b| CALLER_KINDS.contains(&b.kind.as_str()))
    {
        if block.start_byte >= block.end_byte || block.end_byte > source.len() {
            continue;
        }
        let block_source = &source[block.start_byte..block.end_byte];
        let candidates = local_name_to_id.iter().map(|(n, t)| (n.as_str(), t)).chain(
            global_names
                .iter()
                .flat_map(|g| g.iter())
                .filter(|(n, _)| !local_name_to_id.contains_key(*n))
                .filter(|(n, _)| !GENERIC_NAMES.contains(&n.as_str()))
                .map(|(n, t)| (n.as_str(), t)),
        );
        for (name, target_id) in candidates {
            if should_skip_fallback(name) {
                continue;
            }
            if super::super::generic_edges::contains_word_boundary(block_source, name)
                && target_id != &block.id
            {
                edges.push((block.id.clone(), target_id.clone()));
            }
        }
    }

    edges
}

#[derive(Debug)]
enum CallShape {
    /// `foo()`
    Bare(String),
    /// `obj.attr()` / `pkg.mod.attr()` — root is leftmost identifier when static.
    Attr { root: Option<String>, attr: String },
}

fn classify_call_capture(name_node: Node, source: &str) -> CallShape {
    let name = source[name_node.start_byte()..name_node.end_byte()].to_string();
    let Some(parent) = name_node.parent() else {
        return CallShape::Bare(name);
    };
    if parent.kind() == "attribute" {
        let root = attribute_root_identifier(parent, source);
        CallShape::Attr { root, attr: name }
    } else {
        // Parent is typically `call` with function: identifier.
        CallShape::Bare(name)
    }
}

/// Walk `object` field until a bare identifier (or give up on subscript/call).
fn attribute_root_identifier(attr_node: Node, source: &str) -> Option<String> {
    let mut obj = attr_node.child_by_field_name("object")?;
    while obj.kind() == "attribute" {
        obj = obj.child_by_field_name("object")?;
    }
    if obj.kind() == "identifier" {
        Some(source[obj.start_byte()..obj.end_byte()].to_string())
    } else {
        None
    }
}

fn resolve_python_call<'a>(
    shape: &CallShape,
    imports: &ImportMap,
    local: &'a HashMap<String, Id>,
    global: Option<&'a HashMap<String, Id>>,
) -> Option<&'a Id> {
    match shape {
        CallShape::Bare(name) => {
            // `from models import create as mk` → bare `mk()` resolves export `create`.
            if let Some(b) = imports.get(name) {
                if let Some(export) = &b.export_name {
                    return resolve_named(
                        export,
                        Some(b.module.as_str()),
                        local,
                        global,
                        /*import_bound*/ true,
                    );
                }
            }
            resolve_named(name, None, local, global, false)
        }
        CallShape::Attr { root: None, attr } => {
            // `handlers[x]()` / dynamic — local only.
            local.get(attr)
        }
        CallShape::Attr {
            root: Some(root),
            attr,
        } => {
            if let Some(b) = imports.get(root) {
                if b.export_name.is_none() {
                    // Module alias: `import utils` + `utils.clean()` / `from . import utils`.
                    return resolve_named(attr, Some(b.module.as_str()), local, global, true);
                }
                // `from pkg import Class` + `Class.method()` — method under that module.
                return resolve_named(attr, Some(b.module.as_str()), local, global, true);
            }
            // Not import-bound (`self.foo`, `user.save`) — local only. No global bare fan-out.
            local.get(attr)
        }
    }
}

fn resolve_named<'a>(
    name: &str,
    module_hint: Option<&str>,
    local: &'a HashMap<String, Id>,
    global: Option<&'a HashMap<String, Id>>,
    import_bound: bool,
) -> Option<&'a Id> {
    if let Some(id) = local.get(name) {
        return Some(id);
    }
    let g = global?;
    // Generic blacklist: still enforce for non-import-bound.
    // Import-bound may resolve distinctive names that happen to sit on the list only when
    // longer than 3 chars (skip get/pop/map noise; allow project `clean`/`helper`).
    if GENERIC_NAMES.contains(&name) {
        if !import_bound || name.len() <= 3 {
            return None;
        }
    }
    let id = g.get(name)?;
    // Import-bound honesty gate (v26): warehouse path must structurally agree with the
    // imported module. Stdlib / pip packages absent from inventory score affinity 0 →
    // drop (`subprocess.run` must not link to `typer.main.run`). Silence > invent.
    if import_bound {
        if let Some(m) = module_hint {
            if path_affinity(id.as_str(), m) == 0 {
                return None;
            }
        }
    }
    Some(id)
}

fn global_or_local_ok(
    target_id: &Id,
    local: &HashMap<String, Id>,
    id_is_local_callee: &HashMap<&Id, bool>,
) -> bool {
    if local.values().any(|id| id == target_id) {
        return true;
    }
    match id_is_local_callee.get(target_id) {
        Some(false) => false,
        _ => true,
    }
}

fn local_callee_preference(b: &BlockInfo) -> i32 {
    let k = b.kind.to_ascii_lowercase();
    let mut s = 0i32;
    if k.contains("function_definition") {
        s += 30;
    } else {
        s += 5;
    }
    let f = b.file.to_string_lossy().to_ascii_lowercase();
    if f.contains("_test.") || f.contains("/test/") || f.contains("/tests/") {
        s -= 40;
    }
    s
}

fn should_skip_fallback(name: &str) -> bool {
    const UNIVERSAL_STOP: &[&str] = &["With", "Query", "Result", "Option", "String", "Self"];
    if UNIVERSAL_STOP.contains(&name) {
        return true;
    }
    name.len() <= 3 || (name.chars().all(|c| c.is_ascii_lowercase()) && !name.contains('_'))
}

pub(crate) use super::super::generic_edges::collect_usage_edges;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::parser::ParsedFile;
    use std::path::PathBuf;

    fn parse_py(path: &str, source: &str) -> ParsedFile {
        crate::snooper::lang::python::parse(PathBuf::from(path), source)
            .expect("python parse should succeed")
    }

    fn edge_names(edges: &[(Id, Id)], blocks: &[BlockInfo]) -> Vec<(String, String)> {
        edges
            .iter()
            .map(|(f, t)| {
                let from_n = blocks
                    .iter()
                    .find(|b| &b.id == f)
                    .map(|b| b.name.clone())
                    .unwrap_or_else(|| f.as_str().to_string());
                let to_n = blocks
                    .iter()
                    .find(|b| &b.id == t)
                    .map(|b| b.name.clone())
                    .unwrap_or_else(|| t.as_str().to_string());
                (from_n, to_n)
            })
            .collect()
    }

    #[test]
    fn import_bound_attribute_links_across_files() {
        // Three-file mental model (utils + models + main), edges collected on main
        // with a synthetic global map pointing at utils.clean / models.create.
        let main_src = r#"
import utils
from models import create as mk

def run():
    utils.clean("x")
    mk()
"#;
        let parsed = parse_py("main.py", main_src);
        let blocks = &parsed.blocks;
        let tree = parsed.tree.as_ref().expect("tree");

        let clean_id = Id::new("utils.py", "function_definition", "clean000");
        let create_id = Id::new("models.py", "function_definition", "create00");
        let mut global = HashMap::new();
        global.insert("clean".into(), clean_id.clone());
        global.insert("create".into(), create_id.clone());

        let edges = collect_call_edges(blocks, main_src, tree, Some(&global));
        let run_id = blocks
            .iter()
            .find(|b| b.name == "run")
            .map(|b| b.id.clone())
            .expect("run block");

        assert!(
            edges
                .iter()
                .any(|(from, to)| from == &run_id && to == &clean_id),
            "import-bound utils.clean() must link to clean_id; edges={:?}",
            edge_names(&edges, blocks)
        );
        assert!(
            edges
                .iter()
                .any(|(from, to)| from == &run_id && to == &create_id),
            "from-import alias mk() must link to create_id; edges={:?}",
            edge_names(&edges, blocks)
        );
    }

    #[test]
    fn dynamic_attribute_does_not_global_fanout() {
        let src = r#"
def save():
    pass

def act(user):
    user.save()
"#;
        let parsed = parse_py("app.py", src);
        let blocks = &parsed.blocks;
        let tree = parsed.tree.as_ref().expect("tree");

        // Plant a *different* file's save in the global map — must not link via user.save().
        let other_save = Id::new("other.py", "function_definition", "save0000");
        let mut global = HashMap::new();
        global.insert("save".into(), other_save.clone());

        let edges = collect_call_edges(blocks, src, tree, Some(&global));
        // Local `save` may link if body-scan / local resolve from another path — but
        // `user.save()` must not produce an edge to *other_save*.
        assert!(
            !edges.iter().any(|(_, to)| to == &other_save),
            "dynamic user.save() must not fan out to global other.py save"
        );
    }

    #[test]
    fn local_same_file_method_still_links() {
        let src = r#"
def helper():
    return 1

def run():
    helper()
"#;
        let parsed = parse_py("local.py", src);
        let blocks = &parsed.blocks;
        let tree = parsed.tree.as_ref().expect("tree");
        let edges = collect_call_edges(blocks, src, tree, None);
        let names = edge_names(&edges, blocks);
        assert!(
            names.iter().any(|(f, t)| f == "run" && t == "helper"),
            "local bare call must still work; edges={names:?}"
        );
    }

    #[test]
    fn stdlib_module_attr_does_not_link_homonym() {
        // Crucible: subprocess.run must not fan out to warehouse typer.main.run.
        let src = r#"
import subprocess

def live():
    subprocess.run(["zensical", "build"], check=True)
"#;
        let parsed = parse_py("scripts/docs.py", src);
        let blocks = &parsed.blocks;
        let tree = parsed.tree.as_ref().expect("tree");

        let typer_run = Id::new("typer/main.py", "function_definition", "run00000");
        let mut global = HashMap::new();
        global.insert("run".into(), typer_run.clone());

        let edges = collect_call_edges(blocks, src, tree, Some(&global));
        assert!(
            !edges.iter().any(|(_, to)| to == &typer_run),
            "path affinity 0 for stdlib module must drop edge; edges={:?}",
            edge_names(&edges, blocks)
        );
    }

    #[test]
    fn import_bound_still_links_when_path_matches_module() {
        let src = r#"
import utils

def run():
    utils.clean("x")
"#;
        let parsed = parse_py("main.py", src);
        let blocks = &parsed.blocks;
        let tree = parsed.tree.as_ref().expect("tree");
        let clean_id = Id::new("utils.py", "function_definition", "clean000");
        let mut global = HashMap::new();
        global.insert("clean".into(), clean_id.clone());

        let edges = collect_call_edges(blocks, src, tree, Some(&global));
        let run_id = blocks
            .iter()
            .find(|b| b.name == "run")
            .map(|b| b.id.clone())
            .expect("run");
        assert!(
            edges.iter().any(|(f, t)| f == &run_id && t == &clean_id),
            "in-warehouse utils.clean must still link; edges={:?}",
            edge_names(&edges, blocks)
        );
    }
}
