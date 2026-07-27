//! Structure-first query tokens — no natural-language dictionaries.
//!
//! Free text (English, Polish, …) is not “understood”. We extract identifier- and
//! path-shaped pieces, then **keep only tokens that hit this graph’s names/paths**.
//! Membership is the only lexicon; it is always current for the repo.
//!
//! Intent modes stay on the MCP schema (`goal`, `target_symbol`, tool name), not here.

use std::collections::HashSet;

use super::model::{BlockInfo, CodeGraph};

/// True for symbols that must never win selection (single-letter locals, parser shells).
pub fn is_junk_symbol_name(name: &str) -> bool {
    let n = name.trim();
    if n.is_empty() {
        return true;
    }
    let alnum_len = n
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .count();
    if alnum_len < 3 {
        return true;
    }
    matches!(
        n,
        "test_default" | "unknown" | "mod" | "impl" | "self" | "Self" | "crate" | "super"
    )
}

/// Tokens resolved from a user prompt for structural retrieval / ranking.
#[derive(Debug, Clone, Default)]
pub struct StructuralQuery {
    /// Tokens that appear in this graph (name or path). Primary seed material.
    pub graph_hits: Vec<String>,
    /// Strong code-shaped tokens even if not yet in graph (typed symbol, wrong root).
    pub strong_unmatched: Vec<String>,
    /// Tokens with an **exact** `name_index` / block name hit (always usable seeds).
    /// Covers short PascalCase hubs (`Group`, `Typer`) even if shape heuristics lag.
    pub exact_name_hits: Vec<String>,
    /// Original prompt lowercased (for exact whole-prompt match).
    pub prompt_lower: String,
}

impl StructuralQuery {
    /// Tokens used for ranking / L0. Prefer **strong** graph hits; also keep
    /// **exact name_index** hits (short PascalCase hubs like Group/Typer).
    /// Weak prose fragments that only substring-hit a name are dropped.
    pub fn ranking_tokens(&self) -> Vec<&str> {
        let strong: Vec<&str> = self
            .graph_hits
            .iter()
            .filter(|t| is_strong_query_token(t))
            .map(|s| s.as_str())
            .collect();
        if !strong.is_empty() {
            return strong;
        }
        // Exact index membership even when shape gate was historically weak.
        if !self.exact_name_hits.is_empty() {
            return self.exact_name_hits.iter().map(|s| s.as_str()).collect();
        }
        // No strong membership → fail-closed for ranking (do not use unmatched fakes as seeds)
        Vec::new()
    }

    pub fn is_empty(&self) -> bool {
        self.graph_hits.is_empty()
            && self.strong_unmatched.is_empty()
            && self.exact_name_hits.is_empty()
    }

    /// True when we can seed structure: strong graph-hit **or** exact name_index hit.
    /// Strong-unmatched-only (typo / wrong repo) is **not** usable — fail closed.
    pub fn has_usable_hits(&self) -> bool {
        self.graph_hits.iter().any(|t| is_strong_query_token(t))
            || !self.exact_name_hits.is_empty()
    }
}

/// Tokens that are allowed to drive retrieval (not accidental prose leaks).
pub fn is_strong_query_token(t: &str) -> bool {
    let t = t.trim();
    if t.len() < 3 || is_junk_symbol_name(t) {
        return false;
    }
    if is_code_shaped(t) {
        return true;
    }
    // Long bare identifiers (PascalCase already code_shaped; long lowercase ok if exact-grade)
    t.len() >= 6
}

/// Pure PascalCase type/hub name: `Group`, `Typer`, `App` (initial Upper, rest lower/digit).
///
/// Distinct from camelCase (`initWindow`) which needs an internal capital after a lower.
/// Without this, short hubs fail `is_strong_query_token` and fail-closed as prose.
pub fn is_pure_pascal_case(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 3 {
        return false;
    }
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_uppercase() {
        return false;
    }
    chars.all(|c| c.is_lowercase() || c.is_ascii_digit())
}

