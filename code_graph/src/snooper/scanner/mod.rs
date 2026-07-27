//! Workspace skeleton scan + public facade for load/save (Mixed-three **S1** ownership map).
//!
//! # Package roles (one concern per file — do not merge casually)
//!
//! | File | Owns | Does **not** own |
//! |------|------|------------------|
//! | **`mod.rs` (this file)** | Walk / skip / wave parse / build in-memory skeleton (`scan_workspace`, skip patterns, dual-stack path heuristics) | On-disk bincode layout; multi-bin stitch |
//! | **[`cache`]** | Graph cache load/save, hash-delta, schema version gates, incremental reparse on dirty files | FullEdge edge collect (builder); FS watch loop (watcher) |
//! | **[`shards`]** | Progressive multi-bin parts (inventory / symbols / edges), manifest, parallel hydrate stitch | Parse plan; Tree-sitter language drawers |
//!
//! # Soft-freeze (S1 = docs only; later peels must not casually bump)
//! - `GRAPH_SCHEMA_VERSION` / `EDGE_SEMANTICS_VERSION` / `CACHE_SCHEMA_VERSION` live in [`cache`]
//!   and are re-exported here for stable `scanner::*` call sites.
//! - Bumping versions invalidates warehouses — coordinate, document, do not renumber in peels.
//!
//! # Progressive multi-bin (inventory → symbols waves → edges deferred)
//! - jwalk parallel discovery + ignore / `.gitignore` prune
//! - priority parse (src/ first) so early publish is useful
//! - sources stripped after parse (hydrate on compose)
//!
//! # Public API stability
//! Re-exports from [`cache`] keep `snooper::scanner::load_graph` / `save_graph` / schema consts
//! stable for builder, watcher, and context_engine graph_admit.
//!
//! Zero intentional behavior change for S1 (header/ownership only).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use jwalk::WalkDir as JWalkDir;
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;

use super::parse_file;
use super::project_paths::ProjectPaths;
use super::repo_shape::{classify_from_abs_paths, path_priority_for_plan, ParsePlan};
use super::utils::normalize_path;
use super::CodeGraph;

pub mod cache;
pub mod shards;

// Re-export the cache-managed public surface so the scanner module API is stable.
pub use cache::{
    graph_cache_exists, load_graph, save_graph, save_graph_async, save_graph_owned,
    CACHE_SCHEMA_VERSION, EDGE_SEMANTICS_VERSION, GRAPH_SCHEMA_VERSION,
};

/// Returns the exact skip patterns used by scan_workspace / watcher etc.
/// Combines (union):
/// 1. Patterns from the project's .butlerignore (if present) -- primary human-friendly per-project mechanism.
/// 2. The skip_directories passed in from ButlerSettings.analysis (the single source of truth for defaults + overrides).
///
/// No more hardcoded list here. Callers (via cli settings) must provide the config list.
/// Backward compat: .butlerignore still works exactly as before; passing the full defaults list
/// from config gives the old behavior when no .butlerignore.
pub fn get_skip_patterns(root: &Path, config_skip_directories: &[String]) -> Vec<String> {
    let ignore_file = root.join(".butlerignore");
    let mut skip_patterns: Vec<String> = if ignore_file.exists() {
        std::fs::read_to_string(&ignore_file)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|l| l.trim().to_string())
            .collect()
    } else {
        vec![]
    };

    // Add the ones from config (defaults or user override via .butler/config.toml or env).
    // We extend even if some overlap with .butlerignore; duplicates are harmless for contains().
    for pat in config_skip_directories {
        if !skip_patterns.iter().any(|p| p == pat) {
            skip_patterns.push(pat.clone());
        }
    }
    skip_patterns
}

/// Relativize `path` to project `root` for skip-policy matching.
///
/// Skip rules like `examples/` must apply to **in-repo** segments only. Matching the
/// absolute path used to wipe projects whose root itself lives under `…/examples/…`
/// (e.g. `lambda-wisperer/examples/word-count` → empty Complete cache forever).
pub fn path_for_skip_policy(path: &Path, root: &Path) -> String {
    let path_str = normalize_path(&path.to_string_lossy());
    let root_str = normalize_path(&root.to_string_lossy());
    let root_trim = root_str.trim_end_matches('/');
    if path_str == root_trim {
        return String::new();
    }
    if let Some(rest) = path_str.strip_prefix(root_trim) {
        if rest.is_empty() {
            return String::new();
        }
        if rest.starts_with('/') {
            return rest.trim_start_matches('/').to_string();
        }
    }
    // Already repo-relative, or absolute outside root — use as given.
    path_str
}

fn is_examples_skip_segment(seg: &str) -> bool {
    matches!(seg, "examples" | "example")
}

fn is_tests_skip_segment(seg: &str) -> bool {
    matches!(seg, "tests" | "test" | "__tests__" | "testing" | "testutil" | "testdata")
}

