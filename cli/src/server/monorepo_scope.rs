//! Blank Arch scope from generalist repo shape (code_graph::repo_shape).
//!
//! Layout policy only — language experts stay in lang/*. Fail open on flat/unknown
//! **for Trace** (surgical name path). ArchitecturalSummary on Unknown/flat leviathans
//! gets **suggest-only** top-level dirs (never auto-applied as fat scope_paths).

use code_graph::{
    classify_from_rel_paths, CodeGraph, ParsePlan, ProjectPaths, RepoShapeKind,
};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct MonorepoScopePlan {
    pub scopes: Vec<String>,
    pub spine_first: bool,
    pub reason: String,
    pub fail_open: bool,
    /// Top-level dirs for agent guidance when we refuse auto-cage (Arch on leviathans).
    pub agent_suggestions: Vec<String>,
}

fn is_noise_scope_seg(seg: &str) -> bool {
    let low = seg.to_ascii_lowercase();
    matches!(
        low.as_str(),
        "tests"
            | "test"
            | "docs"
            | "doc"
            | "examples"
            | "example"
            | "fixtures"
            | "third_party"
            | "third-party"
            | "vendor"
            | "node_modules"
            | "obj-x86_64-pc-linux-gnu"
            | "objdir"
            | "target"
            | "__pycache__"
            | "build"
            | "dist"
    )
}

