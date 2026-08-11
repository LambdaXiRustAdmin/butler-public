//! Configuration loading for the Butler CLI and server (layered from files, env, and defaults).

use config::{Config, Environment, File};
use directories::ProjectDirs;
use serde::Deserialize;
use std::path::PathBuf;

/// Server host and port configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub host: String,
    /// Display / Basic-auth username (default: machine hostname).
    /// Env: `BUTLER_USERNAME` or `BUTLER__SERVER__USERNAME`.
    #[serde(default = "default_server_username")]
    pub username: String,
    /// Optional shared secret. When set (non-empty), HTTP routes require
    /// `Authorization: Bearer <password>` (or Basic username:password).
    /// Env: `BUTLER_PASSWORD` / `BUTLER_API_TOKEN` / `BUTLER__SERVER__PASSWORD`.
    /// Leave empty for open local dev (still prefer `host = "127.0.0.1"`).
    #[serde(default)]
    pub password: Option<String>,
    /// Project roots to warm (async graph load + watcher) at server boot.
    /// Also set via env `BUTLER_WARM_ROOTS` (colon- or comma-separated paths).
    #[serde(default)]
    pub warm_roots: Vec<String>,
    /// Max in-memory graphs (LRU eviction of cold roots). 0 = unlimited.
    #[serde(default = "default_max_cached_graphs")]
    pub max_cached_graphs: usize,
    /// Cap for composed `/context` query cache entries.
    #[serde(default = "default_query_cache_cap")]
    pub query_cache_cap: usize,
    /// Hop B: seconds idle before a non-pinned warehouse may sleep (drop RAM + watchers).
    /// Keep Complete on disk. Env: `BUTLER_WAREHOUSE_IDLE_SECS` / `BUTLER__SERVER__WAREHOUSE_IDLE_SECS`.
    #[serde(default = "default_warehouse_idle_secs")]
    pub warehouse_idle_secs: u64,
    /// Hop B: longer idle for last_state project (preferred under light pressure).
    /// Env: `BUTLER_WAREHOUSE_LAST_IDLE_SECS`.
    #[serde(default = "default_warehouse_last_idle_secs")]
    pub warehouse_last_idle_secs: u64,
}

/// Default username = OS hostname (install “computer name” field).
pub fn default_server_username() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("HOSTNAME")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::env::var("COMPUTERNAME")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "butler".to_string())
}