/// In-tree pybind11 headers → project is a binding library (not a random C repo).
///
/// **A′.5 / Track P:** pybind's structural `m.def` fixtures live under `tests/*.cpp`.
/// Default `tests/` skip would wipe Export discovery; admit `tests/` only for these roots.
pub fn looks_pybind_binding_project(root: &Path) -> bool {
    root.join("include/pybind11/pybind11.h").is_file()
        || root.join("include/pybind11").is_dir()
        || root.join("pybind11/include/pybind11/pybind11.h").is_file()
        || root.join("pybind11/include/pybind11").is_dir()
}

/// Package under `examples/<pkg>/` that looks dual-stack (shell + native).
///
/// Kept in inventory so monorepo interconnect can see:
/// - **L2.1** pyfunction demos (word-count: Cargo + Python)
/// - **L2.3** Tauri-style apps (`package.json` + `src-tauri/` / `.ts`/`.svelte`)
pub fn looks_dual_stack_package_dir(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let has_native = dir.join("Cargo.toml").is_file()
        || dir.join("src-tauri").join("Cargo.toml").is_file()
        || dir.join("src").join("lib.rs").is_file()
        || dir.join("src").join("main.rs").is_file()
        || dir.join("CMakeLists.txt").is_file()
        || has_ext_shallow(dir, &["rs", "c", "cc", "cpp", "h", "hpp"], 2);
    let has_python = dir.join("pyproject.toml").is_file()
        || dir.join("setup.py").is_file()
        || dir.join("setup.cfg").is_file()
        || has_ext_shallow(dir, &["py"], 2);
    // Frontend shell for IPC (Tauri/Electron-style demos under examples/).
    let has_frontend = dir.join("package.json").is_file()
        || has_ext_shallow(dir, &["ts", "tsx", "js", "jsx", "svelte", "vue"], 3);
    has_native && (has_python || has_frontend)
}

fn has_ext_shallow(dir: &Path, exts: &[&str], max_depth: usize) -> bool {
    fn walk(d: &Path, exts: &[&str], depth: usize, max_depth: usize) -> bool {
        if depth > max_depth {
            return false;
        }
        let Ok(rd) = std::fs::read_dir(d) else {
            return false;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_file() {
                if let Some(e) = p.extension().and_then(|e| e.to_str()) {
                    if exts.iter().any(|x| x.eq_ignore_ascii_case(e)) {
                        return true;
                    }
                }
            } else if p.is_dir() {
                let n = ent.file_name().to_string_lossy().to_string();
                if n.starts_with('.') || n == "target" || n == "node_modules" || n == "__pycache__"
                {
                    continue;
                }
                if walk(&p, exts, depth + 1, max_depth) {
                    return true;
                }
            }
        }
        false
    }
    walk(dir, exts, 0, max_depth)
}

/// If policy path is under `examples/<pkg>/…` (or `example/`), return that package abs path.
fn dual_stack_examples_package(root: &Path, path_for_policy: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = path_for_policy
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let idx = parts
        .iter()
        .position(|p| *p == "examples" || *p == "example")?;
    // `examples` alone — keep container walkable; not a package.
    if idx + 1 >= parts.len() {
        return None;
    }
    let pkg = parts[..=idx + 1].join("/");
    let abs = root.join(&pkg);
    if looks_dual_stack_package_dir(&abs) {
        Some(abs)
    } else {
        None
    }
}

/// Segment-aware skip hit on a policy path (repo-relative preferred).
/// Never bare `contains("test")` — that matched `test_repos/...` and wiped forests.
///
/// **L2.1:** `examples/` skip does **not** apply to dual-stack packages
/// (`examples/word-count` with Cargo.toml + pyproject / .rs+.py).
///
/// **A′.5:** `tests/` skip does **not** apply when the project root is a pybind11
/// binding tree (`include/pybind11/…`) — `m.def` fixtures live under `tests/`.
fn path_matches_skip(
    path_for_policy: &str,
    skip_patterns: &[String],
    root: Option<&Path>,
) -> bool {
    // Leading `/` so `examples/foo` and `/examples/foo` share the same segment rules.
    let p = if path_for_policy.starts_with('/') {
        path_for_policy.trim_end_matches('/').to_string()
    } else if path_for_policy.is_empty() {
        return false;
    } else {
        format!("/{}", path_for_policy.trim_end_matches('/'))
    };
    skip_patterns.iter().any(|pat| {
        let seg = pat.trim_matches('/');
        if seg.is_empty() {
            return false;
        }
        let needle = format!("/{seg}");
        let hits = p.ends_with(&needle) || p.contains(&format!("{needle}/"));
        if !hits {
            return false;
        }
        if is_examples_skip_segment(seg) {
            if let Some(root) = root {
                if dual_stack_examples_package(root, path_for_policy).is_some() {
                    return false; // keep dual-stack demo packages
                }
                // Always keep walking into the examples/ directory node itself.
                let trimmed = path_for_policy.trim_matches('/');
                if trimmed == "examples" || trimmed == "example" || trimmed.ends_with("/examples")
                    || trimmed.ends_with("/example")
                {
                    return false;
                }
            }
        }
        if is_tests_skip_segment(seg) {
            if let Some(root) = root {
                if looks_pybind_binding_project(root) {
                    return false; // keep pybind m.def + companion .py fixtures
                }
            }
        }
        true
    })
}

