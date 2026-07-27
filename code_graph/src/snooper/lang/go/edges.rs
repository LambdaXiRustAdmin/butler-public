// code_graph/src/snooper/lang/go/edges.rs
//
// Call/usage edge collection for Go.
//
// Package-qualified calls (`binding.Default`) must not collapse to bare-name
// global resolve (`gin.Default`). Tree-sitter cannot tell package vs variable;
// we use the file import table: if the qualifier is an import alias, resolve
// only inside that import path; otherwise fall back to bare-name / method
// best-effort (struct instance).

use super::GENERIC_NAMES;
use std::collections::HashMap;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

use crate::{BlockInfo, Id};

/// Import alias (or last path segment) → import path without quotes.
pub(crate) type ImportTable = HashMap<String, String>;

const CALLER_KINDS: &[&str] = &["function_declaration", "method_declaration"];
const CALLEE_KINDS: &[&str] = &["function_declaration", "method_declaration"];

// Bare identifier call + package/variable selector. Captures full selector text
// via @call for range, and optional qualifier for import-table checks.
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @bare)
(call_expression
  function: (selector_expression
    operand: (identifier) @qual
    field: (field_identifier) @field))
"#;

/// How a call site should resolve (repo-agnostic).
#[derive(Debug, Clone)]
enum CallKind {
    /// `Foo()` — local then global bare-name.
    Bare,
    /// `pkg.Foo()` where pkg is an import alias — package-path filter only.
    Package(String),
    /// `recv.Foo()` non-import — method/receiver; never bare-global (Close trap).
    Method,
}

pub(crate) fn collect_call_edges(
    blocks: &[BlockInfo],
    source: &str,
    tree: &tree_sitter::Tree,
    global_names: Option<&HashMap<String, Id>>,
    go_all: Option<&HashMap<String, Vec<Id>>>,
) -> Vec<(Id, Id)> {
    let imports = parse_go_imports(tree.root_node(), source);
    let mut edges = Vec::new();

    let mut local_name_to_id: HashMap<String, Id> = HashMap::new();
    for b in blocks.iter().filter(|b| CALLEE_KINDS.contains(&b.kind.as_str())) {
        if !b.name.is_empty() {
            local_name_to_id
                .entry(b.name.clone())
                .or_insert_with(|| b.id.clone());
        }
    }

    let query = match Query::new(&tree_sitter_go::LANGUAGE.into(), CALL_QUERY) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("⚠️ Go call query error: {e}");
            return edges;
        }
    };

    // (start, end, name, kind)
    let mut call_sites: Vec<(usize, usize, String, CallKind)> = Vec::new();
    {
        let mut cursor = QueryCursor::new();
        let root = tree.root_node();
        let mut matches = cursor.matches(&query, root, source.as_bytes());
        while let Some(mat) = matches.next() {
            let mut bare: Option<Node> = None;
            let mut qual: Option<Node> = None;
            let mut field: Option<Node> = None;
            for c in mat.captures {
                let name = query.capture_names()[c.index as usize];
                match name {
                    "bare" => bare = Some(c.node),
                    "qual" => qual = Some(c.node),
                    "field" => field = Some(c.node),
                    _ => {}
                }
            }
            if let Some(n) = bare {
                let name = source[n.start_byte()..n.end_byte()].to_string();
                call_sites.push((n.start_byte(), n.end_byte(), name, CallKind::Bare));
            } else if let (Some(q), Some(f)) = (qual, field) {
                let qname = &source[q.start_byte()..q.end_byte()];
                let fname = source[f.start_byte()..f.end_byte()].to_string();
                let kind = match imports.get(qname) {
                    Some(imp) => CallKind::Package(imp.clone()),
                    None => CallKind::Method,
                };
                let start = q.start_byte();
                let end = f.end_byte();
                call_sites.push((start, end, fname, kind));
            }
        }
    }

    for block in blocks
        .iter()
        .filter(|b| CALLER_KINDS.contains(&b.kind.as_str()))
    {
        let (bs, be) = (block.start_byte, block.end_byte);
        for (cs, ce, name, kind) in &call_sites {
            if *cs < bs || *ce > be {
                continue;
            }
            if name.is_empty() || GENERIC_NAMES.contains(&name.as_str()) {
                continue;
            }
            match kind {
                // Package-qualified: never bare-global short-name (gin Default trap).
                CallKind::Package(import_path) => {
                    if let Some(tid) =
                        resolve_package_qualified(name, import_path, &local_name_to_id, go_all)
                    {
                        if tid != &block.id {
                            edges.push((block.id.clone(), tid.clone()));
                        }
                    }
                    // No edge if we cannot prove the package match.
                }
                // Receiver/method `x.Name()`: no name-based resolve without types.
                // Linking unique local method still false-edges ng.Close() → LazyLoader.Close.
                // Honesty over recall (prometheus/gin method traps).
                CallKind::Method => {}
                CallKind::Bare => {
                    if let Some(tid) = resolve_bare(name, &local_name_to_id, global_names) {
                        if tid != &block.id {
                            edges.push((block.id.clone(), tid.clone()));
                        }
                    }
                }
            }
        }
    }

    // No body-scan fallback for Go: it reintroduces bare-name false positives
    // on `pkg.Name` text (word-boundary match of Name inside binding.Default).
    edges
}

