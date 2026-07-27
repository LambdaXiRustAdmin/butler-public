//! Trace seed resolution cascade (exact → fuzzy → module shell).
//!
//! Extracted from `handle_orchestrate` (M1b). Strategies are ordered; later ones only
//! run when earlier return None. **Never** let fuzzy steal when `blocks_for_name` hit.

use code_graph::{BlockInfo, CodeGraph, NeuralSelectionBlend};
use std::collections::HashSet;
use std::path::Path;

/// Outcome of seed selection + optional module-shell open.
pub(crate) struct SeedResolution<'a> {
    pub target: Option<&'a BlockInfo>,
    pub module_resolved_from: Option<String>,
    pub module_interior_candidates: Option<Vec<crate::server::dto::ModuleInteriorCandidate>>,
}

/// Floor for "definition-ish" (impl / method / fn / type). Calls sit at [`TIER_CALL`].
///
/// Prefer-def and out-of-scope lift use this so `impl_item` (70) beats `call_expression` (10)
/// and pure call-only graphs do not win a high-trust ★.
fn def_floor() -> i32 {
    crate::server::filters::TIER_IMPL
}

fn best_tier(matches: &[&BlockInfo]) -> i32 {
    use crate::server::filters::seed_role_tier;
    matches
        .iter()
        .map(|b| seed_role_tier(&b.kind))
        .max()
        .unwrap_or(0)
}

/// When any impl/fn/type def is present, drop call/use sites so ★ is never call_expression.
fn prefer_definition_seeds<'a>(matches: Vec<&'a BlockInfo>) -> Vec<&'a BlockInfo> {
    use crate::server::filters::seed_role_tier;
    let floor = def_floor();
    let has_def = matches.iter().any(|b| seed_role_tier(&b.kind) >= floor);
    if !has_def {
        return matches;
    }
    matches
        .into_iter()
        .filter(|b| seed_role_tier(&b.kind) >= floor)
        .collect()
}

/// Scope can hide the real def (e.g. `src/` while def lives under `platform-native/`).
///
/// **I8:** dir/blank scope + empty or pure `call_expression` → lift global defs.
/// **I9:** never lift past let/mod noise; never lift out of an **explicit file pin**
/// (disambiguate agents pin a location file — call-only there must miss, not steal ★).
fn lift_defs_outside_scope<'a>(
    graph: &'a CodeGraph,
    symbol: &str,
    in_scope_matches: Vec<&'a BlockInfo>,
    file_pin: bool,
) -> Vec<&'a BlockInfo> {
    use crate::server::filters::{seed_role_tier, TIER_CALL};
    let floor = def_floor();
    if best_tier(&in_scope_matches) >= floor {
        return prefer_definition_seeds(in_scope_matches);
    }
    // Explicit file pin: stay in file (I9 pin honesty). Miss if only calls/noise.
    if file_pin {
        return prefer_definition_seeds(in_scope_matches);
    }
    // Dir/blank: pure call sites (or empty) → lift. Let/mod leftovers stay put.
    let allow_lift = in_scope_matches.is_empty()
        || in_scope_matches
            .iter()
            .all(|b| seed_role_tier(&b.kind) == TIER_CALL);
    if allow_lift {
        let global_defs: Vec<&BlockInfo> = graph
            .blocks_for_name(symbol)
            .into_iter()
            .filter(|b| seed_role_tier(&b.kind) >= floor)
            .collect();
        if !global_defs.is_empty() {
            return prefer_definition_seeds(global_defs);
        }
    }
    prefer_definition_seeds(in_scope_matches)
}

fn is_explicit_file_scope(scope_paths: &Option<Vec<String>>) -> bool {
    let Some(paths) = scope_paths.as_ref() else {
        return false;
    };
    paths.iter().any(|s| looks_like_source_file_pin(s))
}

fn looks_like_source_file_pin(s: &str) -> bool {
    let t = s.trim().trim_end_matches('/');
    t.contains('.')
        && (t.ends_with(".rs")
            || t.ends_with(".ts")
            || t.ends_with(".tsx")
            || t.ends_with(".js")
            || t.ends_with(".jsx")
            || t.ends_with(".py")
            || t.ends_with(".go")
            || t.ends_with(".c")
            || t.ends_with(".h")
            || t.ends_with(".cpp")
            || t.ends_with(".hpp")
            || t.ends_with(".svelte")
            || t.ends_with(".cc")
            || t.ends_with(".cxx")
            || t.ends_with(".hxx"))
}