/// Quick helper used by both scanner and watcher.
/// Prefer [`should_scan_path_under`] when the project root is known.
pub fn should_scan_path(path: &Path, skip_patterns: &[String]) -> bool {
    should_scan_path_under(path, skip_patterns, None)
}

/// Like [`should_scan_path`], but skip patterns match **relative to `root`** when given.
pub fn should_scan_path_under(
    path: &Path,
    skip_patterns: &[String],
    root: Option<&Path>,
) -> bool {
    let policy = match root {
        Some(r) => path_for_skip_policy(path, r),
        None => normalize_path(&path.to_string_lossy()),
    };
    if path_matches_skip(&policy, skip_patterns, root) {
        return false;
    }
    path.extension().is_some_and(|ext| {
        ext == "rs"
            || ext == "py"
            || ext == "ts"
            || ext == "tsx"
            || ext == "js"
            || ext == "jsx"
            || ext == "svelte"
            || ext == "go"
            || ext == "c"
            || ext == "h"
            || ext == "cpp"
            || ext == "hpp"
            || ext == "cc"
            || ext == "cxx"
    })
}

/// Path discovery priority: lower = parse first.
/// Prefer [`path_priority_for_plan`] with a shape-derived [`ParsePlan`] when available.
pub fn path_parse_priority(path: &Path) -> u8 {
    // Legacy fallback when no plan (tests / callers without inventory).
    let s = normalize_path(&path.to_string_lossy()).to_ascii_lowercase();
    if s.contains("/src/") || s.contains("/crates/") || s.contains("/lib/") || s.starts_with("src/")
    {
        0
    } else if s.contains("/tools/") || s.contains("/cmd/") || s.contains("/bin/") {
        1
    } else if s.contains("/test") || s.contains("/spec") || s.contains("/bench") {
        3
    } else {
        2
    }
}

/// Build a root-level gitignore matcher once (cheap). Nested .gitignore not walked.
fn build_root_gitignore(root: &Path) -> Option<Gitignore> {
    if !root.join(".git").exists() && !root.join(".gitignore").is_file() {
        return None;
    }
    let mut builder = GitignoreBuilder::new(root);
    let gi = root.join(".gitignore");
    if gi.is_file() {
        let _ = builder.add(&gi);
    }
    builder.build().ok()
}

fn dir_should_prune(
    name: &str,
    path_str: &str,
    skip_patterns: &[String],
    root: Option<&Path>,
) -> bool {
    let n = name.trim_end_matches('/');
    // Built-in hard prune (always on, even when skip_patterns is empty):
    //   - infra: target, .git, node_modules, …
    //   - bundled-vendor segments: vendor, _vendor, _click, third_party, …
    //     (see [`super::path_policy`]; extend via Butler analysis.extra_bundled_vendor_segments
    //      → skip_directories, not by forking this list in application code)
    if super::path_policy::is_infra_prune_dir_segment(n)
        || super::path_policy::is_bundled_vendor_dir_segment(n)
    {
        return true;
    }
    // Segment-aware only. Never `path.contains("test")` — that matches `test_repos`
    // after patterns like `test/` are trimmed, and wipes whole monorepo forests.
    // User / config extras (extra_bundled_vendor_segments, skip_directories) land here.
    // Path match is **relative to project root** so roots under `…/examples/…` stay alive.
    let policy = match root {
        Some(r) => path_for_skip_policy(Path::new(path_str), r),
        None => normalize_path(path_str),
    };
    skip_patterns.iter().any(|pat| {
        let pat = pat.trim_matches('/');
        if pat.is_empty() {
            return false;
        }
        // L2.1: never name-prune `examples/` — dual-stack packages inside must remain reachable.
        // A′.5: never name-prune `tests/` on pybind binding roots (m.def fixtures).
        // Child noise still filtered by path_matches_skip on files / nested dirs.
        if n == pat {
            if is_examples_skip_segment(pat) {
                return false;
            }
            if is_tests_skip_segment(pat) {
                if let Some(root) = root {
                    if looks_pybind_binding_project(root) {
                        return false;
                    }
                }
            }
            return true;
        }
        path_matches_skip(&policy, &[format!("{pat}/")], root)
    })
}

/// Parallel-friendly discovery via **jwalk** + skip patterns (+ optional gitignore via `ignore`).
/// Paths are absolute; sorted by **shape-derived** parse plan (generalist L0).
pub fn discover_source_paths(root: &Path, skip_patterns: &[String]) -> Vec<PathBuf> {
    discover_source_paths_with_plan(root, skip_patterns).0
}

