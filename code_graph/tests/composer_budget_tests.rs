//! Integration tests for token-aware budgeting in the composer.
//!
//! These tests verify that `compose_context` properly respects `max_tokens`
//! and correctly populates `blocks_omitted`.

use code_graph::snooper::context::ContextMode;
use code_graph::{compose_context, BlockInfo, CodeGraph, ContextOptions, Id};
use std::collections::HashSet;
use std::path::PathBuf;

fn make_block(name: &str, kind: &str, source: &str, score: f64) -> BlockInfo {
    let hash = format!("{:0<16}", name);
    BlockInfo {
        id: Id::new("test.rs", kind, &hash),
        name: name.to_string(),
        file: PathBuf::from("test.rs"),
        kind: kind.to_string(),
        lang: "rust".to_string(),
        start_line: 10,
        end_line: 30,
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

fn default_opts(mode: ContextMode, max_tokens: usize) -> ContextOptions {
    ContextOptions {
        depth: 2,
        max_tokens,
        compress_tests: false,
        format: code_graph::snooper::context::OutputFormat::Markdown,
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
fn test_tight_budget_populates_omitted_blocks() {
    let blocks: Vec<_> = (0..15)
        .map(|i| {
            let source = format!("fn fn_{}() {{ /* some code here */ }}", i);
            make_block(
                &format!("fn_{}", i),
                "function_item",
                &source,
                100.0 - i as f64 * 5.0,
            )
        })
        .collect();

    // Very tight budget — should force many blocks to be omitted
    let opts = default_opts(ContextMode::Balanced, 400);

    let result = compose_context(&CodeGraph::new(), blocks, &opts, "test");

    assert!(
        !result.blocks_omitted.is_empty(),
        "Expected some blocks to be omitted under tight token budget"
    );
    assert!(
        result.token_count <= 450, // allow small overhead
        "Output should respect max_tokens budget (got {} tokens)",
        result.token_count
    );
}

#[test]
fn test_higher_score_blocks_are_preferred_under_budget() {
    let mut blocks = vec![
        make_block(
            "low_score",
            "function_item",
            "fn low_score() { /* long body */ }",
            10.0,
        ),
        make_block(
            "high_score",
            "function_item",
            "fn high_score() { /* body */ }",
            95.0,
        ),
    ];

    // Make the low_score block have a much longer source so it costs more tokens
    blocks[0].source = "fn low_score() { ".to_string() + &"x".repeat(800) + " }";

    let opts = default_opts(ContextMode::Balanced, 300);

    let result = compose_context(&CodeGraph::new(), blocks, &opts, "test");

    // The high-score block should be included, while the low-score one may be omitted or heavily summarized
    let high_included = result.blocks_included.iter().any(|_id| {
        // We can't easily compare Ids here without constructing them, so we check the text
        result.text.contains("high_score")
    });

    assert!(
        high_included || result.text.contains("high_score"),
        "High-scoring block should be preferred under tight budget"
    );
}

#[test]
fn test_surgical_mode_still_respects_budget() {
    let blocks: Vec<_> = (0..12)
        .map(|i| {
            make_block(
                &format!("target_{}", i),
                "function_item",
                &format!("fn target_{}() {{ body }}", i),
                90.0 - i as f64,
            )
        })
        .collect();

    let opts = default_opts(ContextMode::Surgical, 250);

    let result = compose_context(&CodeGraph::new(), blocks, &opts, "test");

    // Even in Surgical mode, we must respect the token limit
    assert!(result.token_count <= 300);
}
