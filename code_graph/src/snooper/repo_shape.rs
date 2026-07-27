//! Generalist repo **shape** from path inventory (L0).
//!
//! Drives parse order and blank-Arch scope suggestions without language experts.
//! Experts (lang/*) only interpret files once the plan says "parse this path."

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::project_paths::ProjectPaths;
use super::utils::normalize_path;

/// High-level layout archetype (not a programming language).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoShapeKind {
    /// Most sources at repo root (tmux, redis-shaped).
    FlatSinglePackage,
    /// Top-level packages aligned with project name (uniffi_*, crates workspace).
    NamedMultiPackage,
    /// Root entry scripts + tools/cmd pipeline (emscripten-shaped).
    CliPipeline,
    /// Nested apps/packages/src monorepo.
    NestedAppMonorepo,
    /// No confident read — fail open.
    Unknown,
}

/// Schedule for progressive parse + hints for blank Arch.
#[derive(Debug, Clone)]
pub struct ParsePlan {
    pub shape: RepoShapeKind,
    /// Lower priority number = parse earlier. Matched as path prefix/contains.
    pub priority_prefixes: Vec<String>,
    /// Parse later (noise / peripheral).
    pub defer_prefixes: Vec<String>,
    /// Blank Arch should not set tight scope_paths.
    pub fail_open_scope: bool,
    /// Suggested scope_paths when spine is confident (dirs end with `/`, files bare).
    pub suggested_scopes: Vec<String>,
    pub reason: String,
}

const SOURCE_EXTS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "c", "h", "cpp", "hpp", "cc", "cxx", "m", "swift",
    "kt", "java", "rb",
];

/// Non-vendor noise tops for shape classification. Bundled-vendor segments come
/// from [`super::path_policy::is_bundled_vendor_dir_segment`] (single source).
const NOISE_TOP: &[&str] = &[
    "test",
    "tests",
    "testing",
    "fixtures",
    "examples",
    "example",
    "benches",
    "bench",
    "docs",
    "doc",
    "docs_src",
    "site",
    "target",
    "bindgen-tests",
    "samples",
    "demo",
    "demos",
    "regress",
    "fuzz",
    "logo",
    "presentations",
];

const PIPELINE: &[&str] = &["tools", "cmd", "bin", "scripts", "cli"];

const ENTRY_FILES: &[&str] = &[
    "main.py",
    "main.rs",
    "main.go",
    "main.c",
    "main.cpp",
    "cli.py",
    "cli.rs",
    "app.py",
    "server.py",
    "index.ts",
    "index.js",
    "emcc.py",
    "em++.py",
    "mod.rs",
    "lib.rs",
    "tmux.c",
];

