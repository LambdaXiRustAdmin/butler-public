//! Natural language intent detection and prompt analysis helpers for the Butler server.

use crate::server::dto::PromptIntent;
use code_graph::CodeGraph;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use strsim::normalized_levenshtein;

/// Validates that a file path is contained within the expected workspace root.
///
/// Prevents directory traversal attacks by canonicalizing both paths and checking
/// that the resolved file path starts with the resolved workspace root.
///
/// # Arguments
/// * `file_path` - The file path to validate
/// * `workspace_root` - The expected workspace root directory
///
/// # Returns
/// - `Some(canonicalized_path)` if the file is within the workspace root
/// - `None` if canonicalization fails or the path escapes the workspace (potential traversal attack)
pub fn validate_file_path(file_path: &Path, workspace_root: &Path) -> Option<std::path::PathBuf> {
    let canonical_file = file_path.canonicalize().ok()?;
    let canonical_root = workspace_root.canonicalize().ok()?;
    if canonical_file.starts_with(&canonical_root) {
        Some(canonical_file)
    } else {
        None // path traversal attempt
    }
}

/// Detects when a prompt requests an entire file's contents (full module dump).
///
/// Triggered by keywords like "full", "entire", "whole", "raw" or when the prompt matches
/// a filename. Validates paths against directory traversal attacks before reading files.
///
/// # Returns
/// A vector of `(file_path, file_content)` tuples for matched files.
pub fn detect_full_files(prompt: &str, graph: &CodeGraph) -> Vec<(String, String)> {
    let lower = prompt.to_lowercase();
    let wants_full = lower.contains("full")
        || lower.contains("entire")
        || lower.contains("whole")
        || lower.contains("raw")
        || lower.ends_with(".rs");
    if !wants_full {
        return vec![];
    }

    let mut results = vec![];
    for file_path in graph
        .nodes
        .values()
        .map(|b| &b.file)
        .collect::<HashSet<_>>()
    {
        let file_name = file_path
            .file_name()
            .map_or("".to_string(), |s| s.to_string_lossy().to_string());
        let stem = file_path
            .file_stem()
            .map_or("".to_string(), |s| s.to_string_lossy().to_string());
        let similarity = normalized_levenshtein(&lower, &file_name.to_lowercase());
        if similarity > 0.6
            || lower.contains(&file_name.to_lowercase())
            || lower.contains(&stem.to_lowercase())
        {
            // Validate path to prevent directory traversal attacks
            if let Some(valid_path) = validate_file_path(file_path, Path::new(".")) {
                if let Ok(content) = fs::read_to_string(&valid_path) {
                    results.push((file_path.display().to_string(), content));
                }
            }
        }
    }
    results
}

/// Detects whether a prompt appears to be a natural language question rather than keyword-style input.
///
/// Analyzes the prompt for common indicators of full-sentence usage (question marks, English filler words,
/// request phrases) and returns a formatted guidance message if detected. This helps small LLM clients
/// understand why their query failed and how to fix it.
///
/// # Returns
/// - `Some(guidance_message)` if the prompt looks like natural language, containing actionable advice
/// - `None` if the prompt appears to be valid keyword input
pub fn detect_natural_language_guidance(prompt: &str) -> Option<String> {
    let lower = prompt.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    let mut reasons = vec![];

    // Strong signals
    if lower.contains('?') {
        reasons.push("contains a question mark");
    }
    if lower.starts_with("can ") || lower.starts_with("could ") || lower.starts_with("please ") {
        reasons.push("starts with a request phrase");
    }

    let question_indicators = [
        "what ", "how ", "where ", "why ", "find ", "show ", "explain ", "get the ", "list all",
        "tell me",
    ];
    if question_indicators.iter().any(|w| lower.contains(w)) {
        reasons.push("contains question/request words");
    }

    // Many common English filler words
    let filler = [
        " the ", " a ", " an ", " is ", " are ", " to ", " for ", " with ", " and ", " that ",
        " this ", " please ",
    ];
    let filler_count = filler.iter().filter(|w| lower.contains(*w)).count();

    if words.len() > 7 && filler_count >= 3 {
        reasons.push("looks like a full English sentence");
    }

    if reasons.is_empty() {
        return None;
    }

    let reason_text = reasons.join("+");
    // Dense agent contract — models don't need English tutorials.
    Some(cli::butler_instructions::dense_nl_nudge(&reason_text))
}