/// Like [`discover_source_paths`], also returns the L0 [`ParsePlan`] for logging / Arch.
pub fn discover_source_paths_with_plan(
    root: &Path,
    skip_patterns: &[String],
) -> (Vec<PathBuf>, ParsePlan) {
    let skip_for_prune = skip_patterns.to_vec();
    let root_for_prune = root.to_path_buf();
    let gitignore = build_root_gitignore(root);

    let mut paths: Vec<PathBuf> = JWalkDir::new(root)
        .skip_hidden(false)
        .process_read_dir(move |_depth, path, _state, children| {
            let parent_str = normalize_path(&path.to_string_lossy());
            children.retain(|entry| {
                let Ok(e) = entry else {
                    return true;
                };
                if e.file_type.is_dir() {
                    let name = e.file_name.to_string_lossy();
                    let child_str = format!("{}/{}", parent_str.trim_end_matches('/'), name);
                    return !dir_should_prune(
                        &name,
                        &child_str,
                        &skip_for_prune,
                        Some(root_for_prune.as_path()),
                    );
                }
                true
            });
        })
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path())
        .filter(|p| should_scan_path_under(p, skip_patterns, Some(root)))
        .filter(|p| {
            gitignore
                .as_ref()
                .map(|gi| !gi.matched(p, false).is_ignore())
                .unwrap_or(true)
        })
        .map(|p| PathBuf::from(normalize_path(&p.to_string_lossy())))
        .collect();

    let plan = classify_from_abs_paths(root, &paths);
    println!(
        "📐 Repo shape={:?} fail_open={} — {} ({})",
        plan.shape,
        plan.fail_open_scope,
        plan.reason,
        if plan.priority_prefixes.is_empty() {
            "uniform priority".into()
        } else {
            format!("priority={:?}", plan.priority_prefixes)
        }
    );

    paths.sort_by(|a, b| {
        path_priority_for_plan(&plan, a)
            .cmp(&path_priority_for_plan(&plan, b))
            .then_with(|| a.cmp(b))
    });
    (paths, plan)
}

/// Centralized path list (jwalk + shape-aware priority sort).
fn walk_source_paths(root: &Path, skip_patterns: &[String]) -> Vec<PathBuf> {
    discover_source_paths(root, skip_patterns)
}

/// Content hashes for every scannable source file under `root` (all languages Butler parses).
/// Used by `load_graph` delta detection — must match `should_scan_path`, not only rs/py.
/// Keys are **repo-relative** normalized paths (portable cache / Docker).
///
/// **Parallel** over files (rayon) — gecko-class open was serial full-text hash of 30k+ files.
pub fn collect_source_file_hashes(
    root: &Path,
    skip_patterns: &[String],
) -> std::collections::HashMap<String, u64> {
    let pp = ProjectPaths::new(root);
    let paths = walk_source_paths(root, skip_patterns);
    paths
        .into_par_iter()
        .filter_map(|path| {
            let source = std::fs::read_to_string(&path).ok()?;
            let key = pp.key(&path);
            Some((key, CodeGraph::content_hash(&source)))
        })
        .collect()
}

/// Cheap tree fingerprint: max mtime (ns), total size, path count — no content reads.
/// Used to skip full content-hash on trusted Complete cache hydrate.
pub fn sources_stat_fingerprint(
    root: &Path,
    skip_patterns: &[String],
) -> (u64, u64, u64) {
    let paths = walk_source_paths(root, skip_patterns);
    reduce_stat_fingerprint(paths)
}

/// Same fingerprint from inventory keys (repo-relative) under `root` — save path.
pub fn sources_stat_fingerprint_from_inventory(
    root: &Path,
    file_hashes: &std::collections::HashMap<String, u64>,
) -> (u64, u64, u64) {
    let pp = ProjectPaths::new(root);
    let paths: Vec<PathBuf> = file_hashes
        .keys()
        .map(|k| pp.to_abs(Path::new(k)))
        .collect();
    reduce_stat_fingerprint(paths)
}

fn reduce_stat_fingerprint(paths: Vec<PathBuf>) -> (u64, u64, u64) {
    let stats: Vec<(u64, u64)> = paths
        .into_par_iter()
        .filter_map(|path| {
            let meta = std::fs::metadata(&path).ok()?;
            let len = meta.len();
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            Some((mtime_ns, len))
        })
        .collect();
    let count = stats.len() as u64;
    let mut max_mtime = 0u64;
    let mut total_bytes = 0u64;
    for (m, len) in stats {
        max_mtime = max_mtime.max(m);
        total_bytes = total_bytes.saturating_add(len);
    }
    (max_mtime, total_bytes, count)
}

/// True when live tree stats match the stamp persisted in the cache manifest.
pub fn sources_stat_matches_manifest(
    root: &Path,
    skip_patterns: &[String],
    max_mtime_ns: u64,
    total_bytes: u64,
    path_count: u64,
) -> bool {
    if path_count == 0 || max_mtime_ns == 0 {
        return false; // old cache / missing stamp → force content hash
    }
    let (m, b, c) = sources_stat_fingerprint(root, skip_patterns);
    m == max_mtime_ns && b == total_bytes && c == path_count
}