/// Token for outbound HTTP clients (MCP bridge → butler-server).
/// Prefer `BUTLER_API_TOKEN`, then `BUTLER_PASSWORD`.
pub fn client_token_from_env() -> Option<String> {
    for key in ["BUTLER_API_TOKEN", "BUTLER_PASSWORD", "BUTLER_TOKEN"] {
        if let Ok(v) = std::env::var(key) {
            let t = v.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

/// Apply Bearer header when env token is set.
pub fn apply_client_auth(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match client_token_from_env() {
        Some(tok) => req.header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {tok}"),
        ),
        None => req,
    }
}

/// Flat env overrides not always mapped by `BUTLER__SERVER__*` nested form.
fn apply_server_identity_env(server: &mut ServerConfig) {
    // Legacy compose: BUTLER_HOST=0.0.0.0 (llm-stack) — same as BUTLER__SERVER__HOST
    if let Ok(h) = std::env::var("BUTLER_HOST") {
        let t = h.trim();
        if !t.is_empty() {
            server.host = t.to_string();
        }
    }
    if let Ok(u) = std::env::var("BUTLER_USERNAME") {
        let t = u.trim();
        if !t.is_empty() {
            server.username = t.to_string();
        }
    }
    for key in ["BUTLER_PASSWORD", "BUTLER_API_TOKEN", "BUTLER_TOKEN"] {
        if let Ok(p) = std::env::var(key) {
            let t = p.trim();
            if !t.is_empty() {
                server.password = Some(t.to_string());
                break;
            }
        }
    }
    // Hop B sleep knobs (flat env for ops probes without nested config).
    if let Ok(v) = std::env::var("BUTLER_WAREHOUSE_IDLE_SECS") {
        if let Ok(n) = v.trim().parse::<u64>() {
            server.warehouse_idle_secs = n;
        }
    }
    if let Ok(v) = std::env::var("BUTLER_WAREHOUSE_LAST_IDLE_SECS") {
        if let Ok(n) = v.trim().parse::<u64>() {
            server.warehouse_last_idle_secs = n;
        }
    }
    if let Ok(v) = std::env::var("BUTLER_MAX_CACHED_GRAPHS") {
        if let Ok(n) = v.trim().parse::<usize>() {
            server.max_cached_graphs = n;
        }
    }
}

/// Default in-memory graph slots. Raised from 12 so multi-repo Trace demos
/// (click/gin/typer/redis/…) do not thrash cold re-scan. 0 = unlimited.
fn default_max_cached_graphs() -> usize {
    32
}

fn default_query_cache_cap() -> usize {
    128
}

/// Non-pinned roots sleep after this many idle seconds (Hop B). 0 = idle sleep disabled.
fn default_warehouse_idle_secs() -> u64 {
    300
}

/// last_state root sleeps after this many idle seconds (longer than generic idle).
fn default_warehouse_last_idle_secs() -> u64 {
    3600
}

/// Exact directory segment names only (mirrors filters::default_noise_path_components).
/// Do not use prefixes — `test_repos` is real eval checkouts, not a test suite.
/// Bundled-vendor segments are injected by [`apply_bundled_vendor_policy`].
fn default_noise_path_components() -> Vec<String> {
    vec![
        "tests".into(),
        "test".into(),
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
        "site".into(),
    ]
}

/// Merge built-in + `extra_bundled_vendor_segments` into skip_directories and noise.
///
/// Built-in list: [`code_graph::BUNDLED_VENDOR_DIR_SEGMENTS`] (scan hard-prune always).
/// User extras extend that list without replacing it and without a code change.
fn apply_bundled_vendor_policy(analysis: &mut AnalysisConfig) {
    let mut segs = code_graph::bundled_vendor_dir_segments_owned();
    for extra in &analysis.extra_bundled_vendor_segments {
        let t = extra.trim().trim_matches('/').to_string();
        if t.is_empty() {
            continue;
        }
        if !segs.iter().any(|s| s.eq_ignore_ascii_case(&t)) {
            segs.push(t);
        }
    }
    for seg in segs {
        let pat = format!("{seg}/");
        if !analysis
            .skip_directories
            .iter()
            .any(|p| p.trim_matches('/').eq_ignore_ascii_case(&seg))
        {
            analysis.skip_directories.push(pat);
        }
        if !analysis
            .noise_path_components
            .iter()
            .any(|p| p.eq_ignore_ascii_case(&seg))
        {
            analysis.noise_path_components.push(seg);
        }
    }
}

fn default_max_context_blocks() -> usize {
    20
}

fn default_hub_budget_pct() -> f64 {
    0.35
}

/// Default ~75% of logical CPUs as the *request*; runtime still caps by MemAvailable
/// (see code_graph edge builder — 8 GiB design point). Override via config/env.
fn default_edge_build_thread_pct() -> f64 {
    0.75
}

fn default_noise_file_patterns() -> Vec<String> {
    vec![
        "_test.go".into(),
        "_test.rs".into(),
        "_testing.go".into(),
        ".test.ts".into(),
        ".spec.ts".into(),
        "target_test.go".into(),
    ]
}

/// Analysis and graph building configuration (stack sizes, skips, depth).
#[derive(Debug, Deserialize, Clone)]
pub struct AnalysisConfig {
    pub worker_stack_size_mb: u32,
    pub skip_directories: Vec<String>,
    pub max_call_graph_depth: usize,
    /// Max neighbors kept per hop in TraceBlastRadius (lowest-scoring edges pruned).
    pub trace_max_fan_out: usize,
    /// Hard cap on unique nodes visited during a trace traversal.
    pub trace_max_visited_nodes: usize,
    /// Hard cap on code blocks delivered in any HTTP/MCP response payload (Sprint 9).
    #[serde(default = "default_max_context_blocks")]
    pub max_context_blocks: usize,
    /// Fraction of `max_context_blocks` reserved for hubs in ArchitecturalSummary (0.30–0.40 typical).
    #[serde(default = "default_hub_budget_pct")]
    pub hub_budget_pct: f64,
    /// Fraction of logical CPUs for background edge-build rayon pool (1.0 = saturate all cores).
    #[serde(default = "default_edge_build_thread_pct")]
    pub edge_build_thread_pct: f64,
    /// Path component directory names excluded from hubs/skeletons (tests, fixtures, …).
    /// Bundled-vendor segments from the built-in list + [`Self::extra_bundled_vendor_segments`]
    /// are merged in at load time.
    #[serde(default = "default_noise_path_components")]
    pub noise_path_components: Vec<String>,
    /// Filename suffix/pattern fragments excluded from hubs/skeletons (`_test.go`, …).
    #[serde(default = "default_noise_file_patterns")]
    pub noise_file_patterns: Vec<String>,
    /// **Bundled-vendor directory segments** (user extras).
    ///
    /// Merged with the built-in skip list in `code_graph::BUNDLED_VENDOR_DIR_SEGMENTS`
    /// (`vendor`, `vendored`, `_vendor`, `_click`, `third_party`, …). Segment-exact
    /// names only — not a security allowlist; a known-vendored-tree **skip list**.
    /// Hard-pruned at scan (via `skip_directories`) and treated as noise for ranking.
    ///
    /// Extend without code changes in `.butler/config.toml`:
    /// ```toml
    /// [analysis]
    /// extra_bundled_vendor_segments = ["_bundled", "thirdparty"]
    /// ```
    /// Built-in names are always kept; this field only **adds** segments.
    #[serde(default)]
    pub extra_bundled_vendor_segments: Vec<String>,
}

/// Cross-language IPC bridging rule (caller site → callee handler).
#[derive(Debug, Deserialize, Clone)]
pub struct IpcRuleConfig {
    pub name: String,
    /// Regex on caller source with `(?P<sym>...)` capture for the bridge symbol.
    pub caller_pattern: String,
    #[serde(default)]
    pub caller_langs: Vec<String>,
    #[serde(default)]
    pub caller_file_extensions: Vec<String>,
    #[serde(default)]
    pub caller_file_contains: Vec<String>,
    #[serde(default)]
    pub callee_langs: Vec<String>,
    #[serde(default)]
    pub callee_kinds: Vec<String>,
    #[serde(default)]
    pub callee_file_contains: Vec<String>,
    #[serde(default)]
    pub callee_source_pattern: Option<String>,
    #[serde(default)]
    pub skip_symbol_pattern: Option<String>,
}

fn default_ipc_rules() -> Vec<IpcRuleConfig> {
    vec![IpcRuleConfig {
        name: "tauri_invoke".into(),
        caller_pattern: r#"invoke\s*\(\s*['"](?P<sym>[^'"]+)['"]"#.into(),
        caller_langs: vec!["typescript".into(), "javascript".into()],
        caller_file_extensions: vec![
            "ts".into(),
            "tsx".into(),
            "js".into(),
            "jsx".into(),
            "svelte".into(),
        ],
        caller_file_contains: vec![],
        callee_langs: vec!["rust".into()],
        callee_kinds: vec!["function_item".into()],
        callee_file_contains: vec!["src-tauri".into()],
        callee_source_pattern: Some(
            r"(?m)#\[tauri::command(?:\([^)]*\))?\]|(?m)#\[command(?:\([^)]*\))?\]".into(),
        ),
        skip_symbol_pattern: Some(r"[:|]".into()),
    }]
}

impl From<&IpcRuleConfig> for code_graph::snooper::ipc_engine::IpcRule {
    fn from(c: &IpcRuleConfig) -> Self {
        Self {
            name: c.name.clone(),
            caller_pattern: c.caller_pattern.clone(),
            caller_langs: c.caller_langs.clone(),
            caller_file_extensions: c.caller_file_extensions.clone(),
            caller_file_contains: c.caller_file_contains.clone(),
            callee_langs: c.callee_langs.clone(),
            callee_kinds: c.callee_kinds.clone(),
            callee_file_contains: c.callee_file_contains.clone(),
            callee_source_pattern: c.callee_source_pattern.clone(),
            skip_symbol_pattern: c.skip_symbol_pattern.clone(),
        }
    }
}

/// Agent and MCP behavior configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    pub expert_mode: bool,
    pub default_max_tokens: u32,
    /// **Butler Rank (optional addon):** in-process GNN ranking (`code_graph::gnn`).
    /// Default **false** — Core product does not need weights. See `plans/butler-rank-addon.md`.
    pub use_neural: bool,
    /// Optional path to prebuilt `lambda-eve` release binary (faster than `cargo run`).
    pub lambda_eve_bin: Option<String>,
    /// Path to lambda-eve `Cargo.toml` for `cargo run --release` fallback.
    pub lambda_eve_manifest: Option<String>,
    /// Blend weight for keyword/text match in neural `select_blocks` (default 0.1).
    pub neural_text_weight: f64,
    /// Blend weight for GNN score in neural `select_blocks` (default 0.9).
    pub neural_score_weight: f64,
    /// Fast retriever: top-N keyword matches before hop expansion (default 128).
    pub neural_subgraph_top_n: usize,
    /// Fast retriever: inbound+outbound hops from seeds (default 1).
    pub neural_subgraph_hops: usize,
    /// L0 funnel: max modules (path keyword match) before block-level seed ranking.
    /// `0` disables L0 and scores the full graph at L1. Default 32.
    #[serde(default = "default_neural_l0_modules")]
    pub neural_l0_modules: usize,
}

fn default_neural_l0_modules() -> usize {
    32
}

/// Top-level Butler settings, loaded from .butler/config.toml + env overrides.
#[derive(Debug, Deserialize, Clone)]
pub struct ButlerSettings {
    pub server: ServerConfig,
    pub analysis: AnalysisConfig,
    pub agent: AgentConfig,
    /// Cross-language IPC edge rules (Tauri invoke → Rust command included by default).
    #[serde(default = "default_ipc_rules")]
    pub ipc_rules: Vec<IpcRuleConfig>,
}

impl Default for ButlerSettings {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                port: 8002,
                // A5: loopback by default (open LAN bind needs explicit 0.0.0.0 + password).
                // Docker/compose should set BUTLER__SERVER__HOST=0.0.0.0 when publishing the port.
                host: "127.0.0.1".to_string(),
                username: default_server_username(),
                password: None,
                warm_roots: Vec::new(),
                max_cached_graphs: default_max_cached_graphs(),
                query_cache_cap: default_query_cache_cap(),
                warehouse_idle_secs: default_warehouse_idle_secs(),
                warehouse_last_idle_secs: default_warehouse_last_idle_secs(),
            },
            analysis: {
                // skip_directories: infra + monorepo noise. Bundled-vendor segments
                // (vendor, _vendor, _click, third_party, …) come from
                // code_graph::BUNDLED_VENDOR_DIR_SEGMENTS via apply_bundled_vendor_policy —
                // extend with analysis.extra_bundled_vendor_segments in config.toml.
                let mut analysis = AnalysisConfig {
                    worker_stack_size_mb: 32,
                    skip_directories: vec![
                        ".butler/".to_string(),
                        ".git/".to_string(),
                        "target/".to_string(),
                        "node_modules/".to_string(),
                        "__pycache__/".to_string(),
                        ".cache/".to_string(),
                        "build/".to_string(),
                        "dist/".to_string(),
                        "out/".to_string(),
                        ".idea/".to_string(),
                        ".pytest_cache/".to_string(),
                        ".ruff_cache/".to_string(),
                        ".mypy_cache/".to_string(),
                        ".cargo/".to_string(),
                        "miniconda/".to_string(),
                        "envs/".to_string(),
                        "venv/".to_string(),
                        "conda/".to_string(),
                        ".tox/".to_string(),
                        "site/".to_string(),
                        "emsdk/".to_string(),
                        "binaryen/".to_string(),
                        "llvm-project/".to_string(),
                        // Emscripten / compiler-suite monorepos (not "project" code)
                        "system/lib/libcxx/".to_string(),
                        "system/lib/compiler-rt/".to_string(),
                        "system/lib/libunwind/".to_string(),
                        "system/lib/libc/".to_string(),
                        "system/include/".to_string(),
                        "sanitizer_common/".to_string(),
                        // Test / bench trees — path-segment match via should_scan_path.
                        "tests/".to_string(),
                        "test/".to_string(),
                        "benches/".to_string(),
                        "testdata/".to_string(),
                        // Tutorial / doc-source trees — keep package code only.
                        "docs/".to_string(),
                        "docs_src/".to_string(),
                        "doc/".to_string(),
                        "tutorials/".to_string(),
                        "tutorial/".to_string(),
                        "guides/".to_string(),
                        "examples/".to_string(),
                    ],
                    max_call_graph_depth: 2,
                    trace_max_fan_out: 20,
                    trace_max_visited_nodes: 200,
                    max_context_blocks: default_max_context_blocks(),
                    hub_budget_pct: default_hub_budget_pct(),
                    edge_build_thread_pct: default_edge_build_thread_pct(),
                    noise_path_components: default_noise_path_components(),
                    noise_file_patterns: default_noise_file_patterns(),
                    extra_bundled_vendor_segments: Vec::new(),
                };
                apply_bundled_vendor_policy(&mut analysis);
                analysis
            },
            agent: AgentConfig {
                expert_mode: false,
                default_max_tokens: 4000,
                use_neural: false,
                lambda_eve_bin: None,
                lambda_eve_manifest: None,
                neural_text_weight: 0.1,
                neural_score_weight: 0.9,
                // Smaller seed set → fewer GNN nodes per request (was 500).
                neural_subgraph_top_n: 128,
                neural_subgraph_hops: 1,
                neural_l0_modules: default_neural_l0_modules(),
            },
            ipc_rules: default_ipc_rules(),
        }
    }
}

