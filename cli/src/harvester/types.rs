//! Core data types for the fat graph output.
//! Explicit structs matching the target schema for training data.
//! Dense and direct.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FatNode {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub node_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub range: String,
    #[serde(default)]
    pub snippet: String,
    #[serde(default)]
    pub exploration_note: String,
    #[serde(default)]
    pub is_critical: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FatEdge {
    pub from: String,
    pub to: String,
    pub edge_type: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FatGraph {
    pub query: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub repo: String,
    pub nodes: Vec<FatNode>,
    pub edges: Vec<FatEdge>,
    pub critical_node_ids: Vec<String>,
    pub rejected_node_ids: Vec<String>,
    pub exploration_summary: String,
    /// Typed runtime bridges only (`edge_type`: `export` | `ipc` | `twin`). Track P.6 / Phase 8.
    #[serde(default)]
    pub interconnect_edges: Vec<FatEdge>,
}
