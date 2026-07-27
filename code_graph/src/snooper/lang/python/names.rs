//! Python-specific global name preference for call-edge resolution.
//!
//! Shared unique-name map (builder) drops ambiguous names. Python monorepos have
//! many `main` / `run` / `compile` defs — without a preference, cross-file edges
//! never form. Prefer production package code over tests/third_party.
//!
//! Path heuristics are **ecosystem-general** (src/lib/app/cli), not single-repo.

use crate::{BlockInfo, Id};
use std::collections::HashMap;
use std::path::Path;

/// Fill `map` with preferred Ids for names that appear more than once among Python blocks.
/// Does not overwrite entries already present (unique map wins).
pub(crate) fn prefer_ambiguous_python_names(
    nodes: &HashMap<Id, BlockInfo>,
    map: &mut HashMap<String, Id>,
) {
    let mut by_name: HashMap<&str, Vec<&BlockInfo>> = HashMap::new();
    for b in nodes.values() {
        if !is_python_block(b) || b.name.is_empty() {
            continue;
        }
        if !is_def_kind(&b.kind) {
            continue;
        }
        by_name.entry(b.name.as_str()).or_default().push(b);
    }

    for (name, candidates) in by_name {
        if map.contains_key(name) {
            continue; // already unique / claimed
        }
        if candidates.len() < 2 {
            // Single py def that lost to a non-py unique collision elsewhere — still useful.
            if candidates.len() == 1 {
                map.insert(name.to_string(), candidates[0].id.clone());
            }
            continue;
        }
        if let Some(best) = pick_preferred(candidates) {
            map.insert(name.to_string(), best.id.clone());
        }
    }
}

fn is_python_block(b: &BlockInfo) -> bool {
    b.lang.eq_ignore_ascii_case("python")
        || b.lang.eq_ignore_ascii_case("py")
        || b.file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "py")
}

fn is_def_kind(kind: &str) -> bool {
    let k = kind.to_ascii_lowercase();
    k.contains("function_definition")
        || k.contains("async_function_definition")
        || k.contains("class_definition")
}

fn pick_preferred(candidates: Vec<&BlockInfo>) -> Option<&BlockInfo> {
    candidates.into_iter().max_by_key(|b| preference_score(b))
}

fn preference_score(b: &BlockInfo) -> i32 {
    let path = b
        .file
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let mut s = 0i32;

    // Production package layouts (general Python packaging)
    if path.contains("/src/") || path.starts_with("src/") {
        s += 40;
    }
    if path.contains("/lib/") || path.starts_with("lib/") {
        s += 30;
    }
    if path.contains("/app/") || path.starts_with("app/") {
        s += 35;
    }
    // CLI / scripts dirs — mild boost (not above src/app)
    if path.contains("/cli/") || path.contains("/scripts/") || path.contains("/bin/") {
        s += 15;
    }
    // Build/glue helpers — mild, not emscripten-only; still below primary package roots
    if path.contains("/tools/") || path.starts_with("tools/") {
        s += 10;
    }
    if path_is_entry_module(&path) {
        s += 25;
    }

    // Prefer real defs
    if b.kind.contains("function_definition") {
        s += 20;
    }
    if b.kind.contains("class_definition") {
        s += 10;
    }

    // Generic entry-ish names (not project-specific pipeline ids)
    let n = b.name.as_str();
    if matches!(n, "main" | "run" | "cli" | "app" | "server" | "create_app") {
        s += 12;
    }

    // Demote noise
    if is_testish(&path) {
        s -= 80;
    }
    if path.contains("/third_party/")
        || path.contains("/third-party/")
        || path.contains("/vendor/")
        || path.contains("/site-packages/")
        || path.contains("/.venv/")
        || path.contains("/venv/")
    {
        s -= 60;
    }
    if path.contains("/docs/") || path.contains("/examples/") || path.contains("/benchmarks/") {
        s -= 40;
    }

    s -= (b.start_line as i32).min(500) / 50;
    s
}

/// Shallow entry modules: `main.py`, `app.py`, package `__main__.py`, etc.
fn path_is_entry_module(path: &str) -> bool {
    let p = path.trim_start_matches('/');
    let depth = p.split('/').filter(|s| !s.is_empty()).count();
    Path::new(p)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|base| {
            matches!(
                base,
                "main.py"
                    | "cli.py"
                    | "app.py"
                    | "server.py"
                    | "__main__.py"
                    | "manage.py"
                    | "wsgi.py"
                    | "asgi.py"
            ) && depth <= 4
        })
}

fn is_testish(path: &str) -> bool {
    path.contains("/test/")
        || path.contains("/tests/")
        || path.contains("/testing/")
        || path.contains("_test.py")
        || path.contains("/test_")
        || path.ends_with("_test.py")
        || path.contains("/conftest.py")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::model::Id;
    use std::path::PathBuf;

    fn blk(name: &str, file: &str, kind: &str) -> BlockInfo {
        let hash = format!("{name:0<16}");
        BlockInfo {
            id: Id::new(file, kind, &hash),
            name: name.into(),
            file: PathBuf::from(file),
            kind: kind.into(),
            lang: "python".into(),
            start_line: 1,
            end_line: 2,
            start_byte: 0,
            end_byte: 1,
            parent_id: None,
            children: vec![],
            content_hash: hash,
            sig_hash: "s".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            score: 0.0,
            usages: vec![],
            external_crates: Default::default(),
            is_highly_connected: false,
        }
    }

    #[test]
    fn prefers_src_run_over_test_run() {
        let mut nodes = HashMap::new();
        let a = blk("run", "src/pkg/run.py", "function_definition");
        let b = blk("run", "tests/test_run.py", "function_definition");
        let id_src = a.id.clone();
        nodes.insert(a.id.clone(), a);
        nodes.insert(b.id.clone(), b);
        let mut map = HashMap::new();
        prefer_ambiguous_python_names(&nodes, &mut map);
        assert_eq!(map.get("run"), Some(&id_src));
    }

    #[test]
    fn prefers_app_over_tools() {
        // tools/ is mild glue — primary package wins
        let mut nodes = HashMap::new();
        let a = blk("compile", "tools/compile.py", "function_definition");
        let b = blk("compile", "src/pipeline/compile.py", "function_definition");
        let id_src = b.id.clone();
        nodes.insert(a.id.clone(), a);
        nodes.insert(b.id.clone(), b);
        let mut map = HashMap::new();
        prefer_ambiguous_python_names(&nodes, &mut map);
        assert_eq!(map.get("compile"), Some(&id_src));
    }
}
