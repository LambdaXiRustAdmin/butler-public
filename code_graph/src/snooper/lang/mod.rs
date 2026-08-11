//! Language-specific parsers and edge collectors.
//! Each submodule provides parse + collect_call/usage edges using Tree-sitter.
//!
//! **C vs C++** are separate modules (`c`, `cpp`) with matching Tree-sitter
//! grammars. Shared dialect sniff + decl↔def semantics live in `c_family`.

pub mod c;
pub mod c_family;
pub mod cpp;
pub mod go;
pub mod python;
pub mod rust;
pub mod typescript;

mod generic_edges;
pub(crate) mod generic_parser;

// Re-export ParseError for language modules to use
pub use super::parser::ParseError;

use crate::{BlockInfo, Id};
use std::collections::HashMap;
use std::path::Path;

/// Dispatch parse for a C-family path using automatic dialect detection.
pub fn parse_c_family(
    path: std::path::PathBuf,
    source: &str,
) -> Result<super::parser::ParsedFile, ParseError> {
    match c_family::dialect_for_file(&path, source) {
        c_family::CFamilyDialect::C => c::parse(path, source),
        c_family::CFamilyDialect::Cpp => cpp::parse(path, source),
    }
}

/// Collect call+usage edges using the dialect that matches how the file was parsed.
/// Uses path+source sniff (same rules as parse) so query language ≡ tree language.
pub(crate) fn collect_c_family_edges(
    path: &Path,
    source: &str,
    blocks: &[BlockInfo],
    tree: &tree_sitter::Tree,
    global: Option<&HashMap<String, Id>>,
) -> Vec<(Id, Id)> {
    match c_family::dialect_for_file(path, source) {
        c_family::CFamilyDialect::C => {
            let mut call = c::collect_call_edges(blocks, source, tree, global);
            let usage = c::collect_usage_edges(blocks, source, tree);
            call.extend(usage);
            call
        }
        c_family::CFamilyDialect::Cpp => {
            let mut call = cpp::collect_call_edges(blocks, source, tree, global);
            let usage = cpp::collect_usage_edges(blocks, source, tree);
            call.extend(usage);
            call
        }
    }
}

pub(crate) fn build_c_family_edges(
    path: &Path,
    source: &str,
    blocks: &[BlockInfo],
    tree: &tree_sitter::Tree,
    graph: &mut crate::CodeGraph,
) {
    match c_family::dialect_for_file(path, source) {
        c_family::CFamilyDialect::C => {
            c::build_call_edges(blocks, source, tree, graph);
            c::build_usage_edges(blocks, source, tree, graph);
        }
        c_family::CFamilyDialect::Cpp => {
            cpp::build_call_edges(blocks, source, tree, graph);
            cpp::build_usage_edges(blocks, source, tree, graph);
        }
    }
}
