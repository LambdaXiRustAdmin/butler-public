//! Token-aware rendering and detail-level logic for the Composer.
//!
//! Extracted via Strangler Fig. This is the "heavy" part of composition:
//! budget management, preference calculation, hub special-casing, and
//! actual text emission at different fidelity levels.

use std::fmt::Write;

use crate::snooper::context::ContextMode;
use crate::{BlockInfo, CodeGraph, ContextOptions, Id};

use super::types::{highly_connected_threshold, is_highly_connected};

// =============================================================================
// Token-aware rendering with detail levels
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum DetailLevel {
    Full,
    Signature,
    Minimal,
    Omitted,
}

/// Renders a block at a specific detail level and returns (rendered_text, tokens_used)
pub(crate) fn render_block_at_level(block: &BlockInfo, level: DetailLevel) -> (String, usize) {
    // Demoscene: use write! for zero extra alloc churn vs repeated format! + push_str.
    // Capacity is small per block; let the formatter grow it efficiently.
    let mut s = String::new();

    match level {
        DetailLevel::Full => {
            let _ = write!(
                &mut s,
                "### {} [score: {:.1}]\nFile: {}:{}-{}\n```rust\n{}\n```\n\n",
                block.name,
                block.score,
                block.file.display(),
                block.start_line,
                block.end_line,
                block.source
            );
        }
        DetailLevel::Signature => {
            let _ = write!(
                &mut s,
                "### {} (signature)\nFile: {}:{}\n",
                block.name,
                block.file.display(),
                block.start_line
            );
            // append first ~4 lines, up to '{', directly (iterator, no collect)
            let mut first = true;
            for line in block.source.lines().take(4) {
                if !first {
                    s.push('\n');
                }
                first = false;
                if let Some(sig_part) = line.split('{').next() {
                    s.push_str(sig_part.trim());
                } else {
                    s.push_str(line.trim());
                }
            }
            s.push_str("\n```\n\n");
        }
        DetailLevel::Minimal => {
            let _ = writeln!(&mut s, "- {} ({})", block.name, block.kind);
        }
        DetailLevel::Omitted => {
            return (String::new(), 0);
        }
    }

    let tokens = crate::snooper::token_manager::count_tokens(&s);
    (s, tokens)
}

