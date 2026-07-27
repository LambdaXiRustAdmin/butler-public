//! Context Composer
//!
//! Responsible for turning a list of scored blocks into high-signal,
//! mode-specific output tailored for LLMs.
//!
//! Split via Strangler Fig pattern:
//! - types.rs: core result types + pure graph helpers (hubs, neighbors)
//! - renderer.rs: the heavy token-budget rendering engine (detail levels, render_with_budget)
//! - This mod.rs (facade): mode dispatch + the compose_* orchestration functions.

pub mod renderer;
pub mod types;

// Re-export everything so external callers (snooper reexports, context, server, tests)
// continue to see the exact same API as before the split.
use renderer::*;
pub(crate) use types::highly_connected_threshold;
pub use types::{ComposedContext, ContextMetadata};

use crate::snooper::context::ContextMode;
use crate::{BlockInfo, CodeGraph, ContextOptions};

/// Main entry point for rich context composition.
/// Returns a `ComposedContext` containing both the final text and metadata.
/// Signature kept identical for public API stability (the zero-copy work happens
/// on the private compose_* helpers and inside the renderer).
pub fn compose_context(
    graph: &CodeGraph,
    mut blocks: Vec<BlockInfo>,
    opts: &ContextOptions,
    prompt: &str,
) -> ComposedContext {
    // Drop blocks with score 0.0, *except* in Surgical mode.
    //
    // In Surgical mode the caller (especially the HTTP/MCP server) can pass raw
    // blocks directly from the graph (score == 0.0) when using target_file + target_line.
    // We must never silently drop the exact line the user asked for.
    if opts.mode != ContextMode::Surgical {
        blocks.retain(|b| b.score > 0.0);
    }

    // Slim cache / progressive scan: sources never live in the warehouse — load span from disk.
    if let Some(ref root) = opts.project_root {
        for b in blocks.iter_mut() {
            if b.source.is_empty() {
                let _ = b.hydrate_source_from_disk(root);
            }
        }
    }

    // Sort by score descending (higher relevance first)
    blocks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // After the (necessary) retain+sort on the owned vec we pass a slice to the
    // private orchestration functions. No further moves of the heavy BlockInfo
    // data into the composer.
    let blocks_ref: &[BlockInfo] = &blocks;

    match opts.mode {
        ContextMode::Surgical => compose_surgical(graph, blocks_ref, opts, prompt),
        ContextMode::Implementation => compose_implementation(graph, blocks_ref, opts, prompt),
        ContextMode::Architecture => compose_architecture(graph, blocks_ref, opts, prompt),
        ContextMode::Compressed => compose_compressed(graph, blocks_ref, opts, prompt),
        ContextMode::Balanced => compose_balanced(graph, blocks_ref, opts, prompt),
        ContextMode::Mini => compose_mini(graph, blocks_ref, opts, prompt),
    }
}

/// Convenience function that returns only the formatted text.
/// Useful during the transition period while some callers still expect a String.
/// Signature unchanged.
pub fn compose(
    graph: &CodeGraph,
    blocks: Vec<BlockInfo>,
    opts: &ContextOptions,
    prompt: &str,
) -> String {
    compose_context(graph, blocks, opts, prompt).into_text()
}

// =============================================================================
// Per-mode composers (orchestration layer)
// These decide *what* to include and call into the renderer for *how* to format it.
// They now take slices (zero-copy from the caller's perspective after the
// retain/sort in the public entry point).
// =============================================================================

fn compose_balanced(
    graph: &CodeGraph,
    blocks: &[BlockInfo],
    opts: &ContextOptions,
    _prompt: &str,
) -> ComposedContext {
    let (text, included, summarized, omitted, token_count) =
        render_with_budget(graph, blocks, opts, ContextMode::Balanced);

    let metadata = ContextMetadata {
        total_blocks_considered: blocks.len(),
        blocks_included_count: included.len(),
        blocks_summarized_count: summarized.len(),
        blocks_omitted_count: omitted.len(),
        mode: ContextMode::Balanced,
        estimated_tokens_saved: 0,
    };

    ComposedContext {
        text,
        token_count,
        blocks_included: included,
        blocks_summarized: summarized,
        blocks_omitted: omitted,
        mode: ContextMode::Balanced,
        metadata,
    }
}