/// True if `s` looks like an identifier or path fragment (shape only — no word lists).
pub fn is_code_shaped(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 2 {
        return false;
    }
    if s.contains('/') || s.contains('\\') || s.contains("::") || s.contains('.') {
        return true;
    }
    if s.contains('_') {
        return true;
    }
    // Pure PascalCase hubs (Group, Typer) — before camelCase scan.
    if is_pure_pascal_case(s) {
        return true;
    }
    // camelCase / multi-hump PascalCase (HttpServer, cpuGnnForward)
    let mut saw_lower = false;
    for c in s.chars() {
        if c.is_lowercase() {
            saw_lower = true;
        } else if c.is_uppercase() && saw_lower {
            return true;
        }
    }
    if s.len() >= 4 && s.chars().all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_') {
        return true;
    }
    let lower = s.to_ascii_lowercase();
    for ext in [".rs", ".go", ".py", ".ts", ".tsx", ".js", ".cpp", ".c", ".h", ".hpp"] {
        if lower.ends_with(ext) {
            return true;
        }
    }
    false
}

fn push_token(out: &mut Vec<String>, seen: &mut HashSet<String>, s: &str) {
    let t = s
        .trim()
        .trim_matches(|c: char| matches!(c, ',' | ';' | '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '!' | '?'));
    if t.len() < 2 {
        return;
    }
    let key = t.to_lowercase();
    if seen.insert(key) {
        out.push(t.to_string());
    }
}

fn split_camel(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            parts.push(std::mem::take(&mut cur));
        }
        cur.push(c);
        prev_lower = c.is_lowercase();
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

fn token_priority(s: &str) -> u32 {
    let mut p = 0u32;
    if is_code_shaped(s) {
        p += 100;
    }
    if s.contains("::") || s.contains('/') {
        p += 50;
    }
    if s.contains('_') {
        p += 20;
    }
    if s.len() >= 8 {
        p += 10;
    }
    p
}

/// Extract candidate tokens from free text by **shape**, not language.
pub fn extract_raw_tokens(prompt: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let trimmed = prompt.trim();
    if !trimmed.is_empty() && !trimmed.chars().any(char::is_whitespace) && trimmed.len() >= 2 {
        push_token(&mut out, &mut seen, trimmed);
    }

    for word in prompt.split_whitespace() {
        let w = word.trim_matches(|c: char| {
            !c.is_alphanumeric()
                && c != '_'
                && c != ':'
                && c != '/'
                && c != '\\'
                && c != '.'
                && c != '-'
        });
        if w.is_empty() {
            continue;
        }
        if w.contains("::")
            || w.contains('/')
            || w.contains('\\')
            || (w.contains('.') && w.len() > 3)
        {
            push_token(&mut out, &mut seen, w);
            if let Some(seg) = w
                .rsplit([':', '/', '\\', '.'])
                .find(|s| !s.is_empty() && s.len() >= 2)
            {
                push_token(&mut out, &mut seen, seg);
            }
        }
    }

    // Identifier spans
    let mut cur = String::new();
    let flush_ident = |cur: &mut String, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        if cur.len() < 2 {
            cur.clear();
            return;
        }
        let piece = std::mem::take(cur);
        push_token(out, seen, &piece);
        if piece.contains('_') {
            for part in piece.split('_') {
                if part.len() >= 3 {
                    push_token(out, seen, part);
                }
            }
        }
        for part in split_camel(&piece) {
            if part.len() >= 3 {
                push_token(out, seen, &part);
            }
        }
    };

    for c in prompt.chars() {
        if c.is_alphanumeric() || c == '_' {
            cur.push(c);
        } else if !cur.is_empty() {
            flush_ident(&mut cur, &mut out, &mut seen);
        }
    }
    if !cur.is_empty() {
        flush_ident(&mut cur, &mut out, &mut seen);
    }

    out.sort_by(|a, b| {
        token_priority(b)
            .cmp(&token_priority(a))
            .then_with(|| b.len().cmp(&a.len()))
            .then_with(|| a.cmp(b))
    });
    out
}

fn name_token_hit(name_lower: &str, token_lower: &str, strong: bool) -> bool {
    if name_lower == token_lower {
        return true;
    }
    // Weak prose tokens: exact name or snake segment only (no contains/"ends with")
    if !strong {
        return name_lower
            .split(['_', ':', '-'])
            .any(|seg| seg == token_lower && seg.len() >= 3);
    }
    if token_lower.len() >= 3 && name_lower.ends_with(&format!("_{token_lower}")) {
        return true;
    }
    if token_lower.len() >= 4 && name_lower.ends_with(token_lower) {
        return true;
    }
    if token_lower.len() >= 3 && name_lower.starts_with(token_lower) {
        return true;
    }
    if name_lower.split(['_', ':']).any(|seg| seg == token_lower) {
        return true;
    }
    if token_lower.len() >= 5 && name_lower.contains(token_lower) {
        return true;
    }
    false
}

fn path_token_hit(path_lower: &str, token_lower: &str, strong: bool) -> bool {
    if token_lower.len() < 3 {
        return false;
    }
    // Weak tokens: path segment equality only
    if !strong {
        return path_lower
            .split(['/', '\\', '.', '-'])
            .any(|seg| seg == token_lower);
    }
    if path_lower
        .split(['/', '\\', '.', '-'])
        .any(|seg| seg == token_lower)
    {
        return true;
    }
    if token_lower.len() >= 5 && path_lower.contains(token_lower) {
        return true;
    }
    false
}

/// Does this token hit any node name or file path in the graph?
pub fn token_hits_graph(graph: &CodeGraph, token: &str) -> bool {
    let t = token.to_lowercase();
    if t.len() < 2 {
        return false;
    }
    // O(hits) exact name first — do not mountain-walk for Group/Typer class hubs.
    if exact_name_in_graph(graph, token) {
        return true;
    }
    let strong = is_strong_query_token(token);
    for b in graph.nodes.values() {
        if is_junk_symbol_name(&b.name) {
            continue;
        }
        let name = b.name.to_lowercase();
        if name_token_hit(&name, &t, strong) {
            return true;
        }
        let path = b.file.to_string_lossy().to_lowercase();
        if path_token_hit(&path, &t, strong) {
            return true;
        }
    }
    false
}

/// Exact symbol name present in the warehouse (name_index preferred, nodes fallback).
pub fn exact_name_in_graph(graph: &CodeGraph, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    if !graph.locations_for_name(name).is_empty() {
        return true;
    }
    // Cold / stale index: still honor live nodes (blocks_for_name may scan).
    !graph.blocks_for_name(name).is_empty()
}

/// Resolve prompt → structural query against a live graph (membership = lexicon).
pub fn resolve_structural_query(graph: &CodeGraph, prompt: &str) -> StructuralQuery {
    let prompt_lower = prompt.to_lowercase();
    let raw = extract_raw_tokens(prompt);

    let mut graph_hits = Vec::new();
    let mut strong_unmatched = Vec::new();
    let mut exact_name_hits = Vec::new();
    let mut seen_hit = HashSet::new();
    let mut seen_strong = HashSet::new();
    let mut seen_exact = HashSet::new();

    for tok in raw {
        let key = tok.to_lowercase();
        if key.len() < 2 || is_junk_symbol_name(&tok) {
            continue;
        }
        // Exact name_index / node name → always an exact seed (Group, Typer, …).
        if exact_name_in_graph(graph, &tok) {
            if seen_exact.insert(key.clone()) {
                exact_name_hits.push(tok.clone());
            }
        }
        if token_hits_graph(graph, &tok) {
            if seen_hit.insert(key.clone()) {
                graph_hits.push(tok);
            }
        } else if is_code_shaped(&tok) {
            if seen_strong.insert(key) {
                strong_unmatched.push(tok);
            }
        }
        // prose that doesn't hit the graph: discarded (any human language)
    }

    graph_hits.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    strong_unmatched.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    exact_name_hits.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    StructuralQuery {
        graph_hits,
        strong_unmatched,
        exact_name_hits,
        prompt_lower,
    }
}

/// Score threshold: name is an **exact** match to a ranking token (or whole prompt).
pub const EXACT_NAME_SCORE: f64 = 100_000.0;

/// Normalize a user `target_symbol` / query Ident for seed integrity checks.
///
/// Strips attribute wrappers (`#[pyfunction]` → `pyfunction`) and takes the
/// last `::` segment so `foo::Bar` can match block name `Bar`.
pub fn normalize_seed_query(query: &str) -> String {
    let mut q = query.trim().to_string();
    // `#[pyfunction]` / `#[pymodule]` — keep the attr path leaf.
    if q.starts_with("#[") && q.ends_with(']') {
        q = q
            .trim_start_matches("#[")
            .trim_end_matches(']')
            .trim()
            .to_string();
        if let Some(leaf) = q.rsplit("::").next() {
            q = leaf.trim().to_string();
        }
    }
    if let Some(leaf) = q.rsplit("::").next() {
        if !leaf.is_empty() {
            q = leaf.trim().to_string();
        }
    }
    q
}

/// Preferred ★ integrity: explicit target Ident must equal the seed name.
///
/// **Miss > wrong hit.** Fuzzy substring / underscore-split promotion
/// (`search_py` → `search_lib_dir`, `create_app` → `create_user`) is not a seed.
/// Homonyms with the *same* name remain valid (ranked separately).
///
/// Module-shell interior open (query `walk` → seed `Batch`) is handled by the
/// caller — this only checks Ident equality.
pub fn seed_name_matches_query(query: &str, candidate_name: &str) -> bool {
    let q = normalize_seed_query(query);
    let n = candidate_name.trim();
    if q.is_empty() || n.is_empty() {
        return false;
    }
    q.eq_ignore_ascii_case(n)
}

/// True when `short` is a segment/prefix piece of a longer multi-part query token.
/// Used so `search` from splitting `search_py` cannot promote `search_lib_dir`.
///
/// Includes `prompt_lower` / strong-unmatched full Idents: when `create_app` is not
/// in the graph, ranking only sees fragment `create`, but the full query still forbids
/// soft-matching `create_user`.
fn token_is_fragment_of_longer(short: &str, longer_candidates: &[String]) -> bool {
    let s = short.to_lowercase();
    if s.len() < 3 {
        return false;
    }
    longer_candidates.iter().any(|t| {
        let tl = t.to_lowercase();
        if tl.len() <= s.len() + 1 || tl == s {
            return false;
        }
        // snake / path pieces of a longer query token
        tl.starts_with(&format!("{s}_"))
            || tl.ends_with(&format!("_{s}"))
            || tl.contains(&format!("_{s}_"))
            || (tl.contains('_') && tl.starts_with(&s) && tl.as_bytes().get(s.len()) == Some(&b'_'))
    })
}

/// Tokens that define fragment ancestry for soft-match refusal (ranking + full prompt Ident).
fn fragment_parent_tokens(query: &StructuralQuery) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim();
        if s.len() >= 3 {
            let key = s.to_lowercase();
            if !out.iter().any(|e| e == &key) {
                out.push(key);
            }
        }
    };
    for t in query.ranking_tokens() {
        push(t);
    }
    for t in &query.graph_hits {
        push(t);
    }
    for t in &query.exact_name_hits {
        push(t);
    }
    for t in &query.strong_unmatched {
        push(t);
    }
    // Whole prompt when it is a single Ident (typical target_symbol).
    if !query.prompt_lower.is_empty() && !query.prompt_lower.chars().any(char::is_whitespace) {
        push(&query.prompt_lower);
    }
    out
}

