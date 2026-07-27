//! Language **clusters** — logical workbenches over one CodeGraph warehouse.
//!
//! Agents rarely need every language's idioms at once. Clusters group nodes by
//! language family (c_cpp, rust, go, python, typescript, …) so Trace/Arch can
//! stamp lang + cluster, show a cluster map, and surface cross-cluster bridges
//! without splitting the warehouse into separate graphs.

use crate::snooper::model::{BlockInfo, CodeGraph, Id};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Stable cluster id for a language family (not a separate graph).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClusterId {
    CCpp,
    Rust,
    Go,
    Python,
    TypeScript,
    Other,
}

impl ClusterId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CCpp => "c_cpp",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Other => "other",
        }
    }

    /// Short badge for dense Trace lines (`core:c`, `shell:py`).
    pub fn badge(self) -> &'static str {
        match self {
            Self::CCpp => "core:c",
            Self::Rust => "core:rs",
            Self::Go => "svc:go",
            Self::Python => "shell:py",
            Self::TypeScript => "ui:ts",
            Self::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::CCpp => "C/C++ core",
            Self::Rust => "Rust core",
            Self::Go => "Go service",
            Self::Python => "Python shell",
            Self::TypeScript => "TS/JS surface",
            Self::Other => "Other",
        }
    }
}

/// Map tree-sitter / BlockInfo.lang strings onto a cluster.
pub fn cluster_from_lang(lang: &str) -> ClusterId {
    let l = lang.trim().to_ascii_lowercase();
    match l.as_str() {
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "c++" => ClusterId::CCpp,
        "rust" | "rs" => ClusterId::Rust,
        "go" | "golang" => ClusterId::Go,
        "python" | "py" => ClusterId::Python,
        "typescript" | "javascript" | "ts" | "tsx" | "js" | "jsx" | "svelte" => {
            ClusterId::TypeScript
        }
        _ => ClusterId::Other,
    }
}

/// Prefer `block.lang`; fall back to file extension when lang is empty/unknown.
pub fn cluster_for_block(b: &BlockInfo) -> ClusterId {
    let from_lang = cluster_from_lang(&b.lang);
    if from_lang != ClusterId::Other {
        return from_lang;
    }
    let ext = b
        .file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    cluster_from_lang(&ext)
}

pub fn normalize_lang_label(lang: &str) -> String {
    let c = cluster_from_lang(lang);
    if c != ClusterId::Other {
        return match c {
            ClusterId::CCpp => "c".into(),
            ClusterId::Rust => "rust".into(),
            ClusterId::Go => "go".into(),
            ClusterId::Python => "python".into(),
            ClusterId::TypeScript => "typescript".into(),
            ClusterId::Other => lang.to_ascii_lowercase(),
        };
    }
    if lang.is_empty() {
        "?".into()
    } else {
        lang.to_ascii_lowercase()
    }
}

#[derive(Debug, Clone)]
pub struct ClusterSummary {
    pub id: ClusterId,
    pub nodes: usize,
    pub files: usize,
    /// Up to a few entry-looking paths (basename or short rel path).
    pub entries: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BridgeEdge {
    pub from_id: Id,
    pub to_id: Id,
    pub from_name: String,
    pub to_name: String,
    pub from_file: String,
    pub to_file: String,
    pub from_lang: String,
    pub to_lang: String,
    pub from_cluster: ClusterId,
    pub to_cluster: ClusterId,
}

/// Histogram of clusters over a block set (scoped working set).
pub fn summarize_clusters<'a>(blocks: impl IntoIterator<Item = &'a BlockInfo>) -> Vec<ClusterSummary> {
    let mut by: HashMap<ClusterId, (usize, HashSet<String>, Vec<(i32, String)>)> = HashMap::new();
    for b in blocks {
        let c = cluster_for_block(b);
        let e = by.entry(c).or_insert_with(|| (0, HashSet::new(), Vec::new()));
        e.0 += 1;
        let f = b.file.to_string_lossy().to_string();
        e.1.insert(f.clone());
        let entry_score = entry_hint_score(&f, &b.name);
        if entry_score > 0 {
            e.2.push((entry_score, short_display_path(&f)));
        }
    }
    let mut out: Vec<ClusterSummary> = by
        .into_iter()
        .map(|(id, (nodes, files, mut entries))| {
            entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            entries.dedup_by(|a, b| a.1 == b.1);
            let entry_paths: Vec<String> = entries.into_iter().take(4).map(|(_, p)| p).collect();
            ClusterSummary {
                id,
                nodes,
                files: files.len(),
                entries: entry_paths,
            }
        })
        .collect();
    out.sort_by(|a, b| b.nodes.cmp(&a.nodes).then_with(|| a.id.cmp(&b.id)));
    out
}

