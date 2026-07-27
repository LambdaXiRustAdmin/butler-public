//! Core data types and pure helpers for the Composer.
//!
//! Extracted via Strangler Fig from the original monolithic composer.rs.
//! Contains the result types (ComposedContext, ContextMetadata) and the
//! small pure functions that operate on the graph for hub handling.

use crate::snooper::context::ContextMode;
use crate::{BlockInfo, CodeGraph, Id};

/// Returns the minimum total degree (in + out) for a node to be considered
/// a "highly connected component" (top X% by degree in the project).
/// TODO: Make this percentage configurable (default 5%).
pub(crate) fn highly_connected_threshold(graph: &CodeGraph) -> usize {
    const TOP_PERCENT: f64 = 0.05;

    if graph.nodes.is_empty() {
        return usize::MAX;
    }

    let mut degrees: Vec<usize> = graph
        .nodes
        .keys()
        .map(|id| {
            let in_deg = graph.reverse.get(id).map_or(0, |v| v.len());
            let out_deg = graph.edges.get(id).map_or(0, |v| v.len());
            in_deg + out_deg
        })
        .collect();

    degrees.sort_unstable_by(|a, b| b.cmp(a)); // descending

    let index = ((TOP_PERCENT * degrees.len() as f64) as usize)
        .max(1)
        .min(degrees.len() - 1);
    degrees[index]
}

/// Checks if a block is part of a highly connected component (top 5%).
/// Prefers the build-time flag set by `CodeGraph::compute_hubs`.
pub(crate) fn is_highly_connected(block: &BlockInfo, _graph: &CodeGraph) -> bool {
    block.is_highly_connected
}

// =============================================================================
// Future rich return type
// =============================================================================

/// Rich result type returned by the composer.
/// Contains both the final text and useful metadata about what was included.
#[derive(Debug, Clone)]
pub struct ComposedContext {
    /// The final formatted context ready to be sent to an LLM
    pub text: String,

    /// Approximate token count of the final `text`
    pub token_count: usize,

    /// Blocks that were included with their full source code
    pub blocks_included: Vec<Id>,

    /// Blocks that were included only as signatures or short summaries
    pub blocks_summarized: Vec<Id>,

    /// Blocks that were considered but ultimately omitted (e.g. due to token budget)
    pub blocks_omitted: Vec<Id>,

    /// Which retrieval mode was used to produce this context
    pub mode: ContextMode,

    /// Additional structured metadata (new)
    pub metadata: ContextMetadata,
}

/// Additional structured information about the context generation process.
#[derive(Debug, Clone, Default)]
pub struct ContextMetadata {
    /// Total number of blocks the composer considered
    pub total_blocks_considered: usize,

    /// Number of blocks rendered with full source
    pub blocks_included_count: usize,

    /// Number of blocks rendered as signatures or summaries
    pub blocks_summarized_count: usize,

    /// Number of blocks dropped due to token budget or filtering
    pub blocks_omitted_count: usize,

    /// The `ContextMode` used
    pub mode: ContextMode,

    /// Rough estimate of how many tokens were "saved" by summarization/omission
    pub estimated_tokens_saved: usize,
}

impl ComposedContext {
    /// Convenience method to get just the text (for backward compatibility)
    pub fn into_text(self) -> String {
        self.text
    }
}