/// When scope is an explicit file pin, keep only defs in those files (I9).
fn prefer_matches_in_file_pins<'a>(
    matches: Vec<&'a BlockInfo>,
    scope_paths: &Option<Vec<String>>,
) -> Vec<&'a BlockInfo> {
    let Some(paths) = scope_paths.as_ref() else {
        return matches;
    };
    let pins: Vec<String> = paths
        .iter()
        .map(|s| s.trim().trim_end_matches('/').replace('\\', "/"))
        .filter(|s| looks_like_source_file_pin(s))
        .collect();
    if pins.is_empty() {
        return matches;
    }
    let pinned: Vec<&BlockInfo> = matches
        .iter()
        .copied()
        .filter(|b| {
            let f = b.file.to_string_lossy().replace('\\', "/");
            pins.iter().any(|p| {
                f == *p
                    || f.ends_with(&format!("/{p}"))
                    || f.ends_with(p.as_str())
                    || {
                        // basename pin
                        !p.contains('/')
                            && f.rsplit('/').next().is_some_and(|base| base == p.as_str())
                    }
            })
        })
        .collect();
    if pinned.is_empty() {
        matches
    } else {
        pinned
    }
}

/// Pure call/use sites are not Trace ★ seeds (external, builtin, printf params, …).
fn strip_call_only_seeds<'a>(matches: Vec<&'a BlockInfo>) -> Vec<&'a BlockInfo> {
    use crate::server::filters::{seed_role_tier, TIER_CALL};
    if best_tier(&matches) <= TIER_CALL {
        return Vec::new();
    }
    // Defensive: drop any residual call-tier peers when a higher tier exists.
    let floor = def_floor();
    if best_tier(&matches) >= floor {
        return matches
            .into_iter()
            .filter(|b| seed_role_tier(&b.kind) >= floor)
            .collect();
    }
    matches
}

/// Prefer package public surfaces when multi-def: `__init__.py`, `mod.rs`, `lib.rs`.
fn is_public_surface_path(file: &std::path::Path) -> bool {
    let s = file.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    s.ends_with("/__init__.py")
        || s.ends_with("/mod.rs")
        || s.ends_with("/lib.rs")
        || s.ends_with("/index.ts")
        || s.ends_with("/index.js")
}

fn pick_from_matches<'a>(
    graph: &'a CodeGraph,
    root_path: &Path,
    matches: Vec<&'a BlockInfo>,
) -> Option<&'a BlockInfo> {
    let matches = strip_call_only_seeds(prefer_definition_seeds(matches));
    if matches.is_empty() {
        return None;
    }
    let entry_first: Vec<&BlockInfo> = matches
        .iter()
        .copied()
        .filter(|b| crate::server::filters::is_entry_landmark(b, root_path))
        .collect();
    let entry_first = strip_call_only_seeds(prefer_definition_seeds(entry_first));
    if !entry_first.is_empty() {
        return crate::server::filters::pick_best_homonym_on_graph(graph, entry_first);
    }
    // P1 light: package-root public surface before buried impls (Python re-export / crate root).
    let surface: Vec<&BlockInfo> = matches
        .iter()
        .copied()
        .filter(|b| is_public_surface_path(&b.file))
        .collect();
    if surface.len() == 1 || (surface.len() > 1 && surface.len() < matches.len()) {
        // Prefer surface when it narrows; if all are surface, fall through to full set.
        if !surface.is_empty() && surface.len() < matches.len() {
            return crate::server::filters::pick_best_homonym_on_graph(graph, surface);
        }
    }
    crate::server::filters::pick_best_homonym_on_graph(graph, matches)
}

