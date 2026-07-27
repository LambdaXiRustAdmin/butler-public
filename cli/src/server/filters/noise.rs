//! Noise path/file filters + application path priority (homonym ranking input).
use cli::config::AnalysisConfig;
use code_graph::BlockInfo;
use std::path::Path;

/// Configurable noise filter (defaults + `.butler/config.toml` overrides).
#[derive(Debug, Clone)]
pub struct NoiseFilterConfig {
    pub path_components: Vec<String>,
    pub file_patterns: Vec<String>,
}

/// Directory *names* treated as noise when a path segment matches **exactly**.
///
/// Intentionally exact-only: `test_repos` must NOT match `test` or `test_*`.
/// That folder holds real app checkouts (fd, bat, …) for Butler evaluation.
pub fn default_noise_path_components() -> Vec<String> {
    // Test/docs/bench noise + built-in bundled-vendor segments (single source:
    // code_graph::BUNDLED_VENDOR_DIR_SEGMENTS). Users extend via
    // analysis.extra_bundled_vendor_segments without code changes.
    let mut v = vec![
        "tests".into(),
        "test".into(), // exact segment only — not test_repos / test-harness / …
        "testutil".into(),
        "testing".into(),
        "testdata".into(),
        "__tests__".into(),
        "docs".into(),
        "docs_src".into(),
        "doc".into(),
        "tutorials".into(),
        "tutorial".into(),
        "guides".into(),
        "examples".into(),
        "fixtures".into(),
        "benches".into(),
    ];
    for seg in code_graph::BUNDLED_VENDOR_DIR_SEGMENTS {
        if !v
            .iter()
            .any(|c: &String| c.eq_ignore_ascii_case(seg))
        {
            v.push((*seg).into());
        }
    }
    v
}

pub fn default_noise_file_patterns() -> Vec<String> {
    vec![
        "_test.go".into(),
        "_test.rs".into(),
        "_testing.go".into(),
        ".test.ts".into(),
        ".spec.ts".into(),
        "target_test.go".into(),
    ]
}

/// Higher = more likely primary application code (Sprint 8.1 homonym ranking).
///
/// `tools/.../src/` must not outrank `crates/.../src/` — peripheral ancestors
/// demote nested primary segments.
pub fn application_path_priority(path: &str) -> i32 {
    let p = path.replace('\\', "/").to_lowercase();
    let segments: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();

    const ROOT_PRIMARY: &[&str] = &[
        "crates", "lib", "libs", "packages", "pkg", "internal", "apps", "modules",
    ];
    // Exact segment names only — `test_repos` is NOT peripheral (eval checkouts live there).
    const PERIPHERAL: &[&str] = &[
        "tools",
        "examples",
        "tests",
        "test",
        "benches",
        "bench",
        "benchmarks", // rich/benchmarks, asv-style trees (A′.10)
        "benchmark",
        "docs",
        "fixtures",
        "testutil",
        "testing",
        "testdata",
    ];

    let peripheral_idx = segments.iter().position(|s| PERIPHERAL.contains(s));

    for (i, seg) in segments.iter().enumerate() {
        if ROOT_PRIMARY.contains(seg) && peripheral_idx.map_or(true, |pi| i < pi) {
            return 200 - i as i32;
        }
    }

    if segments.first() == Some(&"src") && peripheral_idx.is_none() {
        return 150;
    }

    for (i, seg) in segments.iter().enumerate() {
        if *seg == "src" {
            return if peripheral_idx.is_some() && peripheral_idx.unwrap() < i {
                20
            } else {
                100 - i as i32
            };
        }
    }

    if peripheral_idx.is_some() {
        return 10;
    }
    50
}

impl Default for NoiseFilterConfig {
    fn default() -> Self {
        Self::from_analysis(&AnalysisConfig {
            worker_stack_size_mb: 32,
            skip_directories: vec![],
            max_call_graph_depth: 2,
            trace_max_fan_out: 20,
            trace_max_visited_nodes: 200,
            max_context_blocks: 20,
            hub_budget_pct: 0.35,
            edge_build_thread_pct: 1.0,
            noise_path_components: default_noise_path_components(),
            noise_file_patterns: default_noise_file_patterns(),
            extra_bundled_vendor_segments: vec![],
        })
    }
}

impl NoiseFilterConfig {
    pub fn from_analysis(analysis: &AnalysisConfig) -> Self {
        Self {
            path_components: if analysis.noise_path_components.is_empty() {
                default_noise_path_components()
            } else {
                analysis.noise_path_components.clone()
            },
            file_patterns: if analysis.noise_file_patterns.is_empty() {
                default_noise_file_patterns()
            } else {
                analysis.noise_file_patterns.clone()
            },
        }
    }
}

/// True if this path segment is a known noise *directory name* (exact match only).
///
/// Do **not** use prefix matches here — `test_repos` is real project code, not a test suite.
pub fn is_noise_dir_segment(seg: &str, config: &NoiseFilterConfig) -> bool {
    let s = seg.to_lowercase();
    if s == "bench" || s.starts_with("bench_") {
        return true;
    }
    config
        .path_components
        .iter()
        .any(|c| c.eq_ignore_ascii_case(&s))
}

/// True if this **file name** looks like a unit/integration test file (not a folder).
pub fn is_noise_test_filename(file_name: &str) -> bool {
    let f = file_name.to_lowercase();
    if f.starts_with("bench_") {
        return true;
    }
    if f.ends_with("_test.py") || f == "noxfile.py" || f == "tox.ini" {
        return true;
    }
    if f.starts_with("test_") && (f.ends_with(".go") || f.ends_with(".rs") || f.ends_with(".py"))
    {
        return true;
    }
    if f.ends_with("_testing.go") {
        return true;
    }
    // Rust/Go style: foo_test.rs / foo_test.go already covered by patterns often; keep explicit.
    if f.ends_with("_test.rs") || f.ends_with("_test.go") {
        return true;
    }
    false
}

/// Returns true if the block is architectural noise (test *files*, exact test/docs/bench *dirs*).
///
/// Folder policy: only **exact** segment names from config (`tests`, `test`, `fixtures`, …).
/// Prefixes like `test_repos` are **not** noise — they host real repos for evaluation.
pub fn is_noise(b: &BlockInfo, root: &Path, config: &NoiseFilterConfig) -> bool {
    let rel = b.file.strip_prefix(root).unwrap_or(&b.file);
    let path_str = rel.to_string_lossy().replace('\\', "/").to_lowercase();
    let file_name = rel
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    // --- File-level test / bench code ---
    if is_noise_test_filename(&file_name) {
        return true;
    }
    for pat in &config.file_patterns {
        let p = pat.to_lowercase();
        if file_name.ends_with(&p) || file_name == p {
            return true;
        }
    }

    // --- Directory segments: exact names only (never prefix "test_") ---
    path_str
        .split('/')
        .filter(|s| !s.is_empty())
        .any(|seg| is_noise_dir_segment(seg, config))
}
