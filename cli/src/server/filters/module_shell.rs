//! Module shell → interior seed resolution.
use super::seed_tier::{
    filter_seed_candidates, is_testish_seed_block, seed_role_tier,
};
use code_graph::{BlockInfo, CodeGraph, Id};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Module shell → interior seed (deterministic door + heuristic / GNN rank)
// ---------------------------------------------------------------------------

fn path_key(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").to_lowercase()
}

/// True if this block is a module *label* (e.g. `mod walk;`) rather than interior API.
pub fn is_module_shell(b: &BlockInfo) -> bool {
    let k = b.kind.to_lowercase();
    if !(k.contains("mod_item") || k == "module" || k.contains("namespace")) {
        return false;
    }
    let src = b.source.trim();
    // Classic Rust: `mod walk;` / `pub mod walk;`
    if src.ends_with(';') {
        return true;
    }
    // Empty-ish body or very short shell
    if src.len() < 80 && !src.contains('{') {
        return true;
    }
    // Inline `mod walk { ... }` with only a few tokens — still resolve if we find better interior
    seed_role_tier(&b.kind) == 30
}

/// Candidate filesystem locations for `mod name;` relative to the declaring file.
pub fn module_file_path_hints(mod_block: &BlockInfo) -> Vec<PathBuf> {
    let name = mod_block.name.as_str();
    if name.is_empty() {
        return vec![];
    }
    let parent = match mod_block.file.parent() {
        Some(p) => p.to_path_buf(),
        None => return vec![],
    };
    vec![
        // Rust
        parent.join(format!("{name}.rs")),
        parent.join(name).join("mod.rs"),
        parent.join(name).join("lib.rs"),
        // Python package / module
        parent.join(format!("{name}.py")),
        parent.join(name).join("__init__.py"),
        // Go / TS (best-effort)
        parent.join(format!("{name}.go")),
        parent.join(format!("{name}.ts")),
        parent.join(format!("{name}.tsx")),
        parent.join(format!("{name}.js")),
        parent.join(name).join("index.ts"),
        parent.join(name).join("index.js"),
        // C/C++
        parent.join(format!("{name}.c")),
        parent.join(format!("{name}.cc")),
        parent.join(format!("{name}.cpp")),
        parent.join(format!("{name}.h")),
        parent.join(format!("{name}.hpp")),
    ]
}

fn file_belongs_to_module(file: &Path, mod_block: &BlockInfo) -> bool {
    let f = path_key(file);
    let name = mod_block.name.to_lowercase();
    if name.is_empty() {
        return false;
    }
    for hint in module_file_path_hints(mod_block) {
        let h = path_key(&hint);
        if f == h || f.ends_with(&h) {
            return true;
        }
        // ends_with with path separator for uniqueness
        if let Some(stripped) = h.strip_prefix('/') {
            if f.ends_with(stripped) {
                return true;
            }
        }
        // match by suffix components e.g. .../walk.rs
        if let Some(file_name) = hint.file_name().and_then(|s| s.to_str()) {
            if f.ends_with(&format!("/{file_name}")) && f.contains(&format!("/{name}"))
                || f.ends_with(&format!("/{name}.rs"))
                || f.ends_with(&format!("/{name}.py"))
                || f.ends_with(&format!("/{name}.go"))
                || f.ends_with(&format!("/{name}.ts"))
            {
                return true;
            }
        }
    }
    // Directory form: .../walk/foo.rs under same parent as declaration
    let parent = match mod_block.file.parent() {
        Some(p) => path_key(p),
        None => return false,
    };
    let dir_prefix = if parent.ends_with('/') {
        format!("{parent}{name}/")
    } else {
        format!("{parent}/{name}/")
    };
    f.starts_with(&dir_prefix) || f.contains(&format!("/{name}/"))
}

fn is_under_mod_in_ast(graph: &CodeGraph, mut cur: Id, mod_id: &Id) -> bool {
    let mut guard = 0;
    while guard < 64 {
        guard += 1;
        if cur == *mod_id {
            return true;
        }
        let Some(b) = graph.get_block(cur.clone()) else {
            return false;
        };
        match &b.parent_id {
            Some(p) => cur = p.clone(),
            None => return false,
        }
    }
    false
}

