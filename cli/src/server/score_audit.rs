//! Temporary score-funnel diagnostics: heuristic vs neural side-by-side.

use crate::vprintln;
use std::collections::HashSet;

use code_graph::{
    rank_blocks_for_selection, rank_blocks_for_selection_subset, BlockInfo, CodeGraph,
    NeuralSelectionBlend,
};

/// Log top-N candidates with raw heuristic, neural, text-match, and blended scores.
pub fn log_score_funnel_audit(
    graph: &CodeGraph,
    prompt: &str,
    selected: &[&BlockInfo],
    use_neural: bool,
    blend: NeuralSelectionBlend,
    top_n: usize,
) {
    // Zero-copy orchestrate passes the entire scoped set (can be 10k–300k). Never
    // re-rank the full graph for audit — that is pure single-core heat with no substance gain.
    const AUDIT_RANK_CAP: usize = 512;
    let ranked = if selected.is_empty() {
        Vec::new()
    } else if selected.len() > AUDIT_RANK_CAP
        || selected.len() >= graph.nodes.len().saturating_mul(3) / 4
    {
        // Sample highest structural scores already on blocks for a cheap top table.
        let mut sample: Vec<&BlockInfo> = selected.iter().copied().collect();
        sample.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sample.truncate(AUDIT_RANK_CAP);
        rank_blocks_for_selection_subset(graph, &sample, prompt, use_neural, blend)
    } else if selected.len() < graph.nodes.len() / 2 {
        rank_blocks_for_selection_subset(graph, selected, prompt, use_neural, blend)
    } else {
        rank_blocks_for_selection(graph, prompt, use_neural, blend)
    };
    if ranked.is_empty() {
        vprintln!("📊 [score_audit] no candidates for prompt={prompt:?}");
        return;
    }

    let selected_ids: HashSet<_> = selected.iter().map(|b| b.id.as_str()).collect();

    vprintln!(
        "📊 [score_audit] mode={} blend=text:{:.2}+neural:{:.2} prompt={:?}",
        if use_neural { "neural" } else { "heuristic" },
        blend.text_weight,
        blend.neural_weight,
        prompt
    );
    vprintln!(
        "    {:>4} | {:<28} | {:>9} | {:>9} | {:>9} | {:>8} | sel",
        "rank", "name", "text", "heur", "neural", "blend"
    );

    for (i, row) in ranked.iter().take(top_n).enumerate() {
        let mark = if selected_ids.contains(row.id.as_str()) {
            "YES"
        } else {
            ""
        };
        vprintln!(
            "    {:>4} | {:<28} | {:>9.3} | {:>9.3} | {:>9.4} | {:>8.4} | {mark}",
            i + 1,
            truncate_name(&row.name, 28),
            row.text_match,
            row.heuristic,
            row.neural,
            row.blended,
        );
    }

    const FINAL_SELECTED_LOG_CAP: usize = 10;
    // Never O(n log n) sort 80k–200k scoped nodes just for a log line.
    const FINAL_SORT_CAP: usize = 256;
    let top_selected: Vec<_> = if selected.len() > FINAL_SORT_CAP {
        let mut sample: Vec<_> = selected.iter().take(FINAL_SORT_CAP).copied().collect();
        sample.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sample
    } else {
        let mut v: Vec<_> = selected.iter().copied().collect();
        v.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    };
    let shown: Vec<String> = top_selected
        .iter()
        .take(FINAL_SELECTED_LOG_CAP)
        .map(|b| format!("{}({:.4})", b.name, b.score))
        .collect();
    let omitted = selected.len().saturating_sub(shown.len());
    let suffix = if omitted > 0 {
        format!(", ... +{omitted} more")
    } else {
        String::new()
    };
    vprintln!(
        "📊 [score_audit] final_selected ({}): {}{}",
        selected.len(),
        shown.join(", "),
        suffix
    );
}

fn truncate_name(name: &str, max: usize) -> String {
    if name.len() <= max {
        name.to_string()
    } else {
        format!("{}…", &name[..max.saturating_sub(1)])
    }
}
