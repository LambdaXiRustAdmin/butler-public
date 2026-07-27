//! Butler → lambda-eve graph export contract.
//!
//! Serializes the in-memory [`CodeGraph`] into the JSON schema consumed by
//! lambda-eve's `butler_bridge::real` loader. Includes both call edges and
//! containment (parent/child) edges for rich structural signal in GNN training.
//! Butler owns structure; the harness owns feature projection and tensor math.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::model::{BlockInfo, CodeGraph, Id};

/// Export node matching lambda-eve `ExportNode`.
#[derive(Debug, Clone, Serialize)]
pub struct ExportNode {
    pub id: String,
    pub name_hash: u64,
    pub heuristic_score: f64,
    pub degree: f64,
    /// Fast-retriever keyword/text match (prompt-conditioned GNN feature).
    pub text_match: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportEdge {
    pub from: usize,
    pub to: usize,
    /// Relation type for R-GCN Phase 6:
    /// 0 = CONTAINS (AST parent-child)
    /// 1 = CALLS (call expressions)
    /// 2 = IMPLEMENTS (impl to trait)
    /// 3 = REFERENCES (identifier/type refs)
    /// SELF (4) is added synthetically in Eve.
    pub r#type: u8,
}

/// Full export payload written to `.butler/cache/graph_export.json`.
#[derive(Debug, Clone, Serialize)]
pub struct GraphExport {
    pub nodes: Vec<ExportNode>,
    pub edges: Vec<ExportEdge>,
}

fn hash_name(name: &str) -> u64 {
    name.bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64))
}

fn node_degree(graph: &CodeGraph, id: &Id) -> f64 {
    let in_d = graph.reverse.get(id).map_or(0, |v| v.len());
    let out_d = graph.edges.get(id).map_or(0, |v| v.len());
    (in_d + out_d) as f64
}

/// Build export struct from a loaded code graph (stable node index order).
pub fn build_graph_export(graph: &CodeGraph) -> GraphExport {
    let all_ids: HashSet<Id> = graph.nodes.keys().cloned().collect();
    build_graph_export_for_nodes(graph, &all_ids, &HashMap::new(), false)
}

/// Build export for a prompt subgraph only (`node_ids` subset of the full graph).
///
/// If `rich_gnn_relations` is true, edges are classified into the full R-GCN
/// set (0=CONTAINS, 1=CALLS, 2=IMPLEMENTS, 3=REFERENCES). This is feature-gated
/// and only used for the dedicated training bundle data fed to lambda-eve;
/// normal/legacy exports use simple classification (0/1 only) to keep the
/// format stable for other consumers.
pub fn build_graph_export_for_nodes(
    graph: &CodeGraph,
    node_ids: &HashSet<Id>,
    text_match_scores: &HashMap<Id, f64>,
    rich_gnn_relations: bool,
) -> GraphExport {
    let mut nodes_sorted: Vec<&BlockInfo> = graph
        .nodes
        .values()
        .filter(|b| node_ids.contains(&b.id))
        .collect();
    nodes_sorted.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));

    let id_to_idx: HashMap<String, usize> = nodes_sorted
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id.as_str().to_string(), i))
        .collect();

    let nodes: Vec<ExportNode> = nodes_sorted
        .iter()
        .map(|b| ExportNode {
            id: b.id.as_str().to_string(),
            name_hash: hash_name(&b.name),
            heuristic_score: b.score,
            degree: node_degree(graph, &b.id),
            text_match: text_match_scores.get(&b.id).copied().unwrap_or(0.0),
        })
        .collect();

    // Build a quick kind lookup so we can classify relations properly for R-GCN.
    let id_to_kind: HashMap<&str, &str> = nodes_sorted
        .iter()
        .map(|b| (b.id.as_str(), b.kind.as_str()))
        .collect();

    let type_kinds: &[&str] = &[
        "struct_item",
        "enum_item",
        "union_item",
        "trait_item",
        "type_item",
        "class_definition",
        "class_declaration",
        "interface_declaration",
    ];

    let mut edges: Vec<ExportEdge> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (src_id, dst_ids) in &graph.edges {
        if !node_ids.contains(src_id) {
            continue;
        }
        let Some(&from) = id_to_idx.get(src_id.as_str()) else {
            continue;
        };
        for dst_id in dst_ids {
            if !node_ids.contains(dst_id) {
                continue;
            }
            let Some(&to) = id_to_idx.get(dst_id.as_str()) else {
                continue;
            };
            let key = if from <= to { (from, to) } else { (to, from) };
            if seen.insert(key) {
                let rtype = if rich_gnn_relations {
                    let to_kind = id_to_kind.get(dst_id.as_str()).copied().unwrap_or("");
                    let from_kind = id_to_kind.get(src_id.as_str()).copied().unwrap_or("");

                    if from_kind == "impl_item" && to_kind == "trait_item" {
                        2 // IMPLEMENTS
                    } else if type_kinds.contains(&to_kind) {
                        3 // REFERENCES (type/trait use)
                    } else {
                        1 // CALLS (function/method calls)
                    }
                } else {
                    1 // simple legacy classification
                };
                edges.push(ExportEdge {
                    from,
                    to,
                    r#type: rtype,
                });
            }
        }
    }

    // Add containment (parent -> children) edges for proper structural training signal.
    // Uses the parent_id set during visit_node (BlockInfo.children is not wired in
    // the main graph today). This gives the GNN the nesting tree so WL sees structure
    // beyond call edges.
    for block in &nodes_sorted {
        if let Some(ref parent_id) = block.parent_id {
            if node_ids.contains(parent_id) {
                if let (Some(&from), Some(&to)) = (
                    id_to_idx.get(parent_id.as_str()),
                    id_to_idx.get(block.id.as_str()),
                ) {
                    let key = if from <= to { (from, to) } else { (to, from) };
                    if seen.insert(key) {
                        edges.push(ExportEdge {
                            from,
                            to,
                            r#type: 0,
                        }); // CONTAINS for parent-child
                    }
                }
            }
        }
    }

    GraphExport { nodes, edges }
}

/// Write export JSON to `.butler/cache/graph_export.json` under `project_root`.
pub fn write_graph_export(graph: &CodeGraph, project_root: &Path) -> std::io::Result<PathBuf> {
    let all_ids: HashSet<Id> = graph.nodes.keys().cloned().collect();
    write_graph_export_for_nodes(graph, project_root, &all_ids, &HashMap::new(), false)
}

/// Write a subgraph export JSON (micro-export for lambda-eve).
pub fn write_graph_export_for_nodes(
    graph: &CodeGraph,
    project_root: &Path,
    node_ids: &HashSet<Id>,
    text_match_scores: &HashMap<Id, f64>,
    rich_gnn_relations: bool,
) -> std::io::Result<PathBuf> {
    let cache_dir = crate::snooper::ensure_project_butler_cache_dir(project_root)?;
    let path = cache_dir.join("graph_export.json");
    let export =
        build_graph_export_for_nodes(graph, node_ids, text_match_scores, rich_gnn_relations);
    let json = serde_json::to_string_pretty(&export)?;
    std::fs::write(&path, json)?;
    Ok(path)
}