struct ParsedFileOutcome {
    /// Repo-relative path (normalized) — matches BlockInfo.file and file_hashes keys.
    rel_path: PathBuf,
    content_hash: u64,
    parsed: Option<super::parser::ParsedFile>,
    read_error: bool,
    parse_error: Option<String>,
}

/// Scan all source files under `root` into a CodeGraph (skeleton; edges deferred).
/// Sources are **stripped** after parse — hydrate on compose from disk.
pub fn scan_workspace(
    root: impl AsRef<Path>,
    current_file: Option<Arc<std::sync::Mutex<Option<String>>>>,
    config_skip_directories: &[String],
) -> CodeGraph {
    scan_workspace_with_waves(root, current_file, config_skip_directories, None)
}

/// Like [`scan_workspace`], but invokes `on_wave` after each priority wave with a slim
/// snapshot (no sources) so the server can publish L1 early while wave-2 still parses.
pub fn scan_workspace_with_waves(
    root: impl AsRef<Path>,
    current_file: Option<Arc<std::sync::Mutex<Option<String>>>>,
    config_skip_directories: &[String],
    on_wave: Option<&mut dyn FnMut(&CodeGraph)>,
) -> CodeGraph {
    let root = root.as_ref().to_owned();

    println!(
        "🔎 Snooper starting progressive workspace scan on: {}",
        root.display()
    );
    println!("   → Using 32 MiB stack for heavy collection work (to survive deep ASTs)");
    println!("   → Entry point reached. Spawning large-stack worker thread...");

    let current_file = current_file.clone();
    let skips = config_skip_directories.to_vec();
    // Callback cannot cross threads safely as &mut dyn — run do_scan on this thread
    // when progressive publish is needed; otherwise use large-stack worker.
    if on_wave.is_some() {
        // Caller wants mid-wave publish: stay on caller's large-stack thread.
        return do_scan_workspace(&root, current_file, &skips, on_wave);
    }

    let handle = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .name("snooper-large-stack".into())
        .spawn(move || do_scan_workspace(&root, current_file, &skips, None))
        .expect("failed to spawn large-stack snooper thread");

    handle.join().expect("snooper thread panicked")
}