impl ButlerSettings {
    /// Convert configured IPC rules for the code_graph engine.
    pub fn ipc_rules_for_engine(&self) -> Vec<code_graph::snooper::ipc_engine::IpcRule> {
        self.ipc_rules.iter().map(Into::into).collect()
    }

    /// Merge `ipc_rules` from a project's `.butler/config.toml` (if present).
    pub fn merge_project_config(&mut self, project_root: &std::path::Path) {
        #[derive(Deserialize)]
        struct ProjectOverlay {
            #[serde(default)]
            ipc_rules: Option<Vec<IpcRuleConfig>>,
            #[serde(default)]
            analysis: Option<AnalysisConfigOverlay>,
        }

        #[derive(Deserialize, Default)]
        struct AnalysisConfigOverlay {
            #[serde(default)]
            noise_path_components: Option<Vec<String>>,
            #[serde(default)]
            noise_file_patterns: Option<Vec<String>>,
            #[serde(default)]
            max_context_blocks: Option<usize>,
            #[serde(default)]
            hub_budget_pct: Option<f64>,
            #[serde(default)]
            edge_build_thread_pct: Option<f64>,
        }

        let path = project_root.join(".butler/config.toml");
        if !path.exists() {
            return;
        }
        let builder =
            config::Config::builder().add_source(config::File::from(path).required(false));
        let Ok(cfg) = builder.build() else {
            return;
        };
        let Ok(overlay) = cfg.try_deserialize::<ProjectOverlay>() else {
            return;
        };
        if let Some(rules) = overlay.ipc_rules {
            if !rules.is_empty() {
                self.ipc_rules = rules;
            }
        }
        if let Some(analysis) = overlay.analysis {
            if let Some(v) = analysis.noise_path_components {
                if !v.is_empty() {
                    self.analysis.noise_path_components = v;
                }
            }
            if let Some(v) = analysis.noise_file_patterns {
                if !v.is_empty() {
                    self.analysis.noise_file_patterns = v;
                }
            }
            if let Some(v) = analysis.max_context_blocks {
                if v > 0 {
                    self.analysis.max_context_blocks = v;
                }
            }
            if let Some(v) = analysis.hub_budget_pct {
                if (0.05..=1.0).contains(&v) {
                    self.analysis.hub_budget_pct = v;
                }
            }
            if let Some(v) = analysis.edge_build_thread_pct {
                if (0.1..=1.0).contains(&v) {
                    self.analysis.edge_build_thread_pct = v;
                }
            }
        }
    }