fn compose_surgical(
    graph: &CodeGraph,
    blocks: &[BlockInfo],
    opts: &ContextOptions,
    _prompt: &str,
) -> ComposedContext {
    // For surgical, we want the primary target(s) in full detail.
    // The render_with_budget will try to keep top blocks Full.
    let (mut text, included, summarized, omitted, token_count) =
        render_with_budget(graph, blocks, opts, ContextMode::Surgical);

    // Add a small "direct graph neighbors" section for the primary block(s) if we have graph edges.
    // This is the special surgical affordance.
    if let Some(first) = blocks.first() {
        let callers = graph.callers(&first.id);
        let children = graph.children(&first.id);

        if !callers.is_empty() || !children.is_empty() {
            text.push_str("\n\n### Direct Graph Neighbors\n");
            for cid in callers.iter().take(5) {
                // get_block takes Id by value, so we clone the cheap Id here
                // (the heavy BlockInfo data is never cloned in the renderer path).
                if let Some(b) = graph.get_block(cid.clone()) {
                    let (sig, _) = render_block_at_level(b, DetailLevel::Signature);
                    text.push_str(&sig);
                }
            }
            for cid in children.iter().take(5) {
                if let Some(b) = graph.get_block(cid.clone()) {
                    let (sig, _) = render_block_at_level(b, DetailLevel::Signature);
                    text.push_str(&sig);
                }
            }
        }
    }

    let metadata = ContextMetadata {
        total_blocks_considered: blocks.len(),
        blocks_included_count: included.len(),
        blocks_summarized_count: summarized.len(),
        blocks_omitted_count: omitted.len(),
        mode: ContextMode::Surgical,
        estimated_tokens_saved: 0,
    };

    ComposedContext {
        text,
        token_count,
        blocks_included: included,
        blocks_summarized: summarized,
        blocks_omitted: omitted,
        mode: ContextMode::Surgical,
        metadata,
    }
}

fn compose_mini(
    graph: &CodeGraph,
    blocks: &[BlockInfo],
    opts: &ContextOptions,
    _prompt: &str,
) -> ComposedContext {
    let (text, included, summarized, omitted, token_count) =
        render_with_budget(graph, blocks, opts, ContextMode::Mini);

    let metadata = ContextMetadata {
        total_blocks_considered: blocks.len(),
        blocks_included_count: included.len(),
        blocks_summarized_count: summarized.len(),
        blocks_omitted_count: omitted.len(),
        mode: ContextMode::Mini,
        estimated_tokens_saved: 0,
    };

    ComposedContext {
        text,
        token_count,
        blocks_included: included,
        blocks_summarized: summarized,
        blocks_omitted: omitted,
        mode: ContextMode::Mini,
        metadata,
    }
}

fn compose_implementation(
    graph: &CodeGraph,
    blocks: &[BlockInfo],
    opts: &ContextOptions,
    _prompt: &str,
) -> ComposedContext {
    let (text, included, summarized, omitted, token_count) =
        render_with_budget(graph, blocks, opts, ContextMode::Implementation);

    let metadata = ContextMetadata {
        total_blocks_considered: blocks.len(),
        blocks_included_count: included.len(),
        blocks_summarized_count: summarized.len(),
        blocks_omitted_count: omitted.len(),
        mode: ContextMode::Implementation,
        estimated_tokens_saved: 0,
    };

    ComposedContext {
        text,
        token_count,
        blocks_included: included,
        blocks_summarized: summarized,
        blocks_omitted: omitted,
        mode: ContextMode::Implementation,
        metadata,
    }
}