/// Classify shape from **repo-relative** source paths (L0 inventory).
pub fn classify_from_rel_paths(project_root: &Path, rel_paths: &[String]) -> ParsePlan {
    let stems = project_name_stems(project_root);
    let total = rel_paths.len().max(1);

    let mut root_files = 0usize;
    let mut nested_under_dir = 0usize;
    let mut top_dir_files: HashMap<String, usize> = HashMap::new();
    let mut pipeline_files: HashMap<String, usize> = HashMap::new();
    let mut root_entries: Vec<String> = Vec::new();

    for raw in rel_paths {
        let rel = normalize_path(raw);
        let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        if segs.is_empty() {
            continue;
        }
        let top = segs[0];

        if segs.len() == 1 && looks_like_source_file(top) {
            root_files += 1;
            if ENTRY_FILES.iter().any(|e| e.eq_ignore_ascii_case(top)) {
                if !root_entries.iter().any(|e| e == top) {
                    root_entries.push(top.to_string());
                }
            }
        } else if segs.len() >= 2 && !looks_like_source_file(top) && !is_noise(top) {
            nested_under_dir += 1;
            *top_dir_files.entry(top.to_string()).or_default() += 1;
            if PIPELINE.contains(&top) {
                *pipeline_files.entry(top.to_string()).or_default() += 1;
            }
        }
    }

    let flat_ratio = root_files as f64 / total as f64;
    let is_flat = flat_ratio >= 0.45 && nested_under_dir < root_files / 2;

    let defer = default_defer_prefixes();

    // --- Flat ---
    if is_flat {
        return ParsePlan {
            shape: RepoShapeKind::FlatSinglePackage,
            priority_prefixes: vec![], // all product equal; only defer noise
            defer_prefixes: defer,
            fail_open_scope: true,
            suggested_scopes: vec![],
            reason: format!(
                "flat root_files={root_files}/{total} nested_dir={nested_under_dir}"
            ),
        };
    }

    // Name-aligned top dirs
    let mut name_dirs: Vec<(String, usize)> = top_dir_files
        .iter()
        .filter(|(n, c)| **c >= 5 && name_affinity(n, &stems))
        .map(|(n, c)| (n.clone(), *c))
        .collect();
    name_dirs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let pipeline_hits: Vec<(String, usize)> = pipeline_files
        .into_iter()
        .filter(|(_, c)| *c >= 8)
        .collect();

    let has_cli = !root_entries.is_empty() && !pipeline_hits.is_empty();
    let has_named = !name_dirs.is_empty();
    let has_nested_markers = top_dir_files
        .keys()
        .any(|k| matches!(k.as_str(), "src" | "crates" | "packages" | "apps" | "lib" | "libs"));

    // --- CLI + pipeline ---
    if has_cli {
        let mut pri = root_entries.clone();
        for (d, _) in &pipeline_hits {
            pri.push(format!("{d}/"));
        }
        // soft: name-aligned if any
        for (d, _) in name_dirs.iter().take(6) {
            pri.push(format!("{d}/"));
        }
        let scopes = pri.clone();
        return ParsePlan {
            shape: RepoShapeKind::CliPipeline,
            priority_prefixes: pri,
            defer_prefixes: defer,
            fail_open_scope: false,
            suggested_scopes: scopes,
            reason: format!(
                "cli+pipeline entries={:?} pipeline={:?}",
                root_entries,
                pipeline_hits.iter().map(|(d, _)| d.clone()).collect::<Vec<_>>()
            ),
        };
    }

    // --- Named multi-package ---
    if has_named {
        let mut pri: Vec<String> = name_dirs
            .iter()
            .take(12)
            .map(|(d, _)| format!("{d}/"))
            .collect();
        for (d, _) in &pipeline_hits {
            pri.push(format!("{d}/"));
        }
        return ParsePlan {
            shape: RepoShapeKind::NamedMultiPackage,
            priority_prefixes: pri.clone(),
            defer_prefixes: defer,
            fail_open_scope: false,
            suggested_scopes: pri,
            reason: format!("named packages={:?}", name_dirs.iter().take(8).collect::<Vec<_>>()),
        };
    }

    // --- Nested app monorepo ---
    if has_nested_markers {
        let mut pri = vec![
            "src/".into(),
            "crates/".into(),
            "packages/".into(),
            "lib/".into(),
            "apps/".into(),
        ];
        for (d, _) in &pipeline_hits {
            pri.push(format!("{d}/"));
        }
        return ParsePlan {
            shape: RepoShapeKind::NestedAppMonorepo,
            priority_prefixes: pri.clone(),
            defer_prefixes: defer,
            fail_open_scope: false,
            suggested_scopes: pri,
            reason: "nested app markers".into(),
        };
    }

    // --- Unknown: mild defaults, fail open on scope ---
    ParsePlan {
        shape: RepoShapeKind::Unknown,
        priority_prefixes: vec!["src/".into(), "crates/".into(), "lib/".into(), "tools/".into()],
        defer_prefixes: defer,
        fail_open_scope: true,
        suggested_scopes: vec![],
        reason: "unknown fail-open".into(),
    }
}

/// Classify from absolute paths under `project_root`.
pub fn classify_from_abs_paths(project_root: &Path, abs_paths: &[PathBuf]) -> ParsePlan {
    let pp = ProjectPaths::new(project_root);
    let rels: Vec<String> = abs_paths.iter().map(|p| pp.key(p)).collect();
    classify_from_rel_paths(project_root, &rels)
}

/// Parse priority: 0 = first. Uses plan prefixes; lower is earlier.
pub fn path_priority_for_plan(plan: &ParsePlan, path: &Path) -> u8 {
    let s = normalize_path(&path.to_string_lossy()).to_ascii_lowercase();

    for (i, pref) in plan.defer_prefixes.iter().enumerate() {
        let p = pref.to_ascii_lowercase();
        if path_matches_prefix(&s, &p) {
            return 200u8.saturating_add(i as u8).min(250);
        }
    }
    for (i, pref) in plan.priority_prefixes.iter().enumerate() {
        let p = pref.to_ascii_lowercase();
        if path_matches_prefix(&s, &p) {
            return (i as u8).min(50);
        }
    }
    // Default middle band
    100
}