fn entry_hint_score(path: &str, name: &str) -> i32 {
    let p = path.replace('\\', "/").to_ascii_lowercase();
    let base = Path::new(&p)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let mut s = 0i32;
    if matches!(
        base,
        "main.rs"
            | "lib.rs"
            | "mod.rs"
            | "main.py"
            | "main.go"
            | "__main__.py"
            | "emcc.py"
            | "link.py"
            | "index.ts"
            | "index.js"
            | "app.py"
            | "server.py"
            | "cli.py"
    ) {
        s += 100;
    }
    if p.contains("/src/") || p.contains("/include/") || p.contains("/tools/") {
        s += 20;
    }
    if matches!(name, "main" | "Main" | "run" | "start" | "init") {
        s += 15;
    }
    s
}

fn short_display_path(path: &str) -> String {
    let p = path.replace('\\', "/");
    let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 {
        p.trim_start_matches("./").to_string()
    } else {
        parts[parts.len() - 3..].join("/")
    }
}

/// Cross-cluster edges (polyglot interconnect fabric). Prefer distinctive names.
///
/// Walks **CALL** adjacency and **typed bridge** maps (Export/Ipc/Twin).
pub fn find_bridges(
    graph: &CodeGraph,
    scoped: &[&BlockInfo],
    max: usize,
) -> Vec<BridgeEdge> {
    if max == 0 || (graph.edges.is_empty() && graph.bridge_fwd.is_empty()) {
        return vec![];
    }
    let scoped_ids: HashSet<&Id> = if scoped.is_empty() {
        HashSet::new()
    } else {
        scoped.iter().map(|b| &b.id).collect()
    };
    let in_scope = |id: &Id| scoped_ids.is_empty() || scoped_ids.contains(id);

    // When scoped is non-empty, only walk edges **from scoped seeds** (O(scoped·deg)).
    let mut bridges: Vec<BridgeEdge> = Vec::new();
    let push_pair = |from_id: &Id,
                     from: &BlockInfo,
                     to_id: &Id,
                     bridges: &mut Vec<BridgeEdge>| {
        if !in_scope(to_id) {
            return;
        }
        let fc = cluster_for_block(from);
        if fc == ClusterId::Other {
            return;
        }
        let Some(to) = graph.nodes.get(to_id) else {
            return;
        };
        let tc = cluster_for_block(to);
        if tc == ClusterId::Other || tc == fc {
            return;
        }
        if from.name.len() < 4 && to.name.len() < 6 {
            return;
        }
        if is_bridge_noise_name(&from.name) || is_bridge_noise_name(&to.name) {
            return;
        }
        if !is_bridge_worthy_name(&from.name) && !is_bridge_worthy_name(&to.name) {
            return;
        }
        bridges.push(BridgeEdge {
            from_id: from_id.clone(),
            to_id: to_id.clone(),
            from_name: from.name.clone(),
            to_name: to.name.clone(),
            from_file: from.file.to_string_lossy().replace('\\', "/"),
            to_file: to.file.to_string_lossy().replace('\\', "/"),
            from_lang: normalize_lang_label(&from.lang),
            to_lang: normalize_lang_label(&to.lang),
            from_cluster: fc,
            to_cluster: tc,
        });
    };
    let push_from = |from_id: &Id, from: &BlockInfo, bridges: &mut Vec<BridgeEdge>| {
        if let Some(tos) = graph.edges.get(from_id) {
            for to_id in tos {
                push_pair(from_id, from, to_id, bridges);
            }
        }
        if let Some(tos) = graph.bridge_fwd.get(from_id) {
            for (to_id, _) in tos {
                push_pair(from_id, from, to_id, bridges);
            }
        }
    };

    if scoped.is_empty() {
        for from_id in graph.edges.keys() {
            let Some(from) = graph.nodes.get(from_id) else {
                continue;
            };
            push_from(from_id, from, &mut bridges);
        }
        for from_id in graph.bridge_fwd.keys() {
            if graph.edges.contains_key(from_id) {
                continue; // already walked
            }
            let Some(from) = graph.nodes.get(from_id) else {
                continue;
            };
            push_from(from_id, from, &mut bridges);
        }
    } else {
        for b in scoped {
            push_from(&b.id, b, &mut bridges);
        }
    }

    bridges.sort_by(|a, b| {
        bridge_rank(b)
            .cmp(&bridge_rank(a))
            .then_with(|| a.from_name.cmp(&b.from_name))
    });
    bridges.dedup_by(|a, b| a.from_id == b.from_id && a.to_id == b.to_id);
    bridges.truncate(max);
    bridges
}