fn compose_architecture(
    graph: &CodeGraph,
    blocks: &[BlockInfo],
    opts: &ContextOptions,
    _prompt: &str,
) -> ComposedContext {
    let (text, included, summarized, omitted, token_count) =
        render_with_budget(graph, blocks, opts, ContextMode::Architecture);

    let estimated_tokens_saved = (summarized.len() + omitted.len()) * 180;

    let metadata = ContextMetadata {
        total_blocks_considered: blocks.len(),
        blocks_included_count: included.len(),
        blocks_summarized_count: summarized.len(),
        blocks_omitted_count: omitted.len(),
        mode: ContextMode::Architecture,
        estimated_tokens_saved,
    };

    ComposedContext {
        text,
        token_count,
        blocks_included: included,
        blocks_summarized: summarized,
        blocks_omitted: omitted,
        mode: ContextMode::Architecture,
        metadata,
    }
}

fn compose_compressed(
    graph: &CodeGraph,
    blocks: &[BlockInfo],
    opts: &ContextOptions,
    _prompt: &str,
) -> ComposedContext {
    let (text, included, summarized, omitted, token_count) =
        render_with_budget(graph, blocks, opts, ContextMode::Compressed);

    let estimated_tokens_saved = (summarized.len() + omitted.len()) * 180;

    let metadata = ContextMetadata {
        total_blocks_considered: blocks.len(),
        blocks_included_count: included.len(),
        blocks_summarized_count: summarized.len(),
        blocks_omitted_count: omitted.len(),
        mode: ContextMode::Compressed,
        estimated_tokens_saved,
    };

    ComposedContext {
        text,
        token_count,
        blocks_included: included,
        blocks_summarized: summarized,
        blocks_omitted: omitted,
        mode: ContextMode::Compressed,
        metadata,
    }
}

