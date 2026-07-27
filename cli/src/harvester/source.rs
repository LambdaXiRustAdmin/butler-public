//! Source for harvest: repo + Butler CodeGraph (max reuse of Tree-sitter data).
//! Loads repo and optional export for graph context.

use code_graph::load_graph;
use code_graph::snooper::model::CodeGraph;
use code_graph::BridgeKind;
use std::cell::OnceCell;
use std::path::PathBuf;

#[derive(Debug)]
pub struct Source {
    pub repo: PathBuf,
    pub export: Option<PathBuf>,
    graph: OnceCell<CodeGraph>,
}

impl Source {
    pub fn new(repo: PathBuf, export: Option<PathBuf>) -> Self {
        Self {
            repo,
            export,
            graph: OnceCell::new(),
        }
    }

    // Max use of Butler data (Tree-sitter CodeGraph + export): load nodes from export for accurate IDs.
    // Fallback to polyglot scan. Real full CodeGraph for rich.
    // Load path (M2c): `export` → `rich` (CodeGraph) → `dir_scan` — logged once per call.
    pub fn code_graph_nodes(&self) -> Vec<String> {
        // Prefer export for butler-style IDs for alignment.
        let mut nodes = vec![];
        if let Some(ref exp) = self.export {
            if let Ok(data) = std::fs::read_to_string(exp) {
                if let Ok(export) = serde_json::from_str::<serde_json::Value>(&data) {
                    if let Some(ns) = export.get("nodes").and_then(|v| v.as_array()) {
                        for n in ns {
                            if let Some(id) = n.get("id").and_then(|v| v.as_str()) {
                                nodes.push(id.to_string());
                            }
                        }
                    }
                }
            }
        }
        if !nodes.is_empty() {
            eprintln!(
                "🌾 harvest load_path=export nodes={} export={}",
                nodes.len(),
                self.export
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            );
            return nodes;
        }
        // Fallback to full CodeGraph or scan.
        let rich = self.rich_nodes();
        if !rich.is_empty() {
            eprintln!(
                "🌾 harvest load_path=rich nodes={} repo={}",
                rich.len(),
                self.repo.display()
            );
            return rich;
        }
        let exts = ["rs", "py", "ts", "tsx", "js", "jsx", "go", "c", "cpp", "h"];
        if let Ok(entries) = std::fs::read_dir(&self.repo) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_file() {
                    if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                        if exts.contains(&ext) {
                            if let Some(s) = p.file_name().and_then(|n| n.to_str()) {
                                nodes.push(s.to_string());
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "🌾 harvest load_path=dir_scan nodes={} repo={}",
            nodes.len(),
            self.repo.display()
        );
        nodes
    }

    /// Typed interconnect inventory summary (Track P.6 / Phase 8).
    pub fn interconnect_nodes(&self) -> Vec<String> {
        if let Some(g) = self.load_code_graph() {
            let n = g.total_bridge_edges();
            if n == 0 {
                return vec![];
            }
            let mut export = 0usize;
            let mut ipc = 0usize;
            let mut twin = 0usize;
            for tos in g.bridge_fwd.values() {
                for (_, k) in tos {
                    match k {
                        BridgeKind::Export => export += 1,
                        BridgeKind::Ipc => ipc += 1,
                        BridgeKind::Twin => twin += 1,
                    }
                }
            }
            return vec![format!(
                "typed_bridges_{n}_export_{export}_ipc_{ipc}_twin_{twin}"
            )];
        }
        vec![]
    }

    /// Typed interconnect edges from runtime bridge maps (P.6 / L2.4).
    ///
    /// Returns `(from_id, to_id, edge_type, reason)` where `edge_type` is
    /// `export` | `ipc` | `twin` — **not** unlabeled CALL soup.
    pub fn interconnect_edges(&self) -> Vec<(String, String, String, String)> {
        self.load_code_graph()
            .map(typed_interconnect_edges)
            .unwrap_or_default()
    }

    // Full CodeGraph load for Tree-sitter rich data (nodes, edges, interconnect).
    // Cached for zero repeated parses (perf).
    pub fn load_code_graph(&self) -> Option<&CodeGraph> {
        Some(self.graph.get_or_init(|| {
            let skips: Vec<String> = vec![];
            load_graph(&self.repo, None, &skips)
        }))
    }

    pub fn rich_nodes(&self) -> Vec<String> {
        if let Some(g) = self.load_code_graph() {
            // Use full Tree-sitter data: BlockInfo ids for accuracy.
            g.nodes.keys().map(|id| id.as_str().to_string()).collect()
        } else {
            self.code_graph_nodes()
        }
    }

    pub fn get_rich_context(&self) -> Vec<(String, String, String, String, String, bool, usize)> {
        // id, name, kind, lang, snippet, is_highly_connected, external_crates_len
        // Max Tree-sitter/BlockInfo data without extra copies.
        if let Some(g) = self.load_code_graph() {
            g.nodes
                .values()
                .map(|b| {
                    (
                        b.id.as_str().to_string(),
                        b.name.clone(),
                        b.kind.clone(),
                        b.lang.clone(),
                        b.source.clone(),
                        b.is_highly_connected,
                        b.external_crates.len(),
                    )
                })
                .collect()
        } else {
            vec![]
        }
    }
}

impl Clone for Source {
    fn clone(&self) -> Self {
        // Clone only paths (cheap); fresh cache cell so load once per instance.
        Self {
            repo: self.repo.clone(),
            export: self.export.clone(),
            graph: OnceCell::new(),
        }
    }
}

/// Pure extract of typed bridges for harvester fat labels (unit-testable).
///
/// Walks `bridge_fwd` only (each directed Export/Ipc/Twin once). CALL adjacency is ignored.
pub fn typed_interconnect_edges(g: &CodeGraph) -> Vec<(String, String, String, String)> {
    let mut out = Vec::new();
    for (from, tos) in &g.bridge_fwd {
        let Some(fb) = g.nodes.get(from) else {
            continue;
        };
        for (to, kind) in tos {
            let Some(tb) = g.nodes.get(to) else {
                continue;
            };
            let edge_type = kind.as_relation_label().to_string();
            let reason = format!(
                "{} {}→{} ({} → {})",
                edge_type, fb.lang, tb.lang, fb.name, tb.name
            );
            out.push((
                from.as_str().to_string(),
                to.as_str().to_string(),
                edge_type,
                reason,
            ));
        }
    }
    out.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    out.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1 && a.2 == b.2);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph::snooper::model::{BlockInfo, Id};
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn blk(file: &str, lang: &str, kind: &str, name: &str) -> BlockInfo {
        let id = Id::new(file, kind, &format!("h_{name}"));
        BlockInfo {
            id: id.clone(),
            name: name.to_string(),
            file: PathBuf::from(file),
            kind: kind.to_string(),
            lang: lang.to_string(),
            start_line: 1,
            end_line: 2,
            start_byte: 0,
            end_byte: 1,
            parent_id: None,
            children: vec![],
            content_hash: format!("h_{name}"),
            sig_hash: format!("s_{name}"),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            score: 0.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        }
    }

    #[test]
    fn typed_interconnect_uses_bridges_not_call() {
        let mut g = CodeGraph::new();
        let py = blk("word_count/__init__.py", "python", "function_definition", "search_py");
        let rs = blk("src/lib.rs", "rust", "function_item", "search");
        let py_id = py.id.clone();
        let rs_id = rs.id.clone();
        g.add_block(py);
        g.add_block(rs);
        // Lying CALL soup (must not appear in harvest interconnect).
        g.add_edges_batch(vec![(py_id.clone(), rs_id.clone())]);
        // Truth: typed Export bridge.
        g.add_bridge_edge(py_id.clone(), rs_id.clone(), BridgeKind::Export);

        let edges = typed_interconnect_edges(&g);
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].0, py_id.as_str());
        assert_eq!(edges[0].1, rs_id.as_str());
        assert_eq!(edges[0].2, "export");
        assert!(edges[0].3.contains("export"), "{}", edges[0].3);
        assert!(edges[0].3.contains("python"));
        assert!(edges[0].3.contains("rust"));
    }

    #[test]
    fn typed_interconnect_stamps_ipc_and_skips_call_only() {
        let mut g = CodeGraph::new();
        let ts = blk("src/App.svelte", "typescript", "function_declaration", "log");
        let rs = blk("src-tauri/src/cmd.rs", "rust", "function_item", "log_operation");
        let ts_id = ts.id.clone();
        let rs_id = rs.id.clone();
        g.add_block(ts);
        g.add_block(rs);
        g.add_bridge_edge(ts_id.clone(), rs_id.clone(), BridgeKind::Ipc);
        let edges = typed_interconnect_edges(&g);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].2, "ipc");
        assert!(edges[0].3.contains("log_operation"));
    }
}