/// Resolve Trace/Find seed for `symbol` within optional scope.
///
/// Order: qualified / exact name (+ entry landmark preference + homonym) → fuzzy only when
/// name index has no exact hit → module shell open (may change Ident intentionally).
///
/// **I8 policy:** never ★ on `call_expression` when a def exists (in or out of scope);
/// never ★ on call-only name hits (builtin/external/use sites) — fall through to miss.
pub(crate) fn resolve_trace_seed<'a>(
    graph: &'a CodeGraph,
    scoped: &[&'a BlockInfo],
    symbol: &str,
    root_path: &Path,
    scope_paths: &Option<Vec<String>>,
    ignore_paths: &Option<Vec<String>>,
    use_neural_scores: bool,
    blend: NeuralSelectionBlend,
) -> SeedResolution<'a> {
    let scope_ids: HashSet<code_graph::Id> =
        scoped.iter().map(|b| b.id.clone()).collect();
    let in_scope = |b: &BlockInfo| -> bool {
        scope_ids.is_empty() || scope_ids.contains(&b.id)
    };
    let exact_match = if symbol.contains("::") {
        crate::server::filters::seed_qualified_symbol_in(
            graph,
            scoped,
            symbol.trim(),
            Some(root_path),
        )
        .filter(|b| {
            use crate::server::filters::{seed_role_tier, TIER_CALL};
            seed_role_tier(&b.kind) > TIER_CALL
        })
    } else {
        let matches: Vec<&BlockInfo> = graph
            .blocks_for_name(symbol)
            .into_iter()
            .filter(|b| in_scope(b))
            .collect();
        // Prefer defs; dir/blank may lift pure-call scopes (I8). File pins never lift (I9).
        let file_pin = is_explicit_file_scope(scope_paths);
        let matches = prefer_matches_in_file_pins(matches, scope_paths);
        let matches = lift_defs_outside_scope(graph, symbol, matches, file_pin);
        pick_from_matches(graph, root_path, matches)
    };
    // P1: Fuzzy select_blocks **only** when the name index has no exact hit.
    // Never let substring/heuristic blend steal ★ when blocks_for_name had matches
    // (NS_GetMainThread → thread/main bleed on mega-C++).
    let seed_final = exact_match.or_else(|| {
        // Qualified names only resolve via seed_qualified_symbol.
        if symbol.contains("::") {
            return None;
        }
        let index_warm = !graph.name_index.is_empty();
        let exact_locations = graph.locations_for_name(symbol);
        if index_warm && !exact_locations.is_empty() {
            // Exact names exist but were filtered out of scope / noise / call-only strip —
            // do not invent a different symbol via fuzzy.
            return None;
        }
        if index_warm && exact_locations.is_empty() {
            // Warm index, no exact name — fuzzy for typos / prose only.
        }
        let search_results = crate::server::context_engine::select_blocks(
            graph,
            symbol,
            use_neural_scores,
            blend,
        );
        // Keep only candidates whose **name matches the query Ident**.
        let filtered = code_graph::snooper::filter_blocks_by_scope(
            &search_results,
            scope_paths,
            ignore_paths,
        );
        let candidates: Vec<&BlockInfo> = filtered
            .iter()
            .filter_map(|b| graph.get_block(b.id.clone()))
            .filter(|b| code_graph::seed_name_matches_query(symbol.trim(), &b.name))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let file_pin = is_explicit_file_scope(scope_paths);
        let candidates = lift_defs_outside_scope(graph, symbol, candidates, file_pin);
        pick_from_matches(graph, root_path, candidates)
    });
    // Belt: Ident integrity + never call_expression ★.
    let seed_final = seed_final.filter(|b| {
        use crate::server::filters::{seed_role_tier, TIER_CALL};
        code_graph::seed_name_matches_query(symbol.trim(), &b.name)
            && seed_role_tier(&b.kind) > TIER_CALL
    });

    // Module shell → open the door (path/AST), rank interior (heuristic + score).
    // **I9 file pin:** never open sibling modules (mod.rs pin must not ★ search.rs).
    let file_pin = is_explicit_file_scope(scope_paths);
    let mut module_resolved_from: Option<String> = None;
    let mut module_interior_candidates: Option<
        Vec<crate::server::dto::ModuleInteriorCandidate>,
    > = None;
    let target = seed_final.and_then(|seed| {
        if file_pin {
            return Some(seed);
        }
        if let Some(res) = crate::server::filters::resolve_module_shell_detailed(
            graph,
            seed,
            scoped,
            symbol.trim(),
            use_neural_scores,
        ) {
            module_resolved_from = Some(res.from_mod);
            module_interior_candidates = Some(
                crate::server::filters::module_interior_candidate_dtos(
                    symbol.trim(),
                    &res.top_candidates,
                    use_neural_scores,
                    3,
                ),
            );
            // Interior open intentionally changes Ident (walk → Batch).
            // Still refuse call_expression interiors.
            use crate::server::filters::{seed_role_tier, TIER_CALL};
            if seed_role_tier(&res.seed.kind) > TIER_CALL {
                Some(res.seed)
            } else {
                None
            }
        } else if code_graph::seed_name_matches_query(symbol.trim(), &seed.name) {
            Some(seed)
        } else {
            // Refuse ★ on a different Ident (fuzzy / bleed).
            None
        }
    });

    SeedResolution {
        target,
        module_resolved_from,
        module_interior_candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::filters::{seed_role_tier, TIER_CALL, TIER_FUNCTION, TIER_IMPL, TIER_NEVER};

    #[test]
    fn file_pin_detects_source_file_scopes() {
        let scope = Some(vec!["src/panes/mod.rs".into()]);
        assert!(is_explicit_file_scope(&scope));
        assert!(looks_like_source_file_pin("src/panes/mod.rs"));
        assert!(looks_like_source_file_pin("format-inl.h"));
        assert!(!looks_like_source_file_pin("src/panes/"));
        assert!(!is_explicit_file_scope(&Some(vec!["src/panes/".into()])));
    }


    #[test]
    fn def_floor_is_impl_tier() {
        assert_eq!(def_floor(), TIER_IMPL);
        assert!(TIER_FUNCTION > def_floor());
        assert!(TIER_CALL < def_floor());
    }

    #[test]
    fn seed_role_call_below_floor() {
        assert!(seed_role_tier("call_expression") <= TIER_CALL);
        assert!(seed_role_tier("function_item") >= def_floor());
        assert!(seed_role_tier("impl_item") >= def_floor());
    }

    #[test]
    fn let_declaration_is_never_tier_not_call() {
        // I9: lets must not count as pure-call (which would still allow out-of-scope lift).
        assert_eq!(seed_role_tier("let_declaration"), TIER_NEVER);
        assert_ne!(seed_role_tier("let_declaration"), TIER_CALL);
    }
}