/// Hierarchical name score: exact ≫ prefix/suffix ≫ segment ≫ contains. Longer tokens dominate.
/// Bare names that are only suffixes of a *longer* query token are heavily penalized
/// (`forward` when query is `cpu_gnn_forward`).
///
/// Underscore-split fragments of a longer query token do not soft-match other Idents
/// (`search` from `search_py` must not rank `search_lib_dir` as a name hit).
pub fn structural_name_score(block: &BlockInfo, query: &StructuralQuery) -> f64 {
    if is_junk_symbol_name(&block.name) {
        return 0.0;
    }
    let name = block.name.to_lowercase();
    let tokens = query.ranking_tokens();
    if tokens.is_empty() {
        if !query.prompt_lower.is_empty() && name == query.prompt_lower {
            return EXACT_NAME_SCORE;
        }
        return 0.0;
    }

    // If a longer query token is a superstring of this name, only full exact token match counts.
    let overshadowed = tokens.iter().any(|t| {
        let tl = t.to_lowercase();
        tl.len() > name.len() + 2 && (tl.contains(&name) || tl.ends_with(&format!("_{name}")))
    });
    if overshadowed && !tokens.iter().any(|t| t.eq_ignore_ascii_case(&block.name)) {
        // Still allow tiny residual so graph neighborhood can keep it after hop expand,
        // but never outrank the full symbol on text score alone.
        return 1.0;
    }

    let parent_tokens = fragment_parent_tokens(query);
    let mut best = 0.0f64;
    for t in &tokens {
        let tl = t.to_lowercase();
        if tl.len() < 2 {
            continue;
        }
        // Exact short name still allowed (`search` query → name `search`).
        // Non-exact soft hits via a fragment of a longer query token: refuse.
        if name != tl && token_is_fragment_of_longer(&tl, &parent_tokens) {
            continue;
        }
        let len_boost = (tl.len() as f64).max(3.0);
        let score = if name == tl {
            EXACT_NAME_SCORE + len_boost * 100.0
        } else if name.ends_with(&format!("_{tl}")) {
            800.0 + len_boost * 8.0
        } else if name.ends_with(&tl) && name.len() > tl.len() && tl.len() >= 5 {
            500.0 + len_boost * 5.0
        } else if name.starts_with(&tl) && name.len() > tl.len() && tl.len() >= 4 {
            450.0 + len_boost * 4.0
        } else if name.split(['_', ':']).any(|seg| seg == tl && seg.len() >= 3) {
            400.0 + len_boost * 4.0
        } else if tl.len() >= 5 && name.contains(&tl) {
            150.0 + len_boost * 2.0
        } else {
            0.0
        };
        if score > best {
            best = score;
        }
    }

    if !query.prompt_lower.is_empty() && name == query.prompt_lower {
        best = best.max(EXACT_NAME_SCORE * 2.0);
    }
    best
}

