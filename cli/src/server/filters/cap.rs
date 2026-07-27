//! Payload caps (score-ranked truncation).
use code_graph::BlockInfo;

/// Keep the highest-scoring blocks up to `max` (Sprint 9 payload cap).
pub fn cap_blocks_by_score(mut blocks: Vec<BlockInfo>, max: usize) -> (Vec<BlockInfo>, usize) {
    if max == 0 {
        let n = blocks.len();
        blocks.clear();
        return (blocks, n);
    }
    if blocks.len() <= max {
        return (blocks, 0);
    }
    blocks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let omitted = blocks.len() - max;
    blocks.truncate(max);
    (blocks, omitted)
}

/// Reference variant for zero-copy scoped slices.
pub fn cap_block_refs<'a>(blocks: Vec<&'a BlockInfo>, max: usize) -> (Vec<&'a BlockInfo>, usize) {
    if max == 0 {
        let n = blocks.len();
        return (vec![], n);
    }
    if blocks.len() <= max {
        return (blocks, 0);
    }
    let mut sorted = blocks;
    sorted.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let omitted = sorted.len() - max;
    sorted.truncate(max);
    (sorted, omitted)
}

/// Cap ranked string payload entries (skeleton paths, directory summaries, …).
pub fn cap_string_payload(mut items: Vec<String>, max: usize) -> (Vec<String>, usize) {
    if max == 0 {
        let n = items.len();
        items.clear();
        return (items, n);
    }
    if items.len() <= max {
        return (items, 0);
    }
    let omitted = items.len() - max;
    items.truncate(max);
    (items, omitted)
}