fn is_bridge_noise_name(name: &str) -> bool {
    if name.is_empty() || name == "unknown" {
        return true;
    }
    // Module dunders / package metadata are not interconnect endpoints.
    name.starts_with("__") && name.ends_with("__")
}

fn is_bridge_worthy_name(name: &str) -> bool {
    // Align with linker polyglot bar: short / generic dual-stack names out.
    if is_bridge_noise_name(name) {
        return false;
    }
    if name.len() < 8 {
        return false;
    }
    if name.starts_with("test_") || name.ends_with("_test") {
        return name.len() >= 20;
    }
    if name.chars().all(|c| c.is_ascii_lowercase()) {
        return name.contains('_') && name.len() >= 12;
    }
    name.contains('_')
        || name.chars().skip(1).any(|c| c.is_ascii_uppercase())
        || name.len() >= 14
}

fn path_is_testish(path: &str) -> bool {
    let p = path.replace('\\', "/").to_ascii_lowercase();
    p.contains("/tests/")
        || p.contains("/test/")
        || p.contains("/__tests__/")
        || p.contains("_test.")
        || p.contains("_tests.")
        || p.contains("/testdata/")
        || p.contains("/fixtures/")
}

fn path_is_productionish(path: &str) -> bool {
    let p = path.replace('\\', "/").to_ascii_lowercase();
    if path_is_testish(&p) {
        return false;
    }
    // `tools/` is often build glue / codegen — not production interconnect.
    p.contains("/include/")
        || p.contains("/src/")
        || p.contains("/lib/")
        || p.contains("/crates/")
        || p.ends_with(".h")
        || p.ends_with(".hpp")
}

/// Higher = better bridge for Arch (production cores over test glue).
fn bridge_rank(b: &BridgeEdge) -> i32 {
    let mut s = (b.from_name.len().min(40) + b.to_name.len().min(40)) as i32;
    s += entry_hint_score(&b.from_file, &b.from_name) / 5;
    s += entry_hint_score(&b.to_file, &b.to_name) / 5;

    let ff = b.from_file.as_str();
    let tf = b.to_file.as_str();
    if path_is_testish(ff) {
        s -= 220;
    }
    if path_is_testish(tf) {
        s -= 180;
    }
    if b.from_name.starts_with("test_") || b.to_name.starts_with("test_") {
        s -= 80;
    }
    if path_is_productionish(ff) {
        s += 50;
    }
    if path_is_productionish(tf) {
        s += 60;
    }
    // Prefer shell → core over core → shell as the displayed direction for ranking
    // (both still valid edges; score slightly favors py/ts callers into c/rs).
    if matches!(b.from_cluster, ClusterId::Python | ClusterId::TypeScript)
        && matches!(b.to_cluster, ClusterId::CCpp | ClusterId::Rust)
    {
        s += 25;
    }
    s
}

