//! Per-file edge collect dispatch + source path helpers (Thesis B3 peel).
//!
//! Lang match arms for call+usage; abs/rel warehouse path I/O for edge reads.
//! Zero intentional behavior change — no new langs or family rules.

use crate::snooper::model::{BlockInfo, CodeGraph, Id};
use std::path::{Path, PathBuf};

use super::lang;

/// True when extension participates in CALL/usage edge building.
pub(crate) fn is_edge_buildable_ext(ext: &str) -> bool {
    CodeGraph::is_edge_buildable_path(Path::new(&format!("x.{ext}")))
}

/// Resolve a path stored on blocks (repo-relative warehouse keys) to absolute for disk I/O.
/// Docker server cwd is `/app` — must join project root or edge reads fail with 0 edges.
pub(crate) fn abs_source_path(root: &Path, p: &Path) -> PathBuf {
    super::project_paths::ProjectPaths::new(root).to_abs(p)
}

pub(crate) fn rel_source_path(root: &Path, p: &Path) -> PathBuf {
    super::project_paths::ProjectPaths::new(root).to_rel(p)
}

/// Zero-cost dispatch (HIT 4): single tight match instead of duplicated if-ext chains
/// in update_single_file / ensure_call_graph / run_background_...._inner.
/// Returns combined call+usage edges for the parsed file (global optional for cross-crate).
pub(crate) fn collect_edges_for_lang(
    path: &Path,
    ext: &str,
    blocks: &[BlockInfo],
    source: &str,
    tree: &tree_sitter::Tree,
    global: Option<&std::collections::HashMap<String, Id>>,
    go_all: Option<&std::collections::HashMap<String, Vec<Id>>>,
) -> Vec<(Id, Id)> {
    if lang::c_family::is_c_family_ext(ext) {
        return lang::collect_c_family_edges(path, source, blocks, tree, global);
    }
    let mut call = match ext {
        "rs" => lang::rust::collect_call_edges(blocks, source, tree, global),
        "py" => lang::python::collect_call_edges(blocks, source, tree, global),
        "ts" | "tsx" | "js" | "jsx" | "svelte" => {
            lang::typescript::collect_call_edges(blocks, source, tree, global)
        }
        "go" => lang::go::collect_call_edges(blocks, source, tree, global, go_all),
        _ => Vec::new(),
    };
    let usage = match ext {
        "rs" => lang::rust::collect_usage_edges(blocks, source, tree),
        "py" => lang::python::collect_usage_edges(blocks, source, tree),
        "ts" | "tsx" | "js" | "jsx" | "svelte" => {
            lang::typescript::collect_usage_edges(blocks, source, tree)
        }
        "go" => lang::go::collect_usage_edges(blocks, source, tree),
        _ => Vec::new(),
    };
    call.extend(usage);
    call
}