/// Smart budget-aware rendering.
/// Returns (final_text, included_ids, summarized_ids, omitted_ids, total_tokens)
///
/// We still need to produce the three Vec<Id> because they are part of the public
/// ComposedContext return type.  We pre-allocate them and only clone the small Id
/// values at the moment we decide a block's fate.  The heavy BlockInfo data is
/// never moved or cloned inside the renderer – we only ever hold &BlockInfo.
pub(crate) fn render_with_budget(
    graph: &CodeGraph,
    blocks: &[BlockInfo],
    opts: &ContextOptions,
    mode: ContextMode,
) -> (String, Vec<Id>, Vec<Id>, Vec<Id>, usize) {
    // Demoscene: write! + pre-capacity, avoid format! + push churn.
    let mut out = String::with_capacity(8192);
    let _ = write!(
        &mut out,
        "=== Butler Context ({}) ===\nBlocks considered: {} | Depth: {} | Max tokens: {}\n\n",
        mode_label(mode),
        blocks.len(),
        opts.depth,
        opts.max_tokens
    );

    // Pre-allocate the three result vectors (the "fresh Vec per call" cost is
    // unavoidable for the public metadata, but we give them good capacity and
    // only ever push the cheap Id, never a full BlockInfo).
    let mut included = Vec::with_capacity(blocks.len());
    let mut summarized = Vec::with_capacity(blocks.len());
    let mut omitted = Vec::with_capacity(blocks.len());

    // Build the omitted section in a single String instead of a Vec<String> + later
    // iteration.  This removes one level of intermediate allocation for the
    // "truncated references" path.
    let mut omitted_section = String::new();

    let mut used_tokens = crate::snooper::token_manager::count_tokens(&out);
    let max_tokens = opts.max_tokens;

    // Determine preferred detail level per block based on mode + score.
    // We keep a small vec of indices + the decided level so we can sort the
    // consideration order without moving any BlockInfo data.
    let mut block_preferences: Vec<(usize, DetailLevel)> = blocks
        .iter()
        .enumerate()
        .map(|(i, block)| {
            let preferred = preferred_detail_level(block, mode, i);
            (i, preferred)
        })
        .collect();

    // === Protect top-scoring blocks (improved budgeting) ===
    // Always try to give the highest-scoring blocks Full rendering when possible.
    // Use indices only – no Id clones here.
    let top_block_count = 2;
    let top_indices: Vec<usize> = (0..blocks.len()).take(top_block_count).collect();

    // Boost preference for top blocks
    for (idx, pref) in block_preferences.iter_mut() {
        if top_indices.contains(idx) {
            *pref = DetailLevel::Full;
        }
    }

    // Sort by preference priority (Full > Signature > Minimal), then by score.
    // All comparisons are done via indices into the original slice.
    block_preferences.sort_by(|a, b| {
        let score_a = blocks[a.0].score;
        let score_b = blocks[b.0].score;
        match (a.1, b.1) {
            (DetailLevel::Full, DetailLevel::Full) => score_b.partial_cmp(&score_a).unwrap(),
            (DetailLevel::Full, _) => std::cmp::Ordering::Less,
            (_, DetailLevel::Full) => std::cmp::Ordering::Greater,
            (DetailLevel::Signature, DetailLevel::Signature) => {
                score_b.partial_cmp(&score_a).unwrap()
            }
            (DetailLevel::Signature, _) => std::cmp::Ordering::Less,
            (_, DetailLevel::Signature) => std::cmp::Ordering::Greater,
            _ => score_b.partial_cmp(&score_a).unwrap(),
        }
    });

    let _high_degree_threshold = highly_connected_threshold(graph);

    for (original_index, _) in block_preferences {
        let block = &blocks[original_index];

        let is_hub = is_highly_connected(block, graph);

        // Try preferred level first, then downgrade.
        let mut levels_to_try: Vec<DetailLevel> =
            match preferred_detail_level(block, mode, original_index) {
                DetailLevel::Full => vec![
                    DetailLevel::Full,
                    DetailLevel::Signature,
                    DetailLevel::Minimal,
                ],
                DetailLevel::Signature => vec![DetailLevel::Signature, DetailLevel::Minimal],
                _ => vec![DetailLevel::Minimal],
            };

        // If this is a highly connected component (hub), force Signature for the hub itself
        // (the direct surgical target is handled specially in compose_surgical to stay Full)
        if is_hub && levels_to_try[0] == DetailLevel::Full {
            levels_to_try[0] = DetailLevel::Signature;
        }

        let mut rendered = false;

        for &level in &levels_to_try {
            let (mut rendered_text, cost) = render_block_at_level(block, level);

            if is_hub {
                // Label only the hub itself – use push_str to avoid an extra allocation
                // on the temporary string.
                rendered_text.push_str(" (highly connected component - top 5%)\n\n");
            }

            if used_tokens + cost <= max_tokens {
                out.push_str(&rendered_text);
                used_tokens += cost;

                match level {
                    DetailLevel::Full => included.push(block.id.clone()),
                    DetailLevel::Signature | DetailLevel::Minimal => {
                        summarized.push(block.id.clone())
                    }
                    DetailLevel::Omitted => {}
                }

                rendered = true;
                break;
            }
        }

        if !rendered {
            omitted.push(block.id.clone());

            // write! directly to string buffer (no format! temp).
            let _ = writeln!(
                &mut omitted_section,
                "- {}:{} ({})",
                block.file.display(),
                block.start_line,
                block.name
            );
        }
    }

    if !omitted_section.is_empty() {
        let _ = write!(
            &mut out,
            "\n\n=== ADDITIONAL REFERENCES (Omitted due to max_tokens limit) ===\n{}",
            omitted_section
        );
    }

    (out, included, summarized, omitted, used_tokens)
}

pub(crate) fn preferred_detail_level(
    block: &BlockInfo,
    mode: ContextMode,
    index: usize,
) -> DetailLevel {
    match mode {
        ContextMode::Surgical => {
            if index < 3 || block.score > 80.0 {
                DetailLevel::Full
            } else {
                DetailLevel::Minimal
            }
        }
        ContextMode::Implementation => {
            if index < 7 || block.score > 70.0 {
                DetailLevel::Full
            } else {
                DetailLevel::Signature
            }
        }
        ContextMode::Architecture => {
            let is_type = matches!(
                block.kind.as_str(),
                "struct_item" | "enum_item" | "trait_item" | "type_item"
            );
            if is_type || block.source.contains("pub ") {
                DetailLevel::Signature
            } else {
                DetailLevel::Minimal
            }
        }
        ContextMode::Compressed => DetailLevel::Minimal,
        ContextMode::Mini => DetailLevel::Signature, // ultra compact: name + signature
        ContextMode::Balanced => {
            if index < 4 || block.score > 85.0 {
                DetailLevel::Full
            } else if index < 8 {
                DetailLevel::Signature
            } else {
                DetailLevel::Minimal
            }
        }
    }
}

pub(crate) fn mode_label(mode: ContextMode) -> &'static str {
    match mode {
        ContextMode::Balanced => "Balanced",
        ContextMode::Surgical => "Surgical",
        ContextMode::Implementation => "Implementation",
        ContextMode::Architecture => "Architecture",
        ContextMode::Compressed => "Compressed",
        ContextMode::Mini => "Mini",
    }
}