/// Suggested scope path prefixes for a cluster (from entry paths + common roots).
pub fn suggested_scopes_for_cluster(summary: &ClusterSummary) -> Vec<String> {
    let mut out = Vec::new();
    for e in &summary.entries {
        if let Some(parent) = Path::new(e).parent() {
            let s = parent.to_string_lossy().to_string();
            if !s.is_empty() && s != "." && !out.contains(&s) {
                out.push(if s.ends_with('/') { s } else { format!("{s}/") });
            }
        }
    }
    // Language-typical defaults when no entries
    if out.is_empty() {
        match summary.id {
            ClusterId::Rust => out.extend(["src/".into(), "crates/".into()]),
            ClusterId::CCpp => out.extend(["src/".into(), "include/".into(), "lib/".into()]),
            // Prefer package/src over tools/ (build glue often lives under tools/).
            ClusterId::Python => out.extend(["src/".into(), "lib/".into()]),
            ClusterId::Go => out.push("./".into()),
            ClusterId::TypeScript => out.extend(["src/".into(), "app/".into()]),
            ClusterId::Other => {}
        }
    }
    out.truncate(3);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::model::BlockInfo;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn blk(name: &str, file: &str, lang: &str) -> BlockInfo {
        BlockInfo::new(
            PathBuf::from(file),
            "function_definition",
            lang,
            1,
            10,
            0,
            10,
            String::new(),
            name,
            HashSet::new(),
        )
    }

    #[test]
    fn clusters_from_lang_and_ext() {
        assert_eq!(cluster_from_lang("python"), ClusterId::Python);
        assert_eq!(cluster_from_lang("c"), ClusterId::CCpp);
        assert_eq!(cluster_from_lang("rs"), ClusterId::Rust);
        let b = blk("foo", "src/main.go", "");
        // empty lang → ext
        assert_eq!(cluster_for_block(&b), ClusterId::Go);
    }

    #[test]
    fn summarize_orders_by_size() {
        let blocks = vec![
            blk("a", "a.py", "python"),
            blk("b", "b.py", "python"),
            blk("c", "c.c", "c"),
        ];
        let s = summarize_clusters(blocks.iter());
        assert_eq!(s[0].id, ClusterId::Python);
        assert_eq!(s[0].nodes, 2);
        assert_eq!(s[1].id, ClusterId::CCpp);
    }

    #[test]
    fn bridge_rank_prefers_production_over_test_glue() {
        let prod = BridgeEdge {
            from_id: Id::new("tools/shell.py", "function_definition", "aaaaaaaa"),
            to_id: Id::new("include/core.h", "function_definition", "bbbbbbbb"),
            from_name: "wrap_core_magic".into(),
            to_name: "core_magic_dispatch".into(),
            from_file: "tools/shell.py".into(),
            to_file: "include/core.h".into(),
            from_lang: "python".into(),
            to_lang: "c".into(),
            from_cluster: ClusterId::Python,
            to_cluster: ClusterId::CCpp,
        };
        let test = BridgeEdge {
            from_id: Id::new("tests/test_x.py", "function_definition", "cccccccc"),
            to_id: Id::new("include/core.h", "function_definition", "bbbbbbbb"),
            from_name: "test_core_magic_dispatch_aliases_member".into(),
            to_name: "core_magic_dispatch".into(),
            from_file: "tests/test_x.py".into(),
            to_file: "include/core.h".into(),
            from_lang: "python".into(),
            to_lang: "c".into(),
            from_cluster: ClusterId::Python,
            to_cluster: ClusterId::CCpp,
        };
        assert!(
            bridge_rank(&prod) > bridge_rank(&test),
            "prod={} test={}",
            bridge_rank(&prod),
            bridge_rank(&test)
        );
    }
}
