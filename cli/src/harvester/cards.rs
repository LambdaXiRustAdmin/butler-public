//! Local neighborhood cards for gold labeling.
//!
//! Butler builds the full Tree-sitter graph; the LLM only ever sees a small
//! "flashcard": center node + 1-hop structure + real source + query.
//! Labels (critical / hard-negative) join back via Butler node ids.

use code_graph::snooper::model::{BlockInfo, CodeGraph, Id};
use serde::Serialize;
use std::collections::HashSet;

fn id_key<'a>(g: &'a CodeGraph, center_id: &str) -> Option<&'a Id> {
    g.nodes.keys().find(|k| k.as_str() == center_id)
}

/// Default max neighbors (callers + callees + parent/children pooled).
pub const MAX_NEIGHBORS: usize = 12;
/// Default max source characters on the center snippet.
pub const MAX_SNIPPET_CHARS: usize = 1200;

/// How much graph+source to put on each card (token budget for the labeler).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct CardBudget {
    pub max_neighbors: usize,
    pub max_snippet_chars: usize,
    /// Callers/callees/children listed per side before pooling cap.
    pub per_relation_cap: usize,
}

impl Default for CardBudget {
    fn default() -> Self {
        Self {
            max_neighbors: MAX_NEIGHBORS,
            max_snippet_chars: MAX_SNIPPET_CHARS,
            per_relation_cap: 4,
        }
    }
}

impl CardBudget {
    /// Fast remote models (Grok API / agent session): richer cards, fewer blind peeks.
    pub fn fast() -> Self {
        Self {
            max_neighbors: 16,
            max_snippet_chars: 2000,
            per_relation_cap: 5,
        }
    }

    /// Slow local models (CPU Qwen overnight): smaller prompts, more batches.
    pub fn slow() -> Self {
        Self {
            max_neighbors: 6,
            max_snippet_chars: 500,
            per_relation_cap: 2,
        }
    }

    pub fn from_profile(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "slow" | "local" | "small" | "qwen" => Self::slow(),
            "fast" | "remote" | "large" | "grok" | "agent" => Self::fast(),
            _ => Self::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NeighborRef {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// caller | callee | parent | child
    pub relation: String,
}

/// One labeling unit handed to the frontier model.
#[derive(Debug, Clone, Serialize)]
pub struct NeighborhoodCard {
    pub query: String,
    pub center_id: String,
    pub center_name: String,
    pub file: String,
    pub range: String,
    pub kind: String,
    pub lang: String,
    pub in_degree: usize,
    pub out_degree: usize,
    pub is_highly_connected: bool,
    pub snippet: String,
    pub neighbors: Vec<NeighborRef>,
    /// How Butler chose this center: random_walk | expand_critical | query_seed
    pub seed_reason: String,
}

fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    format!("{}...", &s[..i])
}

fn block_by_id<'a>(g: &'a CodeGraph, id: &str) -> Option<&'a BlockInfo> {
    g.nodes.values().find(|b| b.id.as_str() == id)
}

/// Build a 1-hop neighborhood card around `center_id`.
pub fn build_card(
    g: &CodeGraph,
    center_id: &str,
    query: &str,
    seed_reason: &str,
) -> Option<NeighborhoodCard> {
    build_card_with_budget(g, center_id, query, seed_reason, CardBudget::default())
}