/// Checks whether a prompt contains keywords suggesting the user is asking about the Butler tool itself.
///
/// Detects meta-queries like "how to use butler", "configuration", "documentation", etc. These are
/// distinguished from actual code searches so the server can serve instructions instead of searching code.
///
/// # Returns
/// `true` if any meta-related keyword is found in the prompt, `false` otherwise.
pub fn contains_meta_keywords(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();

    let meta_keywords = [
        "instructions",
        "how to use",
        "how do i use",
        "how does this work",
        "what is butler",
        "butler tool",
        "this tool",
        "configuration",
        "config",
        "setup",
        "guide",
        "manual",
        "documentation",
        "how does butler",
        "how to call",
        "how to query",
        "how to get instructions",
    ];

    meta_keywords.iter().any(|kw| lower.contains(kw))
}

/// Detects whether a prompt references a specific line number or file location in the codebase.
///
/// Uses pattern matching for indicators like "line 17", "at l42", "L#50", and Rust identifier patterns
/// (snake_case, CamelCase) near line-number mentions. Used to route requests toward surgical mode.
///
/// # Returns
/// `true` if the prompt appears to target a specific location in code.
pub fn looks_like_specific_location_targeting(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();

    // Detect line number references (general patterns)
    let has_line_number = (lower.contains("line ") && lower.chars().any(|c| c.is_ascii_digit()))
        || (lower.contains(" at line") && lower.chars().any(|c| c.is_ascii_digit()))
        || (lower.contains(" at l") && lower.chars().any(|c| c.is_ascii_digit()))
        || (lower.contains("l#") && lower.chars().any(|c| c.is_ascii_digit()));

    // Detect module / file / identifier references (general, not content-specific)
    let has_module_context = lower.contains(" of ") || 
        lower.contains(" in ") ||
        lower.contains("mod ") ||
        lower.contains("::") ||
        // Looks like a Rust identifier (snake_case or CamelCase) near line mention
        lower.split_whitespace().any(|word| {
            (word.contains('_') || word.chars().any(|c| c.is_uppercase())) && word.len() > 2
        });

    has_line_number && has_module_context
}

/// Analyzes the user's prompt and decides the intent.
/// Supports bypass mechanisms so users can still search for meta-keywords in their code.
/// Classifies the intent of a `butler_context` prompt for smart routing.
///
/// Three possible intents:
/// - [`PromptIntent::NormalSearch`]: Standard keyword-based code search (after applying any bypass stripping)
/// - [`PromptIntent::MetaQuestion`]: User is asking about how to use Butler itself → server serves instructions
/// - [`PromptIntent::LocationTargeting`]: User references a specific line/module → prefer surgical mode
///
/// The function applies several transformations in priority order:
/// 1. **Bypass detection**: `"search:"` prefix or quoted strings strip meta-processing
/// 2. **Natural language + meta keywords** → [`PromptIntent::MetaQuestion`] (serve instructions)
/// 3. **Natural language + location targeting** → [`PromptIntent::LocationTargeting`] (suggest surgical mode)
/// 4. **Default**: [`PromptIntent::NormalSearch`] with the cleaned prompt
///
/// # Bypass Mechanisms
/// - Prefix `search:` forces normal search interpretation regardless of content
/// - Quoted strings (`"instructions"` or `'configuration'`) bypass meta-detection
///
/// # Arguments
/// * `prompt` - The raw prompt string from the client request
///
/// # Returns
/// A [`PromptIntent`] variant indicating how to handle this prompt.
pub fn analyze_butler_prompt_intent(prompt: &str) -> PromptIntent {
    let trimmed = prompt.trim();

    // Bypass 1: "search:" prefix (very explicit)
    if let Some(rest) = trimmed
        .strip_prefix("search:")
        .or_else(|| trimmed.strip_prefix("Search:"))
    {
        return PromptIntent::NormalSearch {
            cleaned_prompt: rest.trim().to_string(),
        };
    }

    // Bypass 2: Quoted string (e.g. "instructions" or 'configuration')
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() > 1)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() > 1)
    {
        let inner = &trimmed[1..trimmed.len() - 1];
        return PromptIntent::NormalSearch {
            cleaned_prompt: inner.to_string(),
        };
    }

    let looks_like_natural_language = detect_natural_language_guidance(prompt).is_some();

    // Priority 1: Meta questions about the tool itself
    let has_meta_keywords = contains_meta_keywords(prompt);
    if looks_like_natural_language && has_meta_keywords {
        return PromptIntent::MetaQuestion;
    }

    // Priority 2: User is trying to target a specific line or module
    // (e.g. "line 17 of fused_chain", "L42 in the allocation module")
    if looks_like_natural_language && looks_like_specific_location_targeting(prompt) {
        return PromptIntent::LocationTargeting;
    }

    PromptIntent::NormalSearch {
        cleaned_prompt: prompt.to_string(),
    }
}