/// Internal implementation that runs on a controlled large-stack thread (or caller thread).
fn do_scan_workspace(
    root: &Path,
    current_file: Option<Arc<std::sync::Mutex<Option<String>>>>,
    config_skip_directories: &[String],
    mut on_wave: Option<&mut dyn FnMut(&CodeGraph)>,
) -> CodeGraph {
    let mut graph = CodeGraph::new();
    println!(
        "🔎 [Thread] Snooper thread started with large stack. Root: {}",
        root.display()
    );
    println!("🔎 Snooper scanning with Tree-sitter: {}", root.display());

    let skip_patterns = get_skip_patterns(root, config_skip_directories);
    println!(
        "   → Loaded {} skip patterns from .butlerignore + config",
        skip_patterns.len()
    );

    graph.dependency_versions = std::collections::HashMap::new();
    let pp = ProjectPaths::new(root);

    // ============================================
    // Phase 0: Inventory (jwalk multi-core discovery)
    // ============================================
    let paths: Vec<PathBuf> = walk_source_paths(root, &skip_patterns);
    let file_count = paths.len();
    println!(
        "📋 Phase 0 inventory: {} source files (jwalk, priority-sorted)",
        file_count
    );

    // Lang honesty: dominant unscanned product code (java/kt/…) vs Butler-supported inventory.
    let census = crate::snooper::warehouse_lang::census_code_extensions(root, &skip_patterns);
    if let Some(void) = crate::snooper::warehouse_lang::assess_lang_void(&census) {
        println!(
            "⚠️  Warehouse lang void: .{}×{} on disk vs {} scanned — refuse product Trace/Arch hubs",
            void.dominant_ext, void.unsupported_files, void.supported_files
        );
        graph.warehouse_lang_void = Some(void);
    } else {
        graph.warehouse_lang_void = None;
        if census.unsupported_total() > 0 {
            println!(
                "📋 Lang census: supported={} unsupported_product={} (no void)",
                census.supported,
                census.unsupported_total()
            );
        }
    }

    // Pre-fill file_hashes with 0 until parsed (inventory completeness for progress meters).
    // Keys are always repo-relative under project root.
    for p in &paths {
        graph.file_hashes.entry(pp.key(p)).or_insert(0);
    }
    if let Some(cb) = on_wave.as_mut() {
        cb(&graph);
    }

    // ============================================
    // Phase 1: Parallel Tree-sitter in priority waves
    // Wave 0 = src/crates/lib, then tools/cmd, then rest, tests last.
    // ============================================
    // Historical ask: ~75% cores. Host pressure + free RAM can cut harder (anti-OOM).
    let requested = (num_cpus::get().max(1) as f64 * 0.75).round() as usize;
    let cpus = crate::sys_pressure::scan_thread_cap(requested.max(1));
    let pressure = crate::sys_pressure::snapshot();
    println!(
        "📦 Phase 1: Collecting blocks (Tree-sitter, {} files, {} rayon threads [asked {}, {}]) progressive waves ...",
        file_count,
        cpus,
        requested.max(1),
        pressure.summary_line()
    );

    let phase1_pool = ThreadPoolBuilder::new()
        .num_threads(cpus)
        .thread_name(|i| format!("butler-scan-{i}"))
        .stack_size(32 * 1024 * 1024)
        .build()
        .expect("failed to build Phase 1 rayon pool");

    let processed_counter = AtomicUsize::new(0);
    let reporter = current_file.clone();

    // Group into waves by priority bucket (0,1,2,3).
    // P.3: dual-stack inventory boosts peer-lang files into early waves so
    // interconnect Export/Ipc maps see both sides without full monorepo wait.
    let path_strs: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    let inv = crate::snooper::interconnect::LangPresence::from_paths(
        path_strs.iter().map(|s| s.as_str()),
    );
    let mut waves: [Vec<PathBuf>; 4] = [vec![], vec![], vec![], vec![]];
    for p in paths {
        // Base priority from path shape (plan already applied when listing paths).
        let base = path_parse_priority(&p).min(3);
        let pri = crate::snooper::interconnect::dual_stack_parse_boost(&p, inv)
            .map(|b| b.min(base))
            .unwrap_or(base)
            .min(3) as usize;
        waves[pri].push(p);
    }

    for (wave_idx, wave_paths) in waves.into_iter().enumerate() {
        if wave_paths.is_empty() {
            continue;
        }
        // Re-sample between waves: if host went Black mid-scan, pause briefly so co-tenants
        // can reclaim (still finish eventually — admission already passed for this open).
        let p_wave = crate::sys_pressure::snapshot();
        if p_wave.tier >= crate::sys_pressure::PressureTier::Red {
            println!(
                "   ⚠️  Phase 1 wave {} under pressure ({}) — brief yield before parse",
                wave_idx,
                p_wave.summary_line()
            );
            std::thread::sleep(std::time::Duration::from_millis(
                if p_wave.tier >= crate::sys_pressure::PressureTier::Black {
                    1500
                } else {
                    400
                },
            ));
        }
        println!(
            "   → Wave {} (priority {}): {} files …",
            wave_idx,
            wave_idx,
            wave_paths.len()
        );

        let outcomes: Vec<ParsedFileOutcome> = phase1_pool.install(|| {
            wave_paths
                .par_iter()
                .map(|path| {
                    let n = processed_counter.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_multiple_of(500) || n == 1 || n <= 5 {
                        println!("   [{}] Processing: {}", n, path.display());
                    }
                    if n.is_multiple_of(25) {
                        if let Some(ref reporter) = reporter {
                            if let Ok(mut guard) = reporter.lock() {
                                *guard = Some(path.display().to_string());
                            }
                        }
                    }

                    // Warehouse key: always repo-relative (ProjectPaths anchor).
                    let rel_path = pp.to_rel(path);
                    let full_path = path.clone();

                    match std::fs::read_to_string(&full_path) {
                        Ok(source) => {
                            let content_hash = CodeGraph::content_hash(&source);
                            match parse_file(rel_path.clone(), &source) {
                                Ok(parsed) => ParsedFileOutcome {
                                    rel_path,
                                    content_hash,
                                    parsed: Some(parsed),
                                    read_error: false,
                                    parse_error: None,
                                },
                                Err(e) => ParsedFileOutcome {
                                    rel_path,
                                    content_hash,
                                    parsed: None,
                                    read_error: false,
                                    parse_error: Some(format!("{e:?}")),
                                },
                            }
                        }
                        Err(_) => ParsedFileOutcome {
                            rel_path,
                            content_hash: 0,
                            parsed: None,
                            read_error: true,
                            parse_error: None,
                        },
                    }
                })
                .collect()
        });

        let (file_hashes, nodes) = merge_parse_outcomes(outcomes);
        graph.file_hashes.extend(file_hashes);
        graph.nodes.extend(nodes);

        // Slim RAM after every wave — no warehouse of sources.
        graph.strip_all_sources();
        graph.rebuild_module_hashes();

        println!(
            "   → Wave {} done: {} blocks total, {} files hashed",
            wave_idx,
            graph.nodes.len(),
            graph.file_hashes.len()
        );

        if let Some(cb) = on_wave.as_mut() {
            cb(&graph);
        }
    }

    // Drop placeholder zero hashes for files we never read successfully.
    graph.file_hashes.retain(|_, h| *h != 0);
    graph.rebuild_module_hashes();
    graph.strip_all_sources();

    println!(
        "✅ Phase 1 complete: {} source files processed → {} blocks collected ({} modules) [sources stripped]",
        file_count,
        graph.nodes.len(),
        graph.module_hashes.len()
    );

    println!("   → Edge building deferred (will run on-demand via ensure_call_graph when needed)");

    println!("🧠 Running final analysis: detect_cycles + compute_hubs (top 5%) ...");
    graph.finalize_build();

    println!(
        "✅ Built graph with {} real blocks (dependency versions loaded lazily, slim RAM)",
        graph.nodes.len()
    );

    if let Some(cb) = on_wave.as_mut() {
        cb(&graph);
    }

    graph
}

