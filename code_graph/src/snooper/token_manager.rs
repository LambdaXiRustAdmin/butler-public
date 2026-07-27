use std::sync::OnceLock;
use tiktoken_rs::CoreBPE;

static BPE: OnceLock<CoreBPE> = OnceLock::new();

pub fn count_tokens(text: &str) -> usize {
    let bpe = BPE.get_or_init(|| tiktoken_rs::cl100k_base().unwrap());
    bpe.encode_ordinary(text).len().max(text.chars().count())
}

/// Fast token-budget truncation: at most two `count_tokens` passes (no per-line loop).
pub fn truncate_to_token_cap(text: &str, cap: usize) -> String {
    if cap == 0 || text.is_empty() {
        return String::new();
    }

    // Small inputs: one count on the full string.
    if text.len() <= cap.saturating_mul(4) && count_tokens(text) <= cap {
        return text.to_string();
    }

    let primary = slice_at_byte_bound(text, cap.saturating_mul(4));
    if count_tokens(primary) <= cap {
        return primary.to_string();
    }

    // Secondary slice only (byte bound) — no third tiktoken pass.
    slice_at_byte_bound(text, cap.saturating_mul(2)).to_string()
}

fn slice_at_byte_bound(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = floor_char_boundary(text, max_bytes);
    if let Some(nl) = text[..end].rfind('\n') {
        end = nl;
    }
    &text[..end]
}

fn floor_char_boundary(text: &str, max_bytes: usize) -> usize {
    let end = max_bytes.min(text.len());
    let mut i = end;
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub fn should_include_full_code(
    block: &crate::BlockInfo,
    current_tokens: usize,
    max_tokens: usize,
    keyword_mode: bool,
) -> bool {
    if keyword_mode && current_tokens > max_tokens / 3 {
        return false;
    }
    current_tokens + count_tokens(&block.source) <= max_tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockInfo;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn make_block(source: &str) -> BlockInfo {
        BlockInfo {
            id: crate::Id::new("test.rs", "function_item", "deadbeef"),
            name: "test_fn".to_string(),
            file: PathBuf::from("test.rs"),
            kind: "function_item".to_string(),
            lang: "rust".to_string(),
            start_line: 1,
            end_line: 5,
            start_byte: 0,
            end_byte: source.len(),
            parent_id: None,
            children: vec![],
            content_hash: "deadbeef".to_string(),
            sig_hash: "sig".to_string(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: source.to_string(),
            score: 0.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        }
    }

    #[test]
    fn test_count_tokens_basic() {
        let text = "fn main() { println!(\"hello\"); }";
        let count = count_tokens(text);
        assert!(count > 0);
        assert!(count < 50); // sanity bound
    }

    #[test]
    fn test_should_include_full_code_within_budget() {
        let block = make_block("fn foo() {}");
        assert!(should_include_full_code(&block, 100, 1000, false));
    }

    #[test]
    fn test_should_include_full_code_exceeds_budget() {
        let long_source = "fn foo() {".to_string() + &"x".repeat(4000);
        let block = make_block(&long_source);
        assert!(!should_include_full_code(&block, 8000, 5000, false));
    }

    #[test]
    fn test_truncate_to_token_cap_short_circuit() {
        let text = "fn main() {}";
        assert_eq!(truncate_to_token_cap(text, 1500), text);
    }

    #[test]
    fn test_truncate_to_token_cap_large_text() {
        let line = "def helper() -> None:\n    pass\n";
        let text = line.repeat(5000);
        let out = truncate_to_token_cap(&text, 1500);
        assert!(!out.is_empty());
        assert!(out.len() < text.len());
        assert!(out.len() <= 1500usize.saturating_mul(2));
        assert!(text.is_char_boundary(out.len()));
    }

    #[test]
    fn test_keyword_mode_cuts_early() {
        let block = make_block("fn foo() {}");
        // In keyword mode, once we're past 1/3 of budget, we stop including full code
        assert!(!should_include_full_code(&block, 2000, 5000, true));
    }
}
