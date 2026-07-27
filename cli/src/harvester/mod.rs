//! Harvester: incremental fat graph generation for high-accuracy GNN training data.
//! Lives in Butler for semantic power (CodeGraph + orchestrate).
//!
//! Labeling path: neighborhood cards (structure + source) → frontier LLM → fat labels.
//! GNN learns structure; LLM only stamps sparse gold on Butler node ids.

pub mod agent_loop;
pub mod cards;
pub mod frontier;
pub mod llm;
pub mod mcp_api;
pub mod session;
pub mod source;
pub mod state;
pub mod template;
pub mod tools;
pub mod types;