fn resolve_bare<'a>(
    name: &str,
    local: &'a HashMap<String, Id>,
    global: Option<&'a HashMap<String, Id>>,
) -> Option<&'a Id> {
    if let Some(id) = local.get(name) {
        return Some(id);
    }
    if let Some(g) = global {
        if !GENERIC_NAMES.contains(&name) {
            return g.get(name);
        }
    }
    None
}

/// Resolve `Name` only among defs whose file path matches the import path.
fn resolve_package_qualified<'a>(
    name: &str,
    import_path: &str,
    local: &'a HashMap<String, Id>,
    go_all: Option<&'a HashMap<String, Vec<Id>>>,
) -> Option<&'a Id> {
    // Same-package style is rare via selector; still allow local if path matches.
    if let Some(id) = local.get(name) {
        if id_matches_import_path(id, import_path) {
            return Some(id);
        }
    }
    let candidates = go_all.and_then(|m| m.get(name))?;
    let mut matches: Vec<&Id> = candidates
        .iter()
        .filter(|id| id_matches_import_path(id, import_path))
        .collect();
    if matches.is_empty() {
        return None;
    }
    // Prefer non-test files when several match the package path.
    matches.sort_by_key(|id| {
        let f = file_from_id(id).to_ascii_lowercase();
        let mut s = 0i32;
        if f.contains("_test.go") || f.contains("/test/") {
            s += 100;
        }
        s
    });
    Some(matches[0])
}

fn file_from_id(id: &Id) -> String {
    id.as_str()
        .split(':')
        .next()
        .unwrap_or("")
        .replace('\\', "/")
}

/// Whether a block id's file path belongs to the imported package path.
///
/// Warehouse Go ids use **project-relative** paths (`gin.go`, `binding/foo.go`).
/// Match by package directory (repo-agnostic; no framework special cases):
/// - file in `binding/x.go` ↔ import ends with `/binding` (or equals `binding`)
/// - root file `gin.go` ↔ module-root import only (`github.com/org/repo`, no
///   extra subpackage suffix) — so `…/gin/binding` does not claim root files
/// - never let a subpackage path match a parent import via substring (`…/gin/`
///   inside `…/gin/binding/…`)
pub(crate) fn id_matches_import_path(id: &Id, import_path: &str) -> bool {
    let file = file_from_id(id);
    if file.is_empty() {
        return false;
    }
    let imp = import_path.trim().trim_matches('"').trim_matches('`');
    if imp.is_empty() {
        return false;
    }
    let parts: Vec<&str> = imp.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return false;
    }

    // Directory of the .go file ("" for repo-root files).
    let dir = file.rsplit_once('/').map(|(d, _)| d).unwrap_or("");

    if !dir.is_empty() {
        // Nested package dir must be a path suffix of the import.
        // `binding` ↔ `…/binding`; `internal/bytesconv` ↔ `…/internal/bytesconv`.
        // Boundary-aware: require exact match or `/{dir}` suffix (not `mybinding`).
        return imp == dir || imp.ends_with(&format!("/{dir}"));
    }

    // Root-level file: only the module-root import (no subpackage suffix).
    // `github.com/gin-gonic/gin` → yes; `github.com/gin-gonic/gin/binding` → no;
    // `fmt` / single-segment stdlib → no (not this warehouse's root package).
    if parts.len() >= 3 && parts[0].contains('.') {
        // host/org/repo[/sub...] — root package only when no sub path.
        return parts.len() == 3;
    }
    if parts.len() == 2 && parts[0].contains('.') {
        // domain.tld/pkg module root
        return true;
    }
    false
}

