//! GNN scoring (**Butler Rank** / SmartButler) — in-process at `code_graph/src/gnn`.
//!
//! - **Code**: `projection.rs` (features + bundle + weight load), `forward.rs` (CPU R-GCN)
//! - **Weights**: `code_graph/weights/` (canonical default + MANIFEST); see that README
//! - **Product status:** 🅿️ **parked as optional addon** — Core does not require this path.
//!   Master switch: `agent.use_neural` (default **false**). Plan: `plans/butler-rank-addon.md`.
//!
//! In-process only. No Eve subprocess for scoring. Eve remains the Xi connector + trainer.

mod forward;
mod projection;

pub use forward::cpu_gnn_forward;
pub use projection::{
    build_scoring_input, load_weights, map_syntax_to_bucket, weight_search_paths, ScoringBundle,
    UniversalBucket, DEFAULT_TEXT_MATCH_GAIN, FEATURE_DIM, WEIGHTS_FILE,
};

pub use forward::{HIDDEN as FORWARD_HIDDEN, NUM_REL};
pub use projection::FEATURE_DIM as PROJ_DIM; // avoid conflict in reexport

/// High level helper (optional convenience for direct callers).
pub fn score_subgraph(
    graph: &crate::snooper::model::CodeGraph,
    subgraph: &crate::PromptSubgraph,
    weights: &[f32],
) -> std::collections::HashMap<crate::snooper::model::Id, f32> {
    let bundle = build_scoring_input(graph, subgraph);
    let raw = cpu_gnn_forward(weights, bundle.nodes.len(), &bundle.features, &bundle.edges);
    bundle.nodes.into_iter().zip(raw).collect()
}
