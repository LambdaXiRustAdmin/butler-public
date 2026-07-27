// code_graph/src/snooper/parser.rs
use super::lang;
use super::utils::normalize_path;
use std::path::PathBuf;
use tree_sitter::Tree;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("Failed to load grammar: {0}")]
    GrammarLoad(String),
    #[error("Tree-sitter parse failed")]
    ParseFailed,
    #[error("Unknown language: {0}")]
    UnknownLanguage(String),
}

/// Result of parsing one source file.
/// Contains the extracted blocks (from visit_node) plus the raw data
/// needed to later build call/usage edges without re-parsing.
#[derive(Debug)]
pub struct ParsedFile {
    pub path: PathBuf,
    pub source: String,
    pub blocks: Vec<crate::BlockInfo>,
    /// Tree is kept for languages that need it for query-based edge building (Rust).
    pub tree: Option<Tree>,
}

pub fn parse_file(path: impl Into<PathBuf>, source: &str) -> Result<ParsedFile, ParseError> {
    let path = normalize_path_buf(path.into());
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    match ext {
        "rs" => lang::rust::parse(path, source),
        "py" => lang::python::parser::parse(path, source),
        "ts" | "tsx" | "js" | "jsx" | "svelte" => lang::typescript::parser::parse(path, source),
        "go" => lang::go::parser::parse(path, source),
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" | "C" => {
            // Dialect from extension + content sniff (C vs C++ grammars stay separate).
            lang::parse_c_family(path, source)
        }
        _ => Err(ParseError::UnknownLanguage(ext.to_string())),
    }
}

fn normalize_path_buf(p: PathBuf) -> PathBuf {
    PathBuf::from(normalize_path(&p.to_string_lossy()))
}

/// Convenience helper for the incremental watcher.
///
/// Reads the file from disk and returns the full `ParsedFile` (blocks + source + tree).
/// This allows the watcher to rebuild call/usage edges for the changed file without
/// triggering a full workspace re-scan.
pub fn parse_single_file(path: impl Into<PathBuf>) -> Result<super::ParsedFile, ParseError> {
    let path = normalize_path_buf(path.into());
    let source = std::fs::read_to_string(&path).map_err(|_| ParseError::ParseFailed)?;
    parse_file(path, &source)
}
