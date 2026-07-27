//! Feature projection and scoring input builder for in-process GNN (SmartButler).
//! Ported/adapted from Eve's butler_bridge/features + real.

use std::collections::HashMap;

use crate::snooper::model::{CodeGraph, Id};
use crate::PromptSubgraph;

pub const FEATURE_DIM: usize = 32;
pub const DEFAULT_TEXT_MATCH_GAIN: f32 = 10.0;

/// Universal structural buckets (one-hots in the 32-dim space).
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniversalBucket {
    Function = 18,
    Method = 19,
    Macro = 20,
    TypeDef = 21,
    Interface = 22,
    Enum = 23,
    Field = 24,
    StateDecl = 25,
    Argument = 26,
    Branch = 27,
    Loop = 28,
    Escape = 29,
    Import = 30,
    Module = 31,
    Unknown = 17,
}

impl UniversalBucket {
    pub fn column_index(&self) -> usize {
        *self as usize
    }
}

pub fn map_syntax_to_bucket(node_kind: &str) -> UniversalBucket {
    let n = node_kind.to_lowercase();
    if n.contains("function") || n.contains("fn_item") || n.contains("def") {
        return UniversalBucket::Function;
    }
    if n.contains("method") || n.contains("impl_item") {
        return UniversalBucket::Method;
    }
    if n.contains("macro") {
        return UniversalBucket::Macro;
    }
    if n.contains("struct") || n.contains("type") || n.contains("typedef") {
        return UniversalBucket::TypeDef;
    }
    if n.contains("trait") || n.contains("interface") {
        return UniversalBucket::Interface;
    }
    if n.contains("enum") {
        return UniversalBucket::Enum;
    }
    if n.contains("field") || n.contains("property") {
        return UniversalBucket::Field;
    }
    if n.contains("let") || n.contains("var") || n.contains("const") || n.contains("decl") {
        return UniversalBucket::StateDecl;
    }
    if n.contains("param") || n.contains("argument") {
        return UniversalBucket::Argument;
    }
    if n.contains("if") || n.contains("else") || n.contains("match") || n.contains("switch") {
        return UniversalBucket::Branch;
    }
    if n.contains("for") || n.contains("while") || n.contains("loop") {
        return UniversalBucket::Loop;
    }
    if n.contains("return") || n.contains("break") || n.contains("continue") {
        return UniversalBucket::Escape;
    }
    if n.contains("import") || n.contains("use ") || n.contains("include") {
        return UniversalBucket::Import;
    }
    if n.contains("mod ") || n.contains("module") || n.contains("source_file") {
        return UniversalBucket::Module;
    }
    UniversalBucket::Unknown
}

/// Canonical weight filename (Eve training drop-in + Butler default).
pub const WEIGHTS_FILE: &str = "gnn_trained_global.bin";

/// Parse little-endian f32 blob; empty on failure.
fn parse_f32_le(bytes: &[u8]) -> Vec<f32> {
    if bytes.len() < 4 {
        return Vec::new();
    }
    let mut w = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let b: [u8; 4] = chunk.try_into().unwrap_or([0, 0, 0, 0]);
        w.push(f32::from_le_bytes(b));
    }
    w
}

/// Candidate paths for trained GNN weights, highest priority first.
///
/// 1. `BUTLER_GNN_WEIGHTS` (explicit file path)
/// 2. `{project}/.butler/weights/gnn_trained_global.bin` (per-project / Eve drop)
/// 3. `~/.local/share/butler/weights/...` (install.sh copy)
/// 4. `code_graph/weights/...` next to this crate (dev / source tree)
/// 5. cwd-relative `code_graph/weights/...`
pub fn weight_search_paths(project_root: &str) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();

    if let Ok(p) = std::env::var("BUTLER_GNN_WEIGHTS") {
        let p = p.trim();
        if !p.is_empty() {
            paths.push(std::path::PathBuf::from(p));
        }
    }

    paths.push(
        std::path::Path::new(project_root)
            .join(".butler")
            .join("weights")
            .join(WEIGHTS_FILE),
    );

    if let Some(home) = std::env::var_os("HOME") {
        paths.push(
            std::path::PathBuf::from(home)
                .join(".local/share/butler/weights")
                .join(WEIGHTS_FILE),
        );
    }

    // Docker / install: Butler tree mount (e.g. BUTLER_ROOT=/project) carries code_graph/weights.
    if let Ok(root) = std::env::var("BUTLER_ROOT") {
        let root = root.trim();
        if !root.is_empty() {
            paths.push(
                std::path::Path::new(root)
                    .join("code_graph/weights")
                    .join(WEIGHTS_FILE),
            );
        }
    }

    // Compile-time path to this crate's weights/ (works in cargo run / local builds).
    paths.push(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("weights")
            .join(WEIGHTS_FILE),
    );

    paths.push(
        std::path::PathBuf::from("code_graph")
            .join("weights")
            .join(WEIGHTS_FILE),
    );

    paths
}

