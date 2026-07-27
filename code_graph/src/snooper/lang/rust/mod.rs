// code_graph/src/snooper/lang/rust/mod.rs
//
// Facade for the Rust language module after directory promotion (Strangler Fig).
// Splits concerns:
// - parser.rs: Phase 1 block extraction (parse, visit_node, name/external extractors)
// - edges.rs: call/usage edge collection (collect_* and build_* variants)
//
// Shared consts, heavy load_dependency_versions, build_search_regexes (used by collector),
// and tests live here. Re-exports preserve the public/crate API for scanner, builder,
// collector, etc.

pub mod edges;
pub mod ffi;
pub mod parser;

pub(crate) use edges::{build_usage_edges, collect_call_edges, collect_usage_edges};
pub use parser::parse;
pub use ffi::{collect_pyfunction_exports, parse_pyfunction_export_name};

use crate::{BlockInfo, CodeGraph};
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Query;

// Re-export ParseError for consistency (was in original)
pub use super::super::parser::ParseError; // or from lang mod, but direct for now

// 4-arg build_call_edges (HIT 9) -- the shim was killed from edges.rs; this forwarder lives
// in the module that owns the lang consts (CALL_QUERY, GENERIC_NAMES) and can access parser::extract.
pub(crate) fn build_call_edges(
    blocks: &[BlockInfo],
    source: &str,
    tree: &tree_sitter::Tree,
    graph: &mut CodeGraph,
) {
    super::generic_edges::build_call_edges(
        blocks,
        source,
        tree,
        graph,
        &["function_item", "impl_item", "trait_item"],
        &["function_item"],
        || Query::new(&tree_sitter_rust::LANGUAGE.into(), CALL_QUERY),
        |b, src| {
            if !b.name.is_empty() {
                Some(b.name.clone())
            } else {
                parser::extract_name_from_block(b, src)
            }
        },
        GENERIC_NAMES,
        // QueryOnly — see edges.rs: body-scan false positives on comments/tests.
        super::generic_edges::FallbackStyle::QueryOnly,
    );
}

// Blacklist of extremely common Rust names (methods, fns, traits) that would
// cause thousands of false-positive cross-crate edges (and pollute the graph)
// if global_names fallback was used for them.
// Distinctive names (e.g. "scan_workspace", "load_graph", "ensure_call_graph",
// "do_scan_workspace" etc.) are NOT in the list and will resolve via global
// when no local def in the current file's blocks.
// Purpose: safety for cross-crate (cli/ <-> code_graph/ etc.) while protecting
// from common-name noise. See ensure_call_graph for global map population.
const GENERIC_NAMES: &[&str] = &[
    "new",
    "default",
    "clone",
    "from",
    "into",
    "parse",
    "build",
    "run",
    "main",
    "eq",
    "partial_eq",
    "fmt",
];

const CALL_QUERY: &str = "
(call_expression
 function: (identifier) @call.name
) @call

(call_expression
 function: (field_expression
  field: (field_identifier) @call.name
 )
) @call
";

// (USAGE queries were stubs; no const here for now)

// Public helper for Rust dependency versions (exact from Cargo.lock, workspace-aware)
// Safe version — never panics on bad paths or multi-byte chars
//
// This version isolates the potentially stack-heavy `cargo metadata` call
// (especially on massive monorepos like rust-lang/rust) by running it
// inside a dedicated thread with an explicit large stack.
pub fn load_dependency_versions(file: &Path) -> HashMap<String, String> {
    let mut versions = HashMap::new();

    // Robust workspace root detection
    let workspace_root = file
        .ancestors()
        .find(|ancestor| ancestor.join("Cargo.toml").exists())
        .unwrap_or_else(|| std::path::Path::new("."));

    let manifest_path = workspace_root.join("Cargo.toml");
    let manifest_for_logging = manifest_path.clone();
    let manifest_for_post_log = manifest_path.clone();

    // Fast path: no Cargo.toml at all → nothing to do
    if !manifest_path.exists() {
        return versions;
    }

    // Fast check: if `cargo` binary isn't available, don't even try
    let cargo_available = std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !cargo_available {
        eprintln!(
            "⚠️ `cargo` command not found in PATH while scanning {:?}.\n\
             Butler is running in an environment without the Rust toolchain (common in Docker).\n\
             Continuing without dependency version lookup. All other features still work.",
            manifest_for_logging
        );
        return versions;
    }

    // Run the potentially heavy cargo_metadata call in a dedicated thread
    // with a large explicit stack to survive huge monorepos.
    let thread_handle = std::thread::Builder::new()
        .name("cargo-metadata-large-stack".to_string())
        .stack_size(32 * 1024 * 1024) // 32 MiB — enough even for rust-lang/rust class repos
        .spawn(move || -> HashMap<String, String> {
            match cargo_metadata::MetadataCommand::new()
                .manifest_path(&manifest_path)
                .exec()
            {
                Ok(metadata) => {
                    let mut v = HashMap::new();
                    for pkg in metadata.packages {
                        v.insert(pkg.name.to_string(), pkg.version.to_string());
                    }
                    v
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("No such file or directory") || err_str.contains("not found") {
                        eprintln!(
                            "⚠️ Could not run `cargo metadata` for {:?} because `cargo` is not in PATH.\n\
                             This is common in minimal Docker containers. Butler will continue without dependency version info.\n\
                             (The project IS a Rust workspace — this is just an environment limitation.)",
                            manifest_for_logging
                        );
                    } else {
                        eprintln!(
                            "⚠️ cargo_metadata failed for {:?}: {} (this is normal if the directory is not a Cargo workspace)",
                            manifest_for_logging, e
                        );
                    }
                    HashMap::new()
                }
            }
        });

    match thread_handle {
        Ok(handle) => match handle.join() {
            Ok(v) => {
                versions = v;
                if !versions.is_empty() {
                    println!("✅ Loaded {} dependency versions", versions.len());
                }
            }
            Err(_) => {
                eprintln!(
                    "⚠️ cargo-metadata thread panicked while scanning {:?}. Continuing without dependency versions.",
                    manifest_for_post_log
                );
            }
        },
        Err(e) => {
            eprintln!(
                "⚠️ Failed to spawn cargo-metadata thread for {:?}: {}. Continuing without dependency versions.",
                manifest_for_post_log, e
            );
        }
    }

    versions
}