/// Build card with an explicit token budget (fast vs slow labelers).
pub fn build_card_with_budget(
    g: &CodeGraph,
    center_id: &str,
    query: &str,
    seed_reason: &str,
    budget: CardBudget,
) -> Option<NeighborhoodCard> {
    let b = block_by_id(g, center_id)?;
    let key = id_key(g, center_id)?;

    let out_degree = g.edges.get(key).map(|v| v.len()).unwrap_or(0);
    let in_degree = g.reverse.get(key).map(|v| v.len()).unwrap_or(0);

    let mut neighbors: Vec<NeighborRef> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(center_id.to_string());
    let max_n = budget.max_neighbors.max(1);
    let per = budget.per_relation_cap.max(1);

    let push_rel = |neighbors: &mut Vec<NeighborRef>,
                    seen: &mut HashSet<String>,
                    nid: &Id,
                    relation: &str| {
        if neighbors.len() >= max_n {
            return;
        }
        let s = nid.as_str().to_string();
        if !seen.insert(s.clone()) {
            return;
        }
        if let Some(nb) = g.nodes.get(nid) {
            neighbors.push(NeighborRef {
                id: s,
                name: nb.name.clone(),
                kind: nb.kind.clone(),
                relation: relation.to_string(),
            });
        }
    };

    if let Some(callers) = g.reverse.get(key) {
        for c in callers.iter().take(per) {
            push_rel(&mut neighbors, &mut seen, c, "caller");
        }
    }
    if let Some(callees) = g.edges.get(key) {
        for c in callees.iter().take(per) {
            push_rel(&mut neighbors, &mut seen, c, "callee");
        }
    }
    if let Some(parent) = &b.parent_id {
        push_rel(&mut neighbors, &mut seen, parent, "parent");
    }
    for child in b.children.iter().take(per) {
        push_rel(&mut neighbors, &mut seen, child, "child");
    }

    Some(NeighborhoodCard {
        query: query.to_string(),
        center_id: center_id.to_string(),
        center_name: b.name.clone(),
        file: b.file.to_string_lossy().into_owned(),
        range: format!("{}-{}", b.start_line, b.end_line),
        kind: b.kind.clone(),
        lang: b.lang.clone(),
        in_degree,
        out_degree,
        is_highly_connected: b.is_highly_connected,
        snippet: truncate_utf8(&b.source, budget.max_snippet_chars.max(1)),
        neighbors,
        seed_reason: seed_reason.to_string(),
    })
}

/// Compact multi-card prompt block (JSON-ish, stable for the model).
pub fn format_cards_for_prompt(cards: &[NeighborhoodCard]) -> String {
    match serde_json::to_string_pretty(cards) {
        Ok(s) => s,
        Err(_) => format!("{cards:?}"),
    }
}

/// Stub-note patterns that must not pass as gold exploration notes.
pub fn is_stub_note(note: &str) -> bool {
    let n = note.trim().to_lowercase();
    if n.len() < 12 {
        return true;
    }
    const BANNED: &[&str] = &[
        "selected from",
        "unfiltered codegraph",
        "from codegraph",
        "high connectivity",
        "high degree",
        "codegraph",
        "n/a",
        "todo",
        "placeholder",
    ];
    BANNED.iter().any(|b| n.contains(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph::snooper::model::{BlockInfo, CodeGraph, Id};
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn mk_block(file: &str, name: &str, hash8: &str) -> (Id, BlockInfo) {
        let id = Id::new(file, "function_item", &format!("{hash8}xxxxxxxx"));
        let b = BlockInfo {
            id: id.clone(),
            name: name.into(),
            file: PathBuf::from(file),
            kind: "function_item".into(),
            lang: "rust".into(),
            start_line: 1,
            end_line: 10,
            start_byte: 0,
            end_byte: 40,
            parent_id: None,
            children: vec![],
            content_hash: format!("{hash8}xxxxxxxx"),
            sig_hash: "s".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: format!("fn {name}() {{ /* body for tests */ }}"),
            score: 1.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        };
        (id, b)
    }

    #[test]
    fn build_card_includes_neighbors_and_snippet() {
        let mut g = CodeGraph::new();
        let (a, ba) = mk_block("a.rs", "alpha", "aaaaaaaa");
        let (b, bb) = mk_block("b.rs", "beta", "bbbbbbbb");
        g.nodes.insert(a.clone(), ba);
        g.nodes.insert(b.clone(), bb);
        g.edges.insert(a.clone(), vec![b.clone()]);
        g.reverse.insert(b.clone(), vec![a.clone()]);

        let card = build_card(&g, a.as_str(), "find core path", "random_walk").expect("card");
        assert_eq!(card.center_name, "alpha");
        assert!(card.snippet.contains("fn alpha"));
        assert_eq!(card.out_degree, 1);
        assert!(card.neighbors.iter().any(|n| n.relation == "callee"));

        let tiny = build_card_with_budget(
            &g,
            a.as_str(),
            "q",
            "random_walk",
            CardBudget {
                max_neighbors: 1,
                max_snippet_chars: 20,
                per_relation_cap: 1,
            },
        )
        .expect("tiny");
        assert!(tiny.neighbors.len() <= 1);
        assert!(tiny.snippet.len() <= 24); // 20 + "..."
    }

    #[test]
    fn stub_notes_detected() {
        assert!(is_stub_note("selected from CodeGraph"));
        assert!(is_stub_note("high connectivity hub"));
        assert!(is_stub_note("short"));
        assert!(!is_stub_note(
            "implements argument extraction used by every #[pyfunction] path"
        ));
    }
}