// =============================================================================
// Tests (kept here in the facade with the mode logic they exercise)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockInfo;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn make_block(name: &str, kind: &str, source: &str, score: f64) -> BlockInfo {
        let hash = format!("{:0<16}", name);
        BlockInfo {
            id: crate::Id::new("test.rs", kind, &hash),
            name: name.to_string(),
            file: PathBuf::from("test.rs"),
            kind: kind.to_string(),
            lang: "rust".to_string(),
            start_line: 10,
            end_line: 20,
            start_byte: 0,
            end_byte: source.len(),
            parent_id: None,
            children: vec![],
            content_hash: hash.clone(),
            sig_hash: "sig".to_string(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: source.to_string(),
            score,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        }
    }

    fn make_graph_with_neighbors() -> (CodeGraph, Vec<BlockInfo>) {
        let mut graph = CodeGraph::new();

        let target = make_block("target_fn", "function_item", "fn target_fn() {}", 100.0);
        let caller = make_block(
            "caller",
            "function_item",
            "fn caller() { target_fn(); }",
            80.0,
        );
        let child = make_block("helper", "function_item", "fn helper() {}", 70.0);

        graph.add_block(target.clone());
        graph.add_block(caller.clone());
        graph.add_block(child.clone());

        // target is called by caller
        graph.add_edge(caller.id.clone(), target.id.clone());
        // target calls child
        graph.add_edge(target.id.clone(), child.id.clone());

        (graph, vec![target, caller, child])
    }

    fn default_opts(mode: ContextMode) -> ContextOptions {
        ContextOptions {
            depth: 2,
            max_tokens: 8000,
            compress_tests: false,
            format: crate::snooper::context::OutputFormat::Markdown,
            mode,
            target_file: None,
            target_line: None,
            importance_threshold: 0.0,
            scope_paths: None,
            ignore_paths: None,
            use_neural_scores: false,
            project_root: None,
        }
    }

    #[test]
    fn test_compressed_mode_produces_minimal_output() {
        let blocks = vec![
            make_block(
                "big_fn",
                "function_item",
                "fn big_fn() { /* lots of code */ }",
                90.0,
            ),
            make_block("other", "function_item", "fn other() {}", 50.0),
        ];
        let opts = default_opts(ContextMode::Compressed);

        let output = compose(&CodeGraph::new(), blocks, &opts, "test");

        // In compressed mode, we should see minimal lines, not full code blocks
        assert!(output.contains("- big_fn"));
        assert!(!output.contains("```rust"));
    }

    #[test]
    fn test_architecture_mode_prefers_signatures() {
        let blocks = vec![
            make_block(
                "MyStruct",
                "struct_item",
                "pub struct MyStruct { x: i32 }",
                95.0,
            ),
            make_block(
                "impl_block",
                "impl_item",
                "impl MyStruct { fn do_stuff(&self) { /* body */ } }",
                80.0,
            ),
        ];
        let opts = default_opts(ContextMode::Architecture);

        let output = compose(&CodeGraph::new(), blocks, &opts, "test");

        // Should prefer signature style for types
        assert!(output.contains("(signature)"));
        assert!(output.contains("MyStruct"));
    }

    #[test]
    fn test_surgical_mode_shows_neighbors_section() {
        let (graph, blocks) = make_graph_with_neighbors();
        let opts = default_opts(ContextMode::Surgical);

        let output = compose(&graph, blocks, &opts, "test");

        assert!(output.contains("Direct Graph Neighbors"));
    }

    /// Regression test: surgical mode must work even when the incoming blocks
    /// have score == 0.0 (the normal situation when the MCP/HTTP server passes
    /// raw nodes from the graph for a target_file + target_line request).
    #[test]
    fn test_surgical_mode_works_with_zero_score_blocks() {
        let mut graph = CodeGraph::new();

        // Simulate a real graph node (score starts at 0.0)
        let mut target = make_block(
            "my_target",
            "function_item",
            "fn my_target() { /* body */ }",
            0.0,
        );
        target.file = PathBuf::from("code_graph/src/snooper/composer.rs");
        target.start_line = 320;
        target.end_line = 330;

        graph.add_block(target.clone());

        let mut opts = default_opts(ContextMode::Surgical);
        opts.target_file = Some(PathBuf::from("code_graph/src/snooper/composer.rs"));
        opts.target_line = Some(323);

        let output = compose(&graph, vec![target], &opts, "");

        // Must not be empty and must mention the surgical target
        assert!(output.contains("Surgical Result") || output.contains("my_target"));
        assert!(!output.trim().is_empty());
    }

    #[test]
    fn test_implementation_mode_gives_more_full_code() {
        let blocks: Vec<_> = (0..10)
            .map(|i| {
                make_block(
                    &format!("fn_{}", i),
                    "function_item",
                    &format!("fn fn_{}() {{ body_{} }}", i, i),
                    90.0 - i as f64,
                )
            })
            .collect();

        let opts_impl = default_opts(ContextMode::Implementation);
        let opts_comp = default_opts(ContextMode::Compressed);

        let out_impl = compose(&CodeGraph::new(), blocks.clone(), &opts_impl, "test");
        let out_comp = compose(&CodeGraph::new(), blocks, &opts_comp, "test");

        // Implementation mode should contain more full code blocks than compressed
        let full_code_count_impl = out_impl.matches("```rust").count();
        let full_code_count_comp = out_comp.matches("```rust").count();

        assert!(full_code_count_impl > full_code_count_comp);
    }

    #[test]
    fn test_balanced_mode_has_mixed_output() {
        // Create enough blocks to trigger all three tiers in Balanced mode
        let blocks: Vec<_> = (0..12)
            .map(|i| {
                make_block(
                    &format!("item_{}", i),
                    "function_item",
                    &format!("fn item_{}() {{ /* code */ }}", i),
                    120.0 - i as f64 * 8.0,
                )
            })
            .collect();

        let opts = default_opts(ContextMode::Balanced);
        let output = compose(&CodeGraph::new(), blocks, &opts, "test");

        // Balanced mode should contain full source, signatures, and minimal entries
        assert!(output.contains("```rust")); // full code
        assert!(output.contains("(signature)")); // signature tier
        assert!(output.contains("- item_")); // minimal tier
    }
}