fn merge_parse_outcomes(
    outcomes: Vec<ParsedFileOutcome>,
) -> (
    std::collections::HashMap<String, u64>,
    std::collections::HashMap<super::model::Id, super::model::BlockInfo>,
) {
    outcomes
        .into_par_iter()
        .filter_map(|outcome| {
            if outcome.read_error {
                eprintln!(
                    "⚠️ Failed to read file: {}",
                    outcome.rel_path.display()
                );
                return None;
            }
            if let Some(err) = outcome.parse_error {
                eprintln!("⚠️ Parse error for {:?}: {}", outcome.rel_path, err);
            }
            let pstr = normalize_path(&outcome.rel_path.to_string_lossy());
            let hash = outcome.content_hash;
            let mut local_nodes = std::collections::HashMap::new();
            if let Some(parsed) = outcome.parsed {
                for mut block in parsed.blocks {
                    // Free source immediately per block (extra safety before wave strip).
                    block.strip_source();
                    local_nodes.insert(block.id.clone(), block);
                }
            }
            Some((pstr, hash, local_nodes))
        })
        .fold(
            || {
                (
                    std::collections::HashMap::<String, u64>::new(),
                    std::collections::HashMap::new(),
                )
            },
            |mut acc, (pstr, hash, local_nodes)| {
                if hash != 0 {
                    acc.0.insert(pstr, hash);
                }
                acc.1.extend(local_nodes);
                acc
            },
        )
        .reduce(
            || {
                (
                    std::collections::HashMap::new(),
                    std::collections::HashMap::new(),
                )
            },
            |mut a, b| {
                a.0.extend(b.0);
                a.1.extend(b.1);
                a
            },
        )
}

#[cfg(test)]
mod skip_segment_tests {
    use super::{
        dir_should_prune, looks_dual_stack_package_dir, looks_pybind_binding_project,
        path_for_skip_policy, should_scan_path, should_scan_path_under,
    };
    use std::path::Path;

    #[test]
    fn test_repos_not_pruned_by_test_pattern() {
        let skips = vec!["tests/".into(), "test/".into(), "benches/".into()];
        let root = Path::new("/projects/test_repos/redis");
        // Parent dir name is test_repos — must not match segment "test"
        assert!(!dir_should_prune(
            "redis",
            "/projects/test_repos/redis",
            &skips,
            Some(root)
        ));
        assert!(!dir_should_prune(
            "src",
            "/projects/test_repos/redis/src",
            &skips,
            Some(root)
        ));
        assert!(dir_should_prune(
            "tests",
            "/projects/test_repos/redis/tests",
            &skips,
            Some(root)
        ));
        assert!(dir_should_prune(
            "unit",
            "/projects/test_repos/redis/tests/unit",
            &skips,
            Some(root)
        ));
    }

    #[test]
    fn should_scan_keeps_test_repos_sources() {
        let skips = vec!["tests/".into(), "test/".into()];
        let root = Path::new("/projects/test_repos/redis");
        assert!(should_scan_path_under(
            Path::new("/projects/test_repos/redis/src/sds.c"),
            &skips,
            Some(root)
        ));
        assert!(!should_scan_path_under(
            Path::new("/projects/test_repos/redis/tests/unit/foo.c"),
            &skips,
            Some(root)
        ));
    }

    #[test]
    fn in_package_vendor_dirs_pruned() {
        let skips: Vec<String> = vec![];
        assert!(dir_should_prune(
            "_click",
            "/projects/test_repos/typer/typer/_click",
            &skips,
            None
        ));
        assert!(dir_should_prune(
            "_vendor",
            "/projects/test_repos/pytorch/torch/_vendor",
            &skips,
            None
        ));
        assert!(dir_should_prune(
            "vendored",
            "/repo/pkg/vendored",
            &skips,
            None
        ));
        // Product private packages must not be pruned by underscore alone.
        assert!(!dir_should_prune(
            "_dynamo",
            "/projects/test_repos/pytorch/torch/_dynamo",
            &skips,
            None
        ));
        assert!(!dir_should_prune(
            "core",
            "/projects/test_repos/typer/typer/core",
            &skips,
            None
        ));
    }

    #[test]
    fn should_scan_skips_in_package_vendor_paths() {
        let skips = vec!["_click/".into(), "_vendor/".into()];
        assert!(!should_scan_path(
            Path::new("/projects/test_repos/typer/typer/_click/core.py"),
            &skips
        ));
        assert!(!should_scan_path(
            Path::new("/projects/test_repos/pytorch/torch/_vendor/packaging/__init__.py"),
            &skips
        ));
        assert!(should_scan_path(
            Path::new("/projects/test_repos/typer/typer/main.py"),
            &skips
        ));
    }