/// Blocks that live in the module's file(s) or under the mod AST node (not the shell itself).
pub fn collect_module_interior<'a>(
    graph: &'a CodeGraph,
    mod_block: &'a BlockInfo,
    scoped: &[&'a BlockInfo],
) -> Vec<&'a BlockInfo> {
    let mod_id = &mod_block.id;
    let mut out: Vec<&BlockInfo> = Vec::new();

    let consider = |b: &'a BlockInfo, out: &mut Vec<&'a BlockInfo>| {
        if b.id == *mod_id || b.name.is_empty() {
            return;
        }
        if seed_role_tier(&b.kind) == 0 {
            return;
        }
        let in_file = file_belongs_to_module(&b.file, mod_block);
        let in_ast = is_under_mod_in_ast(graph, b.id.clone(), mod_id);
        if in_file || in_ast {
            out.push(b);
        }
    };

    for b in scoped.iter().copied() {
        consider(b, &mut out);
    }

    // Scoped view may miss files if scope was tight; fall back to full graph once.
    if out.is_empty() {
        for b in graph.nodes.values() {
            consider(b, &mut out);
        }
    }

    // Dedup by id
    let mut seen = HashSet::new();
    out.retain(|b| seen.insert(b.id.clone()));
    out
}

/// Heuristic (+ optional neural score already on `block.score`) rank for in-module API pick.
///
/// **Dominates:** `seed_role_tier × 1000` (0…100k). Query/stem bonuses hundreds.
/// Neural score weight 80 vs 15 when `use_neural_scores`. Test penalty −1500 (small vs tier×1k).
pub fn module_interior_rank_score(query: &str, b: &BlockInfo, use_neural_scores: bool) -> f64 {
    let q = query.trim().to_lowercase();
    let n = b.name.to_lowercase();
    let mut s = seed_role_tier(&b.kind) as f64 * 1_000.0;

    if !q.is_empty() {
        if n == q {
            s += 500.0;
        } else if n.contains(&q) || q.contains(&n) {
            s += 200.0;
        }
    }

    let stem = b
        .file
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !q.is_empty() && stem == q {
        s += 350.0; // walk.rs contents when query is "walk"
    }
    if stem == n {
        s += 120.0;
    }

    if b.is_highly_connected {
        s += 150.0;
    }

    // Neural (or heuristic) score already applied to the graph when use_neural ran upstream.
    let score_weight = if use_neural_scores { 80.0 } else { 15.0 };
    s += b.score * score_weight;

    s += (b.source.len().min(8_000) as f64) * 0.02;

    let src = b.source.as_str();
    if src.contains("pub ") || src.contains("pub(") || src.contains("export ") {
        s += 80.0;
    }

    if is_testish_seed_block(b) {
        s -= 1_500.0;
    }

    s
}

/// Ranked interior pool (best first). Empty if nothing usable.
pub fn rank_module_interior<'a>(
    query: &str,
    interior: Vec<&'a BlockInfo>,
    use_neural_scores: bool,
) -> Vec<&'a BlockInfo> {
    let mut pool = filter_seed_candidates(interior);
    if pool.is_empty() {
        return pool;
    }
    pool.sort_by(|a, b| {
        module_interior_rank_score(query, b, use_neural_scores)
            .partial_cmp(&module_interior_rank_score(query, a, use_neural_scores))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    pool
}

/// Outcome of opening a module shell into its file / children.
pub struct ModuleResolveResult<'a> {
    pub seed: &'a BlockInfo,
    pub from_mod: String,
    /// Winner first, up to 3 — for dense notes (all relevant, only a few).
    pub top_candidates: Vec<&'a BlockInfo>,
}

/// Public helper: top-k candidate DTOs for structured / dense content.
pub fn module_interior_candidate_dtos(
    query: &str,
    blocks: &[&BlockInfo],
    use_neural_scores: bool,
    k: usize,
) -> Vec<crate::server::dto::ModuleInteriorCandidate> {
    blocks
        .iter()
        .take(k)
        .map(|b| crate::server::dto::ModuleInteriorCandidate {
            name: b.name.clone(),
            file: b.file.to_string_lossy().to_string(),
            line: b.start_line,
            kind: b.kind.clone(),
            rank_score: module_interior_rank_score(query, b, use_neural_scores),
        })
        .collect()
}

/// Full resolve with top-3 candidates for dense reporting.
/// Returns None if no better interior found (caller keeps shell).
pub fn resolve_module_shell_detailed<'a>(
    graph: &'a CodeGraph,
    seed: &'a BlockInfo,
    scoped: &[&'a BlockInfo],
    query: &str,
    use_neural_scores: bool,
) -> Option<ModuleResolveResult<'a>> {
    if !is_module_shell(seed) {
        return None;
    }
    let interior = collect_module_interior(graph, seed, scoped);
    if interior.is_empty() {
        return None;
    }
    let ranked = rank_module_interior(query, interior, use_neural_scores);
    if ranked.is_empty() {
        return None;
    }
    let best = ranked[0];
    if best.id == seed.id {
        return None;
    }
    if seed_role_tier(&best.kind) < 10 {
        return None;
    }
    let top: Vec<&BlockInfo> = ranked.into_iter().take(3).collect();
    Some(ModuleResolveResult {
        seed: top[0],
        from_mod: seed.name.clone(),
        top_candidates: top,
    })
}