fn path_matches_prefix(path_lower: &str, pref_lower: &str) -> bool {
    let pref = pref_lower.trim_end_matches('/');
    if pref.is_empty() {
        return false;
    }
    // file entry (emcc.py)
    if !pref_lower.contains('/') && !pref_lower.ends_with('/') {
        if looks_like_source_file(pref) {
            return path_lower.ends_with(pref)
                || path_lower.ends_with(&format!("/{pref}"))
                || Path::new(path_lower)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(pref));
        }
    }
    path_lower.contains(&format!("/{pref}/"))
        || path_lower.starts_with(&format!("{pref}/"))
        || path_lower.contains(&format!("/{pref}"))
        || path_lower.starts_with(pref)
}

fn default_defer_prefixes() -> Vec<String> {
    NOISE_TOP
        .iter()
        .map(|n| format!("{n}/"))
        .chain(
            [
                "/test/",
                "/tests/",
                "/spec/",
                "/bench/",
                "/fixtures/",
                "/examples/",
                "/regress/",
                "/fuzz/",
            ]
            .iter()
            .map(|s| (*s).to_string()),
        )
        .collect()
}

fn looks_like_source_file(seg: &str) -> bool {
    let seg = seg.trim_end_matches('/');
    Path::new(seg)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SOURCE_EXTS.iter().any(|x| x.eq_ignore_ascii_case(ext)))
}

fn is_noise(seg: &str) -> bool {
    if super::path_policy::is_bundled_vendor_dir_segment(seg)
        || super::path_policy::is_infra_prune_dir_segment(seg)
    {
        return true;
    }
    let s = seg.to_ascii_lowercase();
    NOISE_TOP.iter().any(|n| *n == s)
}

fn project_name_stems(project_root: &Path) -> Vec<String> {
    let base = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if base.is_empty() {
        return vec![];
    }
    let mut stems = vec![base.clone()];
    let unders = base.replace('-', "_");
    if unders != base {
        stems.push(unders);
    }
    if let Some(head) = base.split(['-', '_']).next() {
        if head.len() >= 3 {
            stems.push(head.to_string());
        }
    }
    stems.sort();
    stems.dedup();
    stems
}

fn name_affinity(package: &str, stems: &[String]) -> bool {
    let p = package.to_ascii_lowercase();
    if looks_like_source_file(&p) {
        return false;
    }
    stems.iter().any(|s| {
        p == *s
            || p.starts_with(&format!("{s}_"))
            || p.starts_with(&format!("{s}-"))
            || (s.len() >= 4 && p.starts_with(s.as_str()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_tmux_shape() {
        let paths: Vec<String> = (0..50)
            .map(|i| format!("file{i}.c"))
            .chain([
                "server.c".into(),
                "client.c".into(),
                "tmux.c".into(),
                "tmux.h".into(),
                "cmd-new-session.c".into(),
            ])
            .collect();
        let plan = classify_from_rel_paths(Path::new("/tmp/tmux"), &paths);
        assert_eq!(plan.shape, RepoShapeKind::FlatSinglePackage);
        assert!(plan.fail_open_scope);
        assert!(plan.suggested_scopes.is_empty());
    }

    #[test]
    fn named_uniffi_shape() {
        let mut paths = vec![];
        for crate_ in ["uniffi_bindgen", "uniffi_core", "uniffi"] {
            for i in 0..20 {
                paths.push(format!("{crate_}/src/f{i}.rs"));
            }
        }
        for i in 0..30 {
            paths.push(format!("fixtures/x/src/t{i}.rs"));
        }
        let plan = classify_from_rel_paths(Path::new("/tmp/uniffi-rs"), &paths);
        assert_eq!(plan.shape, RepoShapeKind::NamedMultiPackage);
        assert!(!plan.fail_open_scope);
        assert!(plan.priority_prefixes.iter().any(|p| p.contains("uniffi")));
    }

    #[test]
    fn cli_pipeline_emscripten_shape() {
        let mut paths = vec!["emcc.py".into(), "em++.py".into()];
        for i in 0..30 {
            paths.push(format!("tools/mod{i}.py"));
        }
        for i in 0..40 {
            paths.push(format!("src/lib/web{i}.js"));
        }
        let plan = classify_from_rel_paths(Path::new("/tmp/emscripten"), &paths);
        assert_eq!(plan.shape, RepoShapeKind::CliPipeline);
        assert!(plan.priority_prefixes.iter().any(|p| p.contains("emcc") || p.contains("tools")));
    }
}