    /// Loads settings with the following priority (lowest to highest):
    /// 1. Hardcoded defaults
    /// 2. Global config (~/.config/butler/config.toml or platform equivalent)
    /// 3. Workspace config (.butler/config.toml in CWD)
    /// 4. Environment variables (BUTLER__SERVER__PORT etc.)
    pub fn new() -> Self {
        let mut builder = Config::builder();

        // 1. Hardcoded defaults using recovery to never panic (graceful fallback)
        let defaults = Self::default();
        {
            let b = builder;
            builder = b
                .clone()
                .set_default("server.port", defaults.server.port)
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default server.port: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default("server.host", defaults.server.host.clone())
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default server.host: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default("server.username", defaults.server.username.clone())
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default server.username: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "analysis.worker_stack_size_mb",
                    defaults.analysis.worker_stack_size_mb,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default analysis.worker_stack_size_mb: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "analysis.skip_directories",
                    defaults.analysis.skip_directories.clone(),
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default analysis.skip_directories: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "analysis.max_call_graph_depth",
                    defaults.analysis.max_call_graph_depth as i64,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default analysis.max_call_graph_depth: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "analysis.trace_max_fan_out",
                    defaults.analysis.trace_max_fan_out as i64,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default analysis.trace_max_fan_out: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "analysis.trace_max_visited_nodes",
                    defaults.analysis.trace_max_visited_nodes as i64,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default analysis.trace_max_visited_nodes: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "analysis.max_context_blocks",
                    defaults.analysis.max_context_blocks as i64,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default analysis.max_context_blocks: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default("analysis.hub_budget_pct", defaults.analysis.hub_budget_pct)
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default analysis.hub_budget_pct: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "analysis.edge_build_thread_pct",
                    defaults.analysis.edge_build_thread_pct,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default analysis.edge_build_thread_pct: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default("agent.expert_mode", defaults.agent.expert_mode)
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default agent.expert_mode: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "agent.default_max_tokens",
                    defaults.agent.default_max_tokens,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default agent.default_max_tokens: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default("agent.use_neural", defaults.agent.use_neural)
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default agent.use_neural: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "agent.neural_text_weight",
                    defaults.agent.neural_text_weight,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default agent.neural_text_weight: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "agent.neural_score_weight",
                    defaults.agent.neural_score_weight,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default agent.neural_score_weight: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "agent.neural_subgraph_top_n",
                    defaults.agent.neural_subgraph_top_n as i64,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default agent.neural_subgraph_top_n: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "agent.neural_subgraph_hops",
                    defaults.agent.neural_subgraph_hops as i64,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default agent.neural_subgraph_hops: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "agent.neural_l0_modules",
                    defaults.agent.neural_l0_modules as i64,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default agent.neural_l0_modules: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "server.max_cached_graphs",
                    defaults.server.max_cached_graphs as i64,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default server.max_cached_graphs: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "server.query_cache_cap",
                    defaults.server.query_cache_cap as i64,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default server.query_cache_cap: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "server.warehouse_idle_secs",
                    defaults.server.warehouse_idle_secs as i64,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default server.warehouse_idle_secs: {}. Continuing.",
                        e
                    );
                    b
                });
        }
        {
            let b = builder;
            builder = b
                .clone()
                .set_default(
                    "server.warehouse_last_idle_secs",
                    defaults.server.warehouse_last_idle_secs as i64,
                )
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: Failed to set default server.warehouse_last_idle_secs: {}. Continuing.",
                        e
                    );
                    b
                });
        }

        // 2. Global config
        if let Some(proj_dirs) = ProjectDirs::from("com", "butler", "butler") {
            let global_config = proj_dirs.config_dir().join("config.toml");
            if global_config.exists() {
                builder = builder.add_source(File::from(global_config).required(false));
            }
        }

        // 3. Workspace config
        let workspace_config: PathBuf = std::path::Path::new(".butler").join("config.toml");
        if workspace_config.exists() {
            builder = builder.add_source(File::from(workspace_config).required(false));
        }

        // 4. Environment variables (BUTLER__SERVER__PORT=9000, BUTLER__AGENT__EXPERT_MODE=true, etc.)
        builder = builder.add_source(
            Environment::with_prefix("BUTLER")
                .separator("__")
                .try_parsing(true),
        );

        match builder.build() {
            Ok(config) => match config.try_deserialize::<ButlerSettings>() {
                Ok(mut settings) => {
                    // Union built-in + extra_bundled_vendor_segments into skip/noise
                    // so config.toml can extend vendor prune without code changes.
                    apply_bundled_vendor_policy(&mut settings.analysis);
                    apply_server_identity_env(&mut settings.server);
                    // Empty password string ⇒ treat as open.
                    if settings
                        .server
                        .password
                        .as_ref()
                        .map(|s| s.trim().is_empty())
                        .unwrap_or(false)
                    {
                        settings.server.password = None;
                    }
                    if settings.server.username.trim().is_empty() {
                        settings.server.username = default_server_username();
                    }
                    settings
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to parse Butler config: {}. Using defaults.",
                        e
                    );
                    Self::default()
                }
            },
            Err(e) => {
                eprintln!(
                    "Warning: Failed to load Butler config sources: {}. Using defaults.",
                    e
                );
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod bundled_vendor_config_tests {
    use super::*;

    #[test]
    fn defaults_include_built_in_bundled_vendor_segments() {
        let s = ButlerSettings::default();
        for seg in code_graph::BUNDLED_VENDOR_DIR_SEGMENTS {
            assert!(
                s.analysis
                    .skip_directories
                    .iter()
                    .any(|p| p.trim_matches('/').eq_ignore_ascii_case(seg)),
                "skip_directories missing built-in vendor segment {seg}"
            );
            assert!(
                s.analysis
                    .noise_path_components
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(seg)),
                "noise_path_components missing built-in vendor segment {seg}"
            );
        }
    }

    #[test]
    fn extra_bundled_vendor_segments_merge_into_skip_and_noise() {
        let mut a = ButlerSettings::default().analysis;
        a.extra_bundled_vendor_segments = vec!["_my_bundle".into(), "thirdparty".into()];
        apply_bundled_vendor_policy(&mut a);
        assert!(a
            .skip_directories
            .iter()
            .any(|p| p.trim_matches('/') == "_my_bundle"));
        assert!(a.noise_path_components.iter().any(|p| p == "thirdparty"));
        // Built-ins still present
        assert!(a
            .skip_directories
            .iter()
            .any(|p| p.trim_matches('/') == "_click"));
    }
}