/// Parse Go import specs → alias/name → import path.
pub(crate) fn parse_go_imports(root: Node, source: &str) -> ImportTable {
    let mut map = ImportTable::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let k = node.kind();
        if k == "import_spec" {
            let path_node = node.child_by_field_name("path");
            let name_node = node.child_by_field_name("name");
            let Some(path_node) = path_node else {
                continue;
            };
            let path_raw = &source[path_node.start_byte()..path_node.end_byte()];
            let path = path_raw
                .trim()
                .trim_matches('"')
                .trim_matches('`')
                .to_string();
            if path.is_empty() {
                continue;
            }
            let alias = if let Some(n) = name_node {
                let a = source[n.start_byte()..n.end_byte()].trim();
                if a == "." || a == "_" {
                    // Dot/blank: no stable qualifier for package-level calls.
                    continue;
                }
                a.to_string()
            } else {
                // Default alias = last path segment
                path.rsplit('/')
                    .next()
                    .unwrap_or(path.as_str())
                    .to_string()
            };
            map.insert(alias, path);
            continue;
        }
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            stack.push(ch);
        }
    }
    map
}

pub(crate) use super::super::generic_edges::{build_usage_edges, collect_usage_edges};

/// 4-arg build into graph (legacy path without go_all multi-map).
pub(crate) fn build_call_edges(
    blocks: &[BlockInfo],
    source: &str,
    tree: &tree_sitter::Tree,
    graph: &mut crate::CodeGraph,
) {
    let edges = collect_call_edges(blocks, source, tree, None, None);
    for (from, to) in edges {
        graph.add_edge(from, to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::parser::parse_file;
    use std::path::Path;

    fn parse_go(path: &str, src: &str) -> crate::snooper::parser::ParsedFile {
        parse_file(Path::new(path), src).expect("parse go")
    }

    #[test]
    fn package_qualified_does_not_link_other_package_foo() {
        // Package a: Foo in a/a.go; package b: Foo in b/b.go
        // File in package main imports a and calls a.Foo() — must not hit b.Foo.
        let a_src = r#"package a
func Foo() {}
"#;
        let b_src = r#"package b
func Foo() {}
"#;
        let main_src = r#"package main
import "example.com/mod/a"
func main() { a.Foo() }
"#;
        let a = parse_go("a/a.go", a_src);
        let b = parse_go("b/b.go", b_src);
        let main = parse_go("main.go", main_src);
        let a_foo = a
            .blocks
            .iter()
            .find(|x| x.name == "Foo")
            .expect("a.Foo");
        let b_foo = b
            .blocks
            .iter()
            .find(|x| x.name == "Foo")
            .expect("b.Foo");
        let mut go_all: HashMap<String, Vec<Id>> = HashMap::new();
        go_all.insert(
            "Foo".into(),
            vec![a_foo.id.clone(), b_foo.id.clone()],
        );
        // Prefer wrong single-winner map (b) — package resolve must still pick a.
        let mut global = HashMap::new();
        global.insert("Foo".into(), b_foo.id.clone());

        let tree = main.tree.as_ref().unwrap();
        let edges = collect_call_edges(
            &main.blocks,
            main_src,
            tree,
            Some(&global),
            Some(&go_all),
        );
        let main_fn = main
            .blocks
            .iter()
            .find(|x| x.name == "main")
            .expect("main");
        assert!(
            edges
                .iter()
                .any(|(f, t)| f == &main_fn.id && t == &a_foo.id),
            "a.Foo() must link to package a; edges={edges:?}"
        );
        assert!(
            !edges
                .iter()
                .any(|(f, t)| f == &main_fn.id && t == &b_foo.id),
            "a.Foo() must not link to package b; edges={edges:?}"
        );
    }

    #[test]
    fn aliased_import_resolves_to_aliased_package() {
        let b_src = r#"package b
func Foo() {}
"#;
        let main_src = r#"package main
import b_v2 "example.com/mod/b"
func main() { b_v2.Foo() }
"#;
        let b = parse_go("mod/b/b.go", b_src);
        let main = parse_go("main.go", main_src);
        let b_foo = b.blocks.iter().find(|x| x.name == "Foo").unwrap();
        let mut go_all = HashMap::new();
        go_all.insert("Foo".into(), vec![b_foo.id.clone()]);
        let tree = main.tree.as_ref().unwrap();
        let edges = collect_call_edges(
            &main.blocks,
            main_src,
            tree,
            None,
            Some(&go_all),
        );
        let main_fn = main.blocks.iter().find(|x| x.name == "main").unwrap();
        assert!(
            edges
                .iter()
                .any(|(f, t)| f == &main_fn.id && t == &b_foo.id),
            "b_v2.Foo must resolve via alias; edges={edges:?}"
        );
    }

    #[test]
    fn non_import_qualifier_does_not_false_link_method_name() {
        // engine.Run() / ng.Close() — without types, do not invent CALL to a
        // same-name method/func (prometheus LazyLoader.Close trap).
        let src = r#"package main
type Engine struct{}
func (e Engine) Run() {}
func (e Engine) Close() {}
func helper() {
    var engine Engine
    engine.Run()
    engine.Close()
}
"#;
        let parsed = parse_go("main.go", src);
        let tree = parsed.tree.as_ref().unwrap();
        let edges = collect_call_edges(&parsed.blocks, src, tree, None, None);
        let helper = parsed.blocks.iter().find(|x| x.name == "helper").unwrap();
        let run = parsed.blocks.iter().find(|x| x.name == "Run").unwrap();
        let close = parsed.blocks.iter().find(|x| x.name == "Close").unwrap();
        assert!(
            !edges
                .iter()
                .any(|(f, t)| f == &helper.id && (t == &run.id || t == &close.id)),
            "receiver calls must not name-resolve to methods; edges={edges:?}"
        );
    }

    #[test]
    fn package_close_does_not_link_local_method_close() {
        // storage.Close() must use import table; never LazyLoader.Close in same file.
        let storage_src = r#"package storage
func Close() {}
"#;
        let main_src = r#"package promqltest
import "github.com/prometheus/prometheus/storage"
type LazyLoader struct{}
func (ll *LazyLoader) Close() error { return nil }
func NewTestEngineWithOpts() {
    var s interface{ Close() error }
    _ = s
    storage.Close()
}
"#;
        let st = parse_go("storage/storage.go", storage_src);
        let main = parse_go("promql/promqltest/test.go", main_src);
        let st_close = st
            .blocks
            .iter()
            .find(|b| b.name == "Close" && b.kind.contains("function"))
            .expect("storage.Close");
        let ll_close = main
            .blocks
            .iter()
            .find(|b| b.name == "Close" && b.kind.contains("method"))
            .expect("LazyLoader.Close");
        let caller = main
            .blocks
            .iter()
            .find(|b| b.name == "NewTestEngineWithOpts")
            .expect("caller");
        let mut go_all: HashMap<String, Vec<Id>> = HashMap::new();
        go_all.insert(
            "Close".into(),
            vec![st_close.id.clone(), ll_close.id.clone()],
        );
        let mut global = HashMap::new();
        global.insert("Close".into(), ll_close.id.clone());
        let tree = main.tree.as_ref().unwrap();
        let edges = collect_call_edges(
            &main.blocks,
            main_src,
            tree,
            Some(&global),
            Some(&go_all),
        );
        assert!(
            edges
                .iter()
                .any(|(f, t)| f == &caller.id && t == &st_close.id),
            "storage.Close() must hit package storage; edges={edges:?}"
        );
        assert!(
            !edges
                .iter()
                .any(|(f, t)| f == &caller.id && t == &ll_close.id),
            "storage.Close() must not hit LazyLoader.Close; edges={edges:?}"
        );
    }

    #[test]
    fn id_matches_import_path_by_package_dir() {
        // Project-relative warehouse paths (the live shape).
        let binding = Id::new("binding/binding.go", "function_declaration", "abcdefgh");
        assert!(id_matches_import_path(
            &binding,
            "github.com/gin-gonic/gin/binding"
        ));
        assert!(
            !id_matches_import_path(&binding, "github.com/gin-gonic/gin"),
            "subpackage binding must not match parent import …/gin"
        );

        let root = Id::new("gin.go", "function_declaration", "abcdefgh");
        assert!(
            id_matches_import_path(&root, "github.com/gin-gonic/gin"),
            "root gin.go must match module-root import"
        );
        assert!(
            !id_matches_import_path(&root, "github.com/gin-gonic/gin/binding"),
            "root gin.go must not match subpackage import …/binding"
        );
        assert!(
            !id_matches_import_path(&root, "fmt"),
            "root project files must not match stdlib import fmt"
        );

        let nested = Id::new(
            "internal/bytesconv/bytesconv.go",
            "function_declaration",
            "abcdefgh",
        );
        assert!(id_matches_import_path(
            &nested,
            "github.com/gin-gonic/gin/internal/bytesconv"
        ));
    }

    #[test]
    fn parse_imports_default_and_alias() {
        let src = r#"package main
import (
    "fmt"
    b "example.com/mod/binding"
)
"#;
        let parsed = parse_go("main.go", src);
        let table = parse_go_imports(parsed.tree.as_ref().unwrap().root_node(), src);
        assert_eq!(table.get("fmt").map(|s| s.as_str()), Some("fmt"));
        assert_eq!(
            table.get("b").map(|s| s.as_str()),
            Some("example.com/mod/binding")
        );
    }

    #[test]
    fn gin_bind_calls_binding_default_not_gin_default() {
        use std::path::PathBuf;
        let root = std::env::var_os("BUTLER_HOST_MOUNT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("projects")).join("test_repos/gin");
        if !root.join("context.go").is_file() {
            eprintln!("skip: gin not at expected path");
            return;
        }
        let gin_src = std::fs::read_to_string(root.join("gin.go")).unwrap();
        let bind_src = std::fs::read_to_string(root.join("binding/binding.go")).unwrap();
        let ctx_src = std::fs::read_to_string(root.join("context.go")).unwrap();
        let gin_p = parse_go("gin.go", &gin_src);
        let bind_p = parse_go("binding/binding.go", &bind_src);
        let ctx_p = parse_go("context.go", &ctx_src);
        let gin_def = gin_p
            .blocks
            .iter()
            .find(|b| b.name == "Default" && b.kind.contains("function"))
            .expect("gin.Default");
        let binding_def = bind_p
            .blocks
            .iter()
            .find(|b| b.name == "Default" && b.kind.contains("function"))
            .expect("binding.Default");
        let bind_fn = ctx_p
            .blocks
            .iter()
            .find(|b| b.name == "Bind" && b.kind.contains("method"))
            .expect("Bind");
        let mut go_all: HashMap<String, Vec<Id>> = HashMap::new();
        go_all.insert(
            "Default".into(),
            vec![gin_def.id.clone(), binding_def.id.clone()],
        );
        // Poison single-winner to gin.Default (old bug shape).
        let mut global = HashMap::new();
        global.insert("Default".into(), gin_def.id.clone());
        let tree = ctx_p.tree.as_ref().unwrap();
        let edges = collect_call_edges(
            &ctx_p.blocks,
            &ctx_src,
            tree,
            Some(&global),
            Some(&go_all),
        );
        let from_bind: Vec<_> = edges.iter().filter(|(f, _)| f == &bind_fn.id).collect();
        eprintln!("Bind edges: {from_bind:?}");
        eprintln!("gin.Default id={}", gin_def.id);
        eprintln!("binding.Default id={}", binding_def.id);
        assert!(
            from_bind.iter().any(|(_, t)| *t == binding_def.id),
            "Bind must CALL binding.Default; edges={from_bind:?}"
        );
        assert!(
            !from_bind.iter().any(|(_, t)| *t == gin_def.id),
            "Bind must NOT CALL gin.Default; edges={from_bind:?}"
        );
    }

    /// Prometheus twin of the gin trap: `tsdb.DefaultOptions` vs `agent.DefaultOptions`.
    #[test]
    fn prometheus_tsdb_default_options_not_agent() {
        use std::path::PathBuf;
        let root = std::env::var_os("BUTLER_HOST_MOUNT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("projects")).join("test_repos/prometheus");
        let tsdb_path = root.join("tsdb/db.go");
        let agent_path = root.join("tsdb/agent/db.go");
        let caller_path = root.join("util/teststorage/storage.go");
        if !tsdb_path.is_file() || !agent_path.is_file() || !caller_path.is_file() {
            eprintln!("skip: prometheus not at expected path");
            return;
        }
        let tsdb_src = std::fs::read_to_string(&tsdb_path).unwrap();
        let agent_src = std::fs::read_to_string(&agent_path).unwrap();
        let caller_src = std::fs::read_to_string(&caller_path).unwrap();
        let tsdb_p = parse_go("tsdb/db.go", &tsdb_src);
        let agent_p = parse_go("tsdb/agent/db.go", &agent_src);
        let caller_p = parse_go("util/teststorage/storage.go", &caller_src);
        let tsdb_def = tsdb_p
            .blocks
            .iter()
            .find(|b| b.name == "DefaultOptions" && b.kind.contains("function"))
            .expect("tsdb.DefaultOptions");
        let agent_def = agent_p
            .blocks
            .iter()
            .find(|b| b.name == "DefaultOptions" && b.kind.contains("function"))
            .expect("agent.DefaultOptions");
        // Prefer function_declaration — call_expression shells can share the name.
        let new_with_err = caller_p
            .blocks
            .iter()
            .find(|b| b.name == "NewWithError" && b.kind.contains("function_declaration"))
            .expect("NewWithError");
        let mut go_all: HashMap<String, Vec<Id>> = HashMap::new();
        go_all.insert(
            "DefaultOptions".into(),
            vec![tsdb_def.id.clone(), agent_def.id.clone()],
        );
        // Poison single-winner toward agent (wrong package).
        let mut global = HashMap::new();
        global.insert("DefaultOptions".into(), agent_def.id.clone());
        let tree = caller_p.tree.as_ref().unwrap();
        let edges = collect_call_edges(
            &caller_p.blocks,
            &caller_src,
            tree,
            Some(&global),
            Some(&go_all),
        );
        let from: Vec<_> = edges
            .iter()
            .filter(|(f, _)| f == &new_with_err.id)
            .collect();
        eprintln!("NewWithError edges: {from:?}");
        assert!(
            from.iter().any(|(_, t)| *t == tsdb_def.id),
            "tsdb.DefaultOptions() must link to tsdb/db.go; edges={from:?}"
        );
        assert!(
            !from.iter().any(|(_, t)| *t == agent_def.id),
            "tsdb.DefaultOptions() must NOT link to agent; edges={from:?}"
        );
    }
}