    #[test]
    fn project_rooted_under_examples_is_not_wiped() {
        // Regression: absolute-path skip matched `/examples/` in the *root* prefix and
        // produced empty Complete caches for demos like examples/word-count.
        let skips = vec!["examples/".into(), "tests/".into(), "docs/".into()];
        let root = Path::new("/projects/lambda-wisperer/examples/word-count");
        assert_eq!(
            path_for_skip_policy(
                Path::new("/projects/lambda-wisperer/examples/word-count/src/main.py"),
                root
            ),
            "src/main.py"
        );
        assert!(should_scan_path_under(
            Path::new("/projects/lambda-wisperer/examples/word-count/src/main.py"),
            &skips,
            Some(root)
        ));
        assert!(!dir_should_prune(
            "src",
            "/projects/lambda-wisperer/examples/word-count/src",
            &skips,
            Some(root)
        ));
        // examples/ dir itself is walkable (L2.1); non-dual-stack children still skip via path.
        assert!(!dir_should_prune(
            "examples",
            "/projects/lambda-wisperer/examples/word-count/examples",
            &skips,
            Some(root)
        ));
    }

    #[test]
    fn dual_stack_under_monorepo_examples_stays_scannable() {
        // L2.1: pyo3/examples/word-count must stay in inventory when root is pyo3.
        let skips = vec!["examples/".into(), "tests/".into(), "docs/".into()];
        let Some(mono_buf) = crate::resolve_optional_test_repo("pyo3") else {
            return;
        };
        let mono = mono_buf.as_path();
        let wc = mono.join("examples/word-count");
        if !wc.is_dir() {
            // CI / machines without the keeper tree.
            return;
        }
        assert!(
            looks_dual_stack_package_dir(&wc),
            "word-count should look dual-stack"
        );
        assert!(
            should_scan_path_under(&wc.join("src/lib.rs"), &skips, Some(mono)),
            "dual-stack package under examples/ must scan .rs"
        );
        assert!(
            should_scan_path_under(&wc.join("word_count/__init__.py"), &skips, Some(mono)),
            "dual-stack package under examples/ must scan .py"
        );
        // Plain tutorial under examples (no dual-stack markers) still skipped if present.
        let tutorial = mono.join("examples/not-a-real-dual-stack-xyz");
        if !tutorial.exists() {
            // path match would skip any file under examples/non-dual
            let fake = mono.join("examples/pure-tutorial/foo.py");
            assert!(
                !should_scan_path_under(&fake, &skips, Some(mono)),
                "non-dual-stack under examples/ still skipped"
            );
        }
    }

    #[test]
    fn pybind_binding_project_keeps_tests_mdef_fixtures() {
        // A′.5: pybind11 tests/*.cpp hold m.def export tables — must not wipe under tests/.
        let skips = vec!["tests/".into(), "test/".into(), "docs/".into()];
        let Some(root_buf) = crate::resolve_optional_test_repo("pybind11") else {
            return;
        };
        let root = root_buf.as_path();
        if !root.join("include/pybind11").is_dir() {
            return;
        }
        assert!(
            looks_pybind_binding_project(root),
            "pybind11 checkout should look like a binding project"
        );
        assert!(
            !dir_should_prune(
                "tests",
                &root.join("tests").to_string_lossy(),
                &skips,
                Some(root)
            ),
            "tests/ dir must stay walkable on pybind roots"
        );
        assert!(
            should_scan_path_under(
                &root.join("tests/test_constants_and_functions.cpp"),
                &skips,
                Some(root)
            ),
            "m.def fixture .cpp must scan"
        );
        assert!(
            should_scan_path_under(
                &root.join("tests/test_constants_and_functions.py"),
                &skips,
                Some(root)
            ),
            "companion test .py must scan"
        );
        // Non-pybind roots still skip tests/.
        if let Some(redis_buf) = crate::resolve_optional_test_repo("redis") {
            let redis = redis_buf.as_path();
            if redis.is_dir() {
                assert!(!looks_pybind_binding_project(redis));
                assert!(!should_scan_path_under(
                    &redis.join("tests/unit/foo.c"),
                    &skips,
                    Some(redis)
                ));
            }
        }
    }

    #[test]
    fn dual_stack_tauri_ipc_app_under_examples_stays_scannable() {
        // L2.3: examples/api (package.json + src-tauri) must stay when root is tauri monorepo.
        let skips = vec!["examples/".into(), "tests/".into(), "docs/".into()];
        let Some(mono_buf) = crate::resolve_optional_test_repo("tauri") else {
            return;
        };
        let mono = mono_buf.as_path();
        let api = mono.join("examples/api");
        if !api.is_dir() {
            return;
        }
        assert!(
            looks_dual_stack_package_dir(&api),
            "tauri examples/api should look dual-stack (frontend + src-tauri)"
        );
        assert!(
            should_scan_path_under(&api.join("src-tauri/src/cmd.rs"), &skips, Some(mono)),
            "src-tauri cmd.rs must scan under monorepo"
        );
        assert!(
            should_scan_path_under(
                &api.join("src/views/Communication.svelte"),
                &skips,
                Some(mono)
            ),
            "svelte invoke call sites must scan under monorepo"
        );
    }
}