/// Path co-score (module / file segments).
pub fn structural_path_score(block: &BlockInfo, query: &StructuralQuery) -> f64 {
    let path = block.file.to_string_lossy().to_lowercase();
    let tokens = query.ranking_tokens();
    let mut best = 0.0f64;
    for t in tokens {
        let tl = t.to_lowercase();
        if tl.len() < 3 {
            continue;
        }
        let len_boost = tl.len() as f64;
        let score = if path.split(['/', '\\', '.']).any(|seg| seg == tl) {
            80.0 + len_boost * 2.0
        } else if tl.len() >= 4 && path.contains(&tl) {
            25.0 + len_boost
        } else {
            0.0
        };
        if score > best {
            best = score;
        }
    }
    best
}

/// Combined structural text score for retrieval / selection (name + path, no source).
pub fn structural_block_score(block: &BlockInfo, query: &StructuralQuery) -> f64 {
    let n = structural_name_score(block, query);
    let p = structural_path_score(block, query);
    if n <= 0.0 && p <= 0.0 {
        return 0.0;
    }
    n + p * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::model::BlockInfo;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn block(file: &str, name: &str) -> BlockInfo {
        BlockInfo::new(
            PathBuf::from(file),
            "function_item",
            "rust",
            1,
            3,
            0,
            10,
            format!("fn {name}() {{}}"),
            name,
            HashSet::new(),
        )
    }

    fn graph_with(nodes: Vec<BlockInfo>) -> CodeGraph {
        let mut g = CodeGraph::new();
        for b in nodes {
            g.file_hashes
                .insert(b.file.to_string_lossy().to_string(), 1);
            g.nodes.insert(b.id.clone(), b);
        }
        g.rebuild_module_hashes();
        g
    }

    #[test]
    fn polish_prose_drops_without_dictionary() {
        let g = graph_with(vec![
            block("src/snooper/scanner/cache.rs", "load_graph"),
            block("src/server/context_engine.rs", "run_context_logic"),
        ]);
        let q = resolve_structural_query(&g, "gdzie jest ładowanie grafu load_graph");
        assert!(
            q.graph_hits.iter().any(|t| t.contains("load_graph")),
            "should keep identifier, got {:?}",
            q.graph_hits
        );
        assert!(
            !q.graph_hits.iter().any(|t| t == "gdzie" || t == "jest"),
            "prose should not hit: {:?}",
            q.graph_hits
        );
    }

    #[test]
    fn english_prose_only_keeps_graph_members() {
        let g = graph_with(vec![
            block("code_graph/src/gnn/forward.rs", "cpu_gnn_forward"),
            block("src/other.rs", "unrelated_thing"),
        ]);
        let q = resolve_structural_query(&g, "how does the cpu_gnn_forward scoring work please");
        assert!(
            q.ranking_tokens()
                .iter()
                .any(|t| t.contains("cpu_gnn_forward")),
            "got {:?}",
            q.ranking_tokens()
        );
        assert!(!q
            .graph_hits
            .iter()
            .any(|t| t == "how" || t == "does" || t == "please"));
    }

    #[test]
    fn exact_name_beats_suffix() {
        let g = graph_with(vec![
            block("a.rs", "forward"),
            block("b.rs", "cpu_gnn_forward"),
        ]);
        let q = resolve_structural_query(&g, "cpu_gnn_forward");
        let mut scored: Vec<_> = g
            .nodes
            .values()
            .map(|b| (b.name.as_str(), structural_block_score(b, &q)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        assert_eq!(scored[0].0, "cpu_gnn_forward", "got {:?}", scored);
    }

    #[test]
    fn code_shaped_detection() {
        assert!(is_code_shaped("load_graph"));
        assert!(is_code_shaped("cpuGnnForward"));
        assert!(is_code_shaped("foo::bar"));
        assert!(is_code_shaped("src/gnn/forward.rs"));
        // Pure PascalCase hubs (click Group, typer Typer) — short is fine.
        assert!(is_code_shaped("Group"));
        assert!(is_code_shaped("Typer"));
        assert!(is_pure_pascal_case("Group"));
        assert!(is_strong_query_token("Group"));
        assert!(is_strong_query_token("Typer"));
        assert!(!is_code_shaped("how"));
        assert!(!is_code_shaped("loading"));
    }

    #[test]
    fn fail_closed_unmatched_strong_token() {
        let g = graph_with(vec![block("src/a.rs", "load_graph")]);
        let q = resolve_structural_query(&g, "QuantumFluxTransducer");
        assert!(!q.has_usable_hits());
        assert!(q.ranking_tokens().is_empty());
        assert!(q.strong_unmatched.iter().any(|t| t.contains("Quantum")));
    }

    #[test]
    fn weak_prose_not_usable_hits() {
        let g = graph_with(vec![block("src/a.rs", "load_graph")]);
        let q = resolve_structural_query(&g, "please explain how this works carefully");
        assert!(
            !q.has_usable_hits(),
            "prose must not become usable hits: {:?}",
            q.graph_hits
        );
    }

    #[test]
    fn short_pascal_exact_name_is_usable() {
        let mut g = graph_with(vec![
            block("src/click/core.py", "Group"),
            block("typer/main.py", "Typer"),
        ]);
        g.rebuild_name_index();
        let q = resolve_structural_query(&g, "Group");
        assert!(q.has_usable_hits(), "Group should be usable: {:?}", q);
        assert!(q.exact_name_hits.iter().any(|t| t == "Group"));
        assert!(!q.ranking_tokens().is_empty());
        let q2 = resolve_structural_query(&g, "Typer");
        assert!(q2.has_usable_hits(), "Typer should be usable: {:?}", q2);
    }

    #[test]
    fn seed_integrity_rejects_different_ident() {
        assert!(seed_name_matches_query("search_py", "search_py"));
        assert!(seed_name_matches_query("Search_Py", "search_py"));
        assert!(seed_name_matches_query("foo::Bar", "Bar"));
        assert!(seed_name_matches_query("#[pyfunction]", "pyfunction"));
        assert!(!seed_name_matches_query("search_py", "search_lib_dir"));
        assert!(!seed_name_matches_query("create_app", "create_user"));
        assert!(!seed_name_matches_query("search_py", "search"));
    }

    #[test]
    fn fragment_tokens_do_not_promote_unrelated_names() {
        // `search_py` splits to `search` + `search_py`; must not crown `search_lib_dir`.
        let g = graph_with(vec![
            block("a.rs", "search_lib_dir"),
            block("b.rs", "search_py"),
            block("c.rs", "create_user"),
        ]);
        let q = resolve_structural_query(&g, "search_py");
        let mut scored: Vec<_> = g
            .nodes
            .values()
            .map(|b| (b.name.as_str(), structural_name_score(b, &q)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        assert_eq!(scored[0].0, "search_py", "got {:?}", scored);
        assert!(
            scored
                .iter()
                .find(|(n, _)| *n == "search_lib_dir")
                .map(|(_, s)| *s)
                .unwrap_or(0.0)
                < 10.0,
            "search_lib_dir must not soft-win: {:?}",
            scored
        );

        let q2 = resolve_structural_query(&g, "create_app");
        let s_user = g
            .nodes
            .values()
            .find(|b| b.name == "create_user")
            .map(|b| structural_name_score(b, &q2))
            .unwrap_or(0.0);
        assert!(
            s_user < 10.0,
            "create_user must not soft-win for create_app: {s_user}"
        );
    }
}