/// Top-level directory prefixes by file density (O(files), not O(nodes)).
/// Skips root files (`foo.c`) and common noise dirs.
///
/// **Package spine (L4.1):** when one top-level dir holds most files (e.g. `django/`),
/// also emit its densest children (`django/db/`, `django/contrib/`) so Arch refuse
/// is agent-actionable on monorepos that are not multi-crate at root.
pub fn top_level_dir_scopes(rels: &[String], max: usize) -> Vec<String> {
    if max == 0 {
        return vec![];
    }
    let mut top_counts: HashMap<String, usize> = HashMap::new();
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    let mut total = 0usize;
    for r in rels {
        let r = r.trim_start_matches("./").trim_start_matches('/');
        let mut parts = r.split('/');
        let Some(seg) = parts.next() else {
            continue;
        };
        if seg.is_empty() || seg.starts_with('.') {
            continue;
        }
        // Root-level file (tmux-style) — not a scope dir.
        if seg.contains('.') {
            continue;
        }
        if is_noise_scope_seg(seg) {
            continue;
        }
        total += 1;
        let top = format!("{seg}/");
        *top_counts.entry(top.clone()).or_insert(0) += 1;
        if let Some(child) = parts.next() {
            if !child.is_empty() && !child.contains('.') && !is_noise_scope_seg(child) {
                *child_counts
                    .entry(format!("{seg}/{child}/"))
                    .or_insert(0) += 1;
            }
        }
    }
    let mut ranked: Vec<(String, usize)> = top_counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut out: Vec<String> = Vec::new();
    // Dominant single package → prefer package spine children first.
    if let Some((dom, n)) = ranked.first() {
        let frac = if total == 0 {
            0.0
        } else {
            *n as f64 / total as f64
        };
        if frac >= 0.55 && *n >= 20 {
            let prefix = dom.as_str();
            let mut kids: Vec<(String, usize)> = child_counts
                .iter()
                .filter(|(k, _)| k.starts_with(prefix))
                .map(|(k, v)| (k.clone(), *v))
                .collect();
            kids.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            for (k, _) in kids.into_iter().take(max.saturating_sub(1).max(1)) {
                if !out.contains(&k) {
                    out.push(k);
                }
                if out.len() >= max {
                    break;
                }
            }
            // Always keep the dominant top as a last-resort wider scope.
            if out.len() < max && !out.contains(dom) {
                out.push(dom.clone());
            }
        }
    }
    for (s, _) in ranked {
        if out.len() >= max {
            break;
        }
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out.truncate(max);
    out
}

/// Compute default scope_paths for a large graph with blank agent scope.
pub fn plan_monorepo_scopes(
    graph: &CodeGraph,
    project_root: &Path,
    want_arch_spine: bool,
) -> Option<MonorepoScopePlan> {
    const MONOREPO_BLOCK_THRESHOLD: usize = 10_000;
    if graph.nodes.len() < MONOREPO_BLOCK_THRESHOLD {
        return None;
    }

    let pp = ProjectPaths::new(project_root);
    // Path inventory from warehouse keys (files we know about).
    let mut rels: Vec<String> = if !graph.file_hashes.is_empty() {
        graph.file_hashes.keys().cloned().collect()
    } else {
        graph
            .nodes
            .values()
            .map(|b| pp.key(&b.file))
            .collect()
    };
    rels.sort();
    rels.dedup();
    if rels.is_empty() {
        return None;
    }

    let plan: ParsePlan = classify_from_rel_paths(project_root, &rels);
    let mut out = shape_to_scope_plan(plan, want_arch_spine)?;

    // Arch + fail-open (gecko Unknown): **suggest-only** top-level dirs.
    // Do NOT auto-apply 8 fat prefixes as scope_paths — that was still 3.4M nodes / 78s materialize.
    if out.fail_open && want_arch_spine {
        let tops = top_level_dir_scopes(&rels, 12);
        if !tops.is_empty() {
            out.agent_suggestions = tops;
            out.reason = format!("{} + arch suggest-only (no auto cage)", out.reason);
        }
    }
    Some(out)
}

fn shape_to_scope_plan(plan: ParsePlan, want_arch_spine: bool) -> Option<MonorepoScopePlan> {
    if plan.fail_open_scope || !want_arch_spine {
        // Fail open: no cage. (Non-Arch goals also skip tight spine.)
        if plan.fail_open_scope || plan.suggested_scopes.is_empty() {
            return Some(MonorepoScopePlan {
                scopes: vec![],
                spine_first: false,
                fail_open: true,
                reason: format!("{:?} {}", plan.shape, plan.reason),
                agent_suggestions: vec![],
            });
        }
    }

    if !plan.suggested_scopes.is_empty() && want_arch_spine {
        let spine_first = matches!(
            plan.shape,
            RepoShapeKind::NamedMultiPackage
                | RepoShapeKind::CliPipeline
                | RepoShapeKind::NestedAppMonorepo
        );
        return Some(MonorepoScopePlan {
            scopes: plan.suggested_scopes,
            spine_first,
            fail_open: false,
            reason: format!("{:?} {}", plan.shape, plan.reason),
            agent_suggestions: vec![],
        });
    }

    Some(MonorepoScopePlan {
        scopes: vec![],
        spine_first: false,
        fail_open: true,
        reason: format!("{:?} fallback fail-open {}", plan.shape, plan.reason),
        agent_suggestions: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph::snooper::model::{BlockInfo, CodeGraph, Id};
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn project_name_stems(project_root: &Path) -> Vec<String> {
        let base = project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut stems = vec![base.clone()];
        if let Some(head) = base.split(['-', '_']).next() {
            if head.len() >= 3 {
                stems.push(head.to_string());
            }
        }
        stems
    }

    fn blk(file: &str, name: &str, uniq: usize) -> BlockInfo {
        let hash = format!("{:08x}{:08x}", uniq as u32, (uniq as u32).wrapping_mul(0x9e37));
        BlockInfo {
            id: Id::new(file, "function_item", &hash),
            name: name.into(),
            file: PathBuf::from(file),
            kind: "function_item".into(),
            lang: "rust".into(),
            start_line: 1,
            end_line: 2,
            start_byte: 0,
            end_byte: 1,
            parent_id: None,
            children: vec![],
            content_hash: hash,
            sig_hash: "s".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            score: 0.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        }
    }

    fn add_many(g: &mut CodeGraph, file: &str, n: usize) {
        let base = g.nodes.len();
        for i in 0..n {
            // Distinct paths so L0 inventory (file_hashes) sees package density.
            let f = if n == 1 {
                file.to_string()
            } else {
                let parent = Path::new(file).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                let stem = Path::new(file).file_stem().and_then(|s| s.to_str()).unwrap_or("f");
                let ext = Path::new(file).extension().and_then(|s| s.to_str()).unwrap_or("rs");
                if parent.is_empty() {
                    format!("{stem}_{i}.{ext}")
                } else {
                    format!("{parent}/{stem}_{i}.{ext}")
                }
            };
            let b = blk(&f, &format!("fn_{i}"), base + i);
            g.file_hashes.insert(f, 1);
            g.nodes.insert(b.id.clone(), b);
        }
    }

    #[test]
    fn stems_from_uniffi_rs() {
        let s = project_name_stems(Path::new("/projects/test_repos/uniffi-rs"));
        assert!(s.iter().any(|x| x == "uniffi"));
    }

    #[test]
    fn uniffi_shaped_graph_prefers_name_packages() {
        let mut g = CodeGraph::new();
        for f in [
            "uniffi_bindgen/src/lib.rs",
            "uniffi_core/src/lib.rs",
            "uniffi/src/lib.rs",
        ] {
            add_many(&mut g, f, 50);
        }
        add_many(&mut g, "fixtures/foo/src/lib.rs", 200);
        add_many(&mut g, "uniffi_meta/src/lib.rs", 10_000);
        let plan = plan_monorepo_scopes(&g, Path::new("/tmp/uniffi-rs"), true).expect("plan");
        assert!(!plan.fail_open, "{:?}", plan);
        assert!(plan.spine_first, "{:?}", plan);
        let joined = plan.scopes.join(" ");
        assert!(joined.contains("uniffi"), "{:?}", plan.scopes);
    }

    #[test]
    fn emscripten_shaped_root_cli_and_tools() {
        let mut g = CodeGraph::new();
        add_many(&mut g, "emcc.py", 5);
        add_many(&mut g, "tools/link.py", 80);
        add_many(&mut g, "tools/building.py", 40);
        add_many(&mut g, "src/lib/libwebgl.js", 500);
        add_many(&mut g, "tools/emscripten.py", 10_000);
        let plan = plan_monorepo_scopes(&g, Path::new("/tmp/emscripten"), true).expect("plan");
        assert!(!plan.fail_open, "{:?}", plan);
        assert!(plan.scopes.iter().any(|s| s.contains("tools") || s.contains("emcc")), "{:?}", plan);
    }

    #[test]
    fn tmux_flat_c_fail_open() {
        let mut g = CodeGraph::new();
        for f in [
            "server.c", "client.c", "session.c", "window.c", "cmd.c",
            "cmd-new-session.c", "tmux.c", "tmux.h", "grid.c", "input.c", "options.c",
        ] {
            add_many(&mut g, f, 1000);
        }
        let plan = plan_monorepo_scopes(&g, Path::new("/tmp/tmux"), true).expect("plan");
        // Flat root .c files: no top-level *dirs* → still fail-open (no fake cage).
        assert!(plan.fail_open, "{:?}", plan);
        assert!(plan.scopes.is_empty(), "{:?}", plan.scopes);
    }

    #[test]
    fn arch_unknown_leviathan_suggest_only_no_auto_cage() {
        let mut g = CodeGraph::new();
        // Gecko-shaped: many top-level product dirs, no cargo monorepo markers.
        for (dir, n) in [
            ("xpcom/base/nsCOMPtr.h", 4000),
            ("js/src/vm/JSContext.cpp", 5000),
            ("dom/base/nsINode.cpp", 3000),
            ("layout/base/nsCSSFrameConstructor.cpp", 2000),
        ] {
            add_many(&mut g, dir, n);
        }
        let plan = plan_monorepo_scopes(&g, Path::new("/tmp/gecko-dev"), true).expect("plan");
        // Suggest-only: fail_open + empty scopes (never auto-apply fat tops as materialize set).
        assert!(plan.fail_open, "{:?}", plan);
        assert!(plan.scopes.is_empty(), "{:?}", plan.scopes);
        let joined = plan.agent_suggestions.join(" ");
        assert!(
            joined.contains("xpcom") || joined.contains("js") || joined.contains("dom"),
            "suggestions={:?}",
            plan.agent_suggestions
        );
    }

    #[test]
    fn top_level_dir_scopes_skips_root_files_and_noise() {
        let rels = vec![
            "xpcom/base/a.h".into(),
            "xpcom/base/b.h".into(),
            "js/src/x.cpp".into(),
            "tests/unit/t.cpp".into(),
            "server.c".into(),
        ];
        let s = top_level_dir_scopes(&rels, 8);
        assert!(s.iter().any(|x| x == "xpcom/"), "{:?}", s);
        assert!(s.iter().any(|x| x == "js/"), "{:?}", s);
        assert!(!s.iter().any(|x| x.starts_with("tests")), "{:?}", s);
        assert!(!s.iter().any(|x| x.contains("server")), "{:?}", s);
    }

    #[test]
    fn django_shaped_package_spine_suggests_children() {
        // L4.1: single dominant package → child spines for Arch refuse.
        let mut rels = Vec::new();
        for i in 0..80 {
            rels.push(format!("django/db/models/m{i}.py"));
        }
        for i in 0..40 {
            rels.push(format!("django/contrib/admin/a{i}.py"));
        }
        for i in 0..25 {
            rels.push(format!("django/http/h{i}.py"));
        }
        let s = top_level_dir_scopes(&rels, 8);
        assert!(
            s.iter().any(|x| x.starts_with("django/db")),
            "expected django/db/* spine, got {s:?}"
        );
        assert!(
            s.iter().any(|x| x.starts_with("django/contrib") || x.starts_with("django/http")),
            "expected other django/* children, got {s:?}"
        );
        // Dominant top still available as wider fallback.
        assert!(s.iter().any(|x| x == "django/"), "{s:?}");
    }
}