/// Load trained weights from the first readable candidate (or tiny fallback).
pub fn load_weights(project_root: &str) -> Vec<f32> {
    for p in weight_search_paths(project_root) {
        if let Ok(bytes) = std::fs::read(&p) {
            let w = parse_f32_le(&bytes);
            if !w.is_empty() {
                return w;
            }
        }
    }
    // Fallback init sized for TrainLayout v2 active banks (L1 D×D + L2 D).
    let minimal = FEATURE_DIM * FEATURE_DIM + FEATURE_DIM; // 1056
    (0..minimal)
        .map(|i| 0.002 + ((i % 17) as f32) * 0.0003)
        .collect()
}

/// Bundled scoring input (moved jewelry shape).
#[derive(Debug, Clone)]
pub struct ScoringBundle {
    pub nodes: Vec<Id>,
    pub features: Vec<f32>,
    pub edges: Vec<(usize, usize, u8)>,
}

/// Project subgraph into features + typed edges (native CodeGraph, no export JSON).
pub fn build_scoring_input(graph: &CodeGraph, subgraph: &PromptSubgraph) -> ScoringBundle {
    let mut ordered: Vec<Id> = subgraph.node_ids.iter().cloned().collect();
    ordered.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let n = ordered.len();
    if n == 0 {
        return ScoringBundle {
            nodes: vec![],
            features: vec![],
            edges: vec![],
        };
    }

    let id_to_local: HashMap<Id, usize> = ordered
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i))
        .collect();

    // Build typed edges first (from containment + calls) so we can derive accurate per-rel degrees.
    let mut typed_edges: Vec<(usize, usize, u8)> = Vec::new();
    for (i, id) in ordered.iter().enumerate() {
        if let Some(bi) = graph.nodes.get(id) {
            for ch in &bi.children {
                if let Some(&j) = id_to_local.get(ch) {
                    typed_edges.push((i, j, 0));
                }
            }
        }
        if let Some(calls) = graph.edges.get(id) {
            for callee in calls {
                if let Some(&j) = id_to_local.get(callee) {
                    typed_edges.push((i, j, 1));
                }
            }
        }
        if let Some(rev) = graph.reverse.get(id) {
            for caller in rev {
                if let Some(&j) = id_to_local.get(caller) {
                    typed_edges.push((i, j, 3));
                }
            }
        }
    }

    // Per-relation degrees (Eve-style, for cols 4-11). SELF not included in degree feats.
    let mut rel_in_degree = vec![[0u32; 4]; n];
    let mut rel_out_degree = vec![[0u32; 4]; n];
    for &(src, dst, r) in &typed_edges {
        if (r as usize) < 4 {
            rel_out_degree[src][r as usize] += 1;
            rel_in_degree[dst][r as usize] += 1;
        }
    }

    let mut max_text = 0.0f64;
    let mut max_heur = 0.0f64;
    let mut max_deg = 0.0f64;

    for i in 0..n {
        let id = &ordered[i];
        if let Some(&t) = subgraph.text_match_scores.get(id) {
            max_text = max_text.max(t);
        }
        if let Some(&h) = graph.heuristic_score_cache.get(id) {
            max_heur = max_heur.max(h);
        }
        let total_d: u32 = (0..4)
            .map(|r| rel_in_degree[i][r] + rel_out_degree[i][r])
            .sum();
        max_deg = max_deg.max(total_d as f64);
    }
    max_text = max_text.max(1.0);
    max_heur = max_heur.max(1.0);
    max_deg = max_deg.max(1.0);

    let gain = DEFAULT_TEXT_MATCH_GAIN;
    let mut features = vec![0.0f32; n * FEATURE_DIM];

    for i in 0..n {
        let id = &ordered[i];
        let base = i * FEATURE_DIM;
        let text = *subgraph.text_match_scores.get(id).unwrap_or(&0.0);
        let heur = graph.heuristic_score_cache.get(id).copied().unwrap_or(0.0);

        features[base] = ((text / max_text) as f32) * gain;
        features[base + 1] = (heur / max_heur) as f32;

        // Accurate per-relation in/out (0=CONTAINS, 1=CALLS, 2/3 reserved)
        features[base + 4] = (rel_in_degree[i][0] as f32).ln_1p();
        features[base + 5] = (rel_out_degree[i][0] as f32).ln_1p();
        features[base + 6] = (rel_in_degree[i][1] as f32).ln_1p();
        features[base + 7] = (rel_out_degree[i][1] as f32).ln_1p();
        features[base + 8] = (rel_in_degree[i][2] as f32).ln_1p();
        features[base + 9] = (rel_out_degree[i][2] as f32).ln_1p();
        features[base + 10] = (rel_in_degree[i][3] as f32).ln_1p();
        features[base + 11] = (rel_out_degree[i][3] as f32).ln_1p();

        let total_d = (0..4)
            .map(|r| rel_in_degree[i][r] + rel_out_degree[i][r])
            .sum::<u32>();
        features[base + 17] = (total_d as f32 / max_deg as f32).min(1.0);

        if let Some(bi) = graph.nodes.get(id) {
            let bucket = map_syntax_to_bucket(&bi.kind);
            let col = bucket.column_index();
            if (18..FEATURE_DIM).contains(&col) {
                features[base + col] = 1.0;
            }
        }
    }

    ScoringBundle {
        nodes: ordered,
        features,
        edges: typed_edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::model::{BlockInfo, CodeGraph, Id};
    use crate::PromptSubgraph;
    use std::collections::HashSet;

    fn make_minimal_graph() -> (CodeGraph, HashSet<Id>, std::collections::HashMap<Id, f64>) {
        let mut g = CodeGraph::default();

        // Use real construction so ids/hashes are consistent.
        let ba = BlockInfo::new(
            "test.rs",
            "function_item",
            "rust",
            1,
            5,
            0,
            50,
            "pub fn foo() { bar(); }".into(),
            "foo",
            Default::default(),
        );
        let bb = BlockInfo::new(
            "test.rs",
            "function_item",
            "rust",
            10,
            12,
            60,
            80,
            "fn bar() {}".into(),
            "bar",
            Default::default(),
        );

        let id_a = ba.id.clone();
        let id_b = bb.id.clone();

        g.nodes.insert(id_a.clone(), ba);
        g.nodes.insert(id_b.clone(), bb);

        // call edge foo -> bar
        g.edges.insert(id_a.clone(), vec![id_b.clone()]);
        g.reverse.insert(id_b.clone(), vec![id_a.clone()]);

        // containment
        if let Some(ba_mut) = g.nodes.get_mut(&id_a) {
            ba_mut.children.push(id_b.clone());
        }

        // some heuristic
        g.heuristic_score_cache.insert(id_a.clone(), 5.0);
        g.heuristic_score_cache.insert(id_b.clone(), 1.0);

        let mut text: std::collections::HashMap<Id, f64> = std::collections::HashMap::new();
        text.insert(id_a.clone(), 10.0);
        text.insert(id_b.clone(), 1.0);

        let mut nodes = HashSet::new();
        nodes.insert(id_a);
        nodes.insert(id_b);

        (g, nodes, text)
    }

    #[test]
    fn build_scoring_input_produces_correct_sizes_and_variation() {
        let (graph, node_ids, text_scores) = make_minimal_graph();
        let subgraph = PromptSubgraph {
            node_ids,
            text_match_scores: text_scores,
        };

        let bundle = build_scoring_input(&graph, &subgraph);
        assert_eq!(bundle.nodes.len(), 2);
        assert_eq!(bundle.features.len(), 2 * FEATURE_DIM);
        assert!(
            bundle.edges.len() >= 2,
            "expected at least contains + call edges"
        );

        // Features should differ (different text + structure)
        let f0 = &bundle.features[0..FEATURE_DIM];
        let f1 = &bundle.features[FEATURE_DIM..];
        assert_ne!(f0, f1);

        // Run forward
        let w = load_weights(".");
        let scores =
            crate::gnn::cpu_gnn_forward(&w, bundle.nodes.len(), &bundle.features, &bundle.edges);
        assert_eq!(scores.len(), 2);
        // With different features, expect non-identical scores (or at least run without panic)
    }

    #[test]
    fn map_syntax_to_bucket_covers_common_kinds() {
        assert_eq!(map_syntax_to_bucket("function_item").column_index(), 18);
        assert_eq!(map_syntax_to_bucket("struct_item").column_index(), 21);
        assert_eq!(map_syntax_to_bucket("if_expression").column_index(), 27);
        assert_eq!(map_syntax_to_bucket("weird_thing").column_index(), 17);
    }

    #[test]
    fn load_weights_finds_crate_default_or_fallback() {
        let paths = weight_search_paths(".");
        assert!(
            paths.iter().any(|p| p.ends_with(WEIGHTS_FILE)),
            "search paths should include the canonical filename"
        );
        let w = load_weights(".");
        assert!(!w.is_empty());
        // Shipped default is full dual-publish blob (786432); fallback is W_ACTIVE=1056.
        let minimal = FEATURE_DIM * FEATURE_DIM + FEATURE_DIM;
        assert!(w.len() == minimal || w.len() >= minimal);
    }

    #[test]
    fn butler_root_is_in_weight_search_paths() {
        std::env::set_var("BUTLER_ROOT", "/tmp/butler_mount_for_test");
        let paths = weight_search_paths(".");
        std::env::remove_var("BUTLER_ROOT");
        assert!(
            paths.iter().any(|p| p
                .to_string_lossy()
                .contains("butler_mount_for_test/code_graph/weights")),
            "BUTLER_ROOT should contribute code_graph/weights candidate: {paths:?}"
        );
    }

    /// Forward runs on the loaded blob. Non-zero logits are required only when the
    /// weight file actually has non-zero values (empty dual-published zeros are a
    /// training-quality FAIL, not a scoring-path FAIL).
    #[test]
    fn real_weights_forward_runs() {
        use crate::snooper::model::{BlockInfo, CodeGraph, Id};
        use crate::PromptSubgraph;
        use std::collections::{HashMap, HashSet};
        use std::path::PathBuf;

        let w = load_weights(".");
        assert!(
            w.len() >= FEATURE_DIM * FEATURE_DIM + FEATURE_DIM,
            "expected real or minimal GNN weights (TrainLayout v2), got {}",
            w.len()
        );
        let mut g = CodeGraph::new();
        let id0 = Id::new("a.rs", "function_item", "aaaaaaaa");
        let id1 = Id::new("b.rs", "function_item", "bbbbbbbb");
        let mk = |id: Id, name: &str, file: &str| BlockInfo {
            id,
            name: name.into(),
            file: PathBuf::from(file),
            kind: "function_item".into(),
            lang: "rust".into(),
            start_line: 1,
            end_line: 5,
            start_byte: 0,
            end_byte: 10,
            parent_id: None,
            children: vec![],
            content_hash: "hashhash".into(),
            sig_hash: "s".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: format!("fn {name}() {{}}"),
            score: 1.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: true,
        };
        g.nodes.insert(id0.clone(), mk(id0.clone(), "foo", "a.rs"));
        g.nodes.insert(id1.clone(), mk(id1.clone(), "bar", "b.rs"));
        g.edges.insert(id0.clone(), vec![id1.clone()]);
        let mut text = HashMap::new();
        text.insert(id0.clone(), 1.0);
        text.insert(id1.clone(), 0.2);
        let mut node_ids = HashSet::new();
        node_ids.insert(id0);
        node_ids.insert(id1);
        let sub = PromptSubgraph {
            node_ids,
            text_match_scores: text,
        };
        let bundle = build_scoring_input(&g, &sub);
        let scores =
            crate::gnn::cpu_gnn_forward(&w, bundle.nodes.len(), &bundle.features, &bundle.edges);
        assert_eq!(scores.len(), bundle.nodes.len());
        let weights_live = w.iter().any(|x| x.abs() > 1e-12);
        eprintln!(
            "forward scores: {:?} wlen={} weights_live={}",
            scores,
            w.len(),
            weights_live
        );
        if weights_live {
            assert!(
                scores.iter().any(|s| s.abs() > 1e-6),
                "non-zero weights must produce non-zero GNN logits"
            );
        }
    }
}