/// Builds a set of case-insensitive regexes for robust keyword matching
/// against Rust code (handles snake_case ↔ camelCase/PascalCase).
///
/// This is the recommended way to do keyword matching in Butler.
pub fn build_search_regexes(keywords: &[&str]) -> Vec<regex::Regex> {
    keywords
        .iter()
        .filter_map(|kw| {
            let kw = kw.trim();
            if kw.is_empty() {
                return None;
            }

            let mut patterns = vec![regex::escape(kw)];

            if kw.contains('_') {
                // snake_case → also match the squashed version (common in LLM output)
                let squashed = kw.replace('_', "");
                patterns.push(regex::escape(&squashed));
            } else {
                // camelCase / PascalCase → also match snake_case version
                let snake = kw.chars().fold(String::new(), |mut acc, c| {
                    if c.is_uppercase() && !acc.is_empty() {
                        acc.push('_');
                    }
                    acc.push(c.to_lowercase().next().unwrap());
                    acc
                });
                if snake != kw {
                    patterns.push(regex::escape(&snake));
                }
            }

            let pattern = patterns.join("|");

            regex::RegexBuilder::new(&pattern)
                .case_insensitive(true)
                .build()
                .ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_search_regexes_snake_to_camel() {
        let keywords = vec!["symbol_tank", "rate_limit"];
        let regexes = build_search_regexes(&keywords);

        assert!(!regexes.is_empty());

        // Should match both snake_case and camelCase/PascalCase
        assert!(regexes.iter().any(|r| r.is_match("symbol_tank")));
        assert!(regexes.iter().any(|r| r.is_match("SymbolTank")));
        assert!(regexes.iter().any(|r| r.is_match("symbolTank")));

        assert!(regexes.iter().any(|r| r.is_match("rate_limit")));
        assert!(regexes.iter().any(|r| r.is_match("RateLimit")));
    }

    #[test]
    fn test_build_search_regexes_camel_to_snake() {
        let keywords = vec!["symbolTank", "RateLimit"];
        let regexes = build_search_regexes(&keywords);

        assert!(regexes.iter().any(|r| r.is_match("symbol_tank")));
        assert!(regexes.iter().any(|r| r.is_match("symbolTank")));
        assert!(regexes.iter().any(|r| r.is_match("RateLimit")));
        assert!(regexes.iter().any(|r| r.is_match("rate_limit")));
    }

    #[test]
    fn test_build_search_regexes_case_insensitive() {
        let keywords = vec!["auth"];
        let regexes = build_search_regexes(&keywords);

        assert!(regexes.iter().any(|r| r.is_match("Auth")));
        assert!(regexes.iter().any(|r| r.is_match("AUTH")));
        assert!(regexes.iter().any(|r| r.is_match("aUtH")));
    }

    #[test]
    fn test_build_search_regexes_ignores_empty() {
        let keywords = vec!["", "   ", "valid"];
        let regexes = build_search_regexes(&keywords);

        // Should only have one regex (for "valid")
        assert_eq!(regexes.len(), 1);
    }

    #[test]
    fn test_build_search_regexes_matches_source_code() {
        let keywords = vec!["my_foo_bar"];
        let regexes = build_search_regexes(&keywords);

        let code_snippet = "fn myFooBar() { /* ... */ }";
        assert!(regexes.iter().any(|r| r.is_match(code_snippet)));
    }
}
