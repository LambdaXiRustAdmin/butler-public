//! Core data model for the Snooper code graph: [`Id`], [`BlockInfo`], and [`CodeGraph`].
//!
//! Pure-ish data structures, constructors, accessors, and graph algorithms
//! (add, query, cycle detection, hub computation).
//!
//! **P2 peels:** name map / peer callers → [`super::name_index`]; edge Complete stamp →
//! [`super::edge_lifecycle`]. Background FullEdge telemetry lives in [`super::bg_progress`];
//! heavy edge-build orchestration in `super::builder`.
//!
//! [`BackgroundEdgeBuildState`] / [`BgBuildProgress`] are re-exported here so existing
//! `model::…` paths keep working.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use rayon::prelude::*;

use super::normalize_path;
use super::scanner::cache::{EDGE_SEMANTICS_VERSION, GRAPH_SCHEMA_VERSION};

/// Cached [`CodeGraph::current_trace_epoch`] — O(1) after first compute until invalidated.
#[derive(Debug, Default)]
struct TraceEpochCache {
    epoch: AtomicU64,
    valid: AtomicBool,
}


// Lifecycle telemetry — defined in bg_progress; re-export for `model::` call sites.
pub use super::bg_progress::{BackgroundEdgeBuildState, BgBuildProgress};

/// A stable, content-addressed identifier for any structural element within a codebase.
///
/// Each [`Id`] encodes the file path, entity kind (e.g., `"function_item"`), and an 8-character
/// prefix of the content hash, making it deterministic and collision-resistant for practical purposes.
/// Two blocks with identical file, kind, and source content will always produce the same `Id`.
/// Content-addressed block id. **`Arc<str>`** so edge merge clones are O(1) refcount
/// bumps instead of heap-copying long `file:kind:hash` strings (torch batch merge tax).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(std::sync::Arc<str>);

impl serde::Serialize for Id {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Id {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Id::from_string(s))
    }
}

impl Id {
    /// Creates a new stable identifier from the given file path, entity kind, and content hash.
    ///
    /// The ID format is `{file}:{kind}:{content_hash_prefix}` where `content_hash_prefix`
    /// is the first 8 characters of the full hash. This provides deterministic identification
    /// while keeping IDs compact.
    pub fn new(file: impl AsRef<Path>, kind: &str, content_hash: &str) -> Self {
        let mut file = normalize_path(&file.as_ref().to_string_lossy());
        // Strip absolute mount prefixes so IDs are repo-relative across docker/host.
        // Host/container roots come only from env (no personal home paths).
        // `/projects/` is the in-container convention used by the stack compose files.
        let mut abs_prefixes: Vec<String> = Vec::new();
        if let Ok(c) = std::env::var("BUTLER_CONTAINER_MOUNT") {
            if !c.is_empty() {
                abs_prefixes.push(format!("{}/", c.trim_end_matches('/')));
            }
        }
        if let Ok(h) = std::env::var("BUTLER_HOST_MOUNT") {
            if !h.is_empty() {
                abs_prefixes.push(format!("{}/", h.trim_end_matches('/')));
            }
        }
        if !abs_prefixes.iter().any(|p| p == "/projects/") {
            abs_prefixes.push("/projects/".to_string());
        }
        for prefix in &abs_prefixes {
            if let Some(stripped) = file.strip_prefix(prefix.as_str()) {
                file = stripped.to_string();
                break;
            }
        }
        file = file.trim_start_matches('/').to_string();
        // Twin leak after absolute strip: `test_repos/<proj>/src/main.c` vs `src/main.c`.
        // Drop `test_repos/<proj>/` only when a layout segment follows (src/, lib/, …),
        // not `test_repos/<proj>/<pkg>/…` where <pkg> may equal <proj> (typer/typer/main.py).
        // Full collapse still runs via CodeGraph::canonize_identity when project root is known.
        if let Some(rest) = file.strip_prefix("test_repos/") {
            // rest = "<proj>/…"
            if let Some((proj, after)) = rest.split_once('/') {
                let first = after.split('/').next().unwrap_or("");
                let layoutish = matches!(
                    first,
                    "src" | "lib" | "tools" | "crates" | "packages" | "include" | "apps"
                        | "bin" | "cmd" | "tests" | "test" | "benches" | "docs" | "scripts"
                );
                // Always drop test_repos/<proj>/ when after is layout OR when after still
                // starts with proj/ (test_repos/typer/typer/main.py → typer/main.py).
                if layoutish || first == proj || after.contains('/') {
                    file = after.to_string();
                }
            }
        }
        let hash = if content_hash.len() >= 8 {
            &content_hash[0..8]
        } else {
            content_hash
        };
        let s = format!("{}:{}:{}", file, kind, hash);
        Self(std::sync::Arc::from(s))
    }

    /// Returns the string representation of this identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Build from an owned string (tests / remaps). Interns into `Arc<str>`.
    pub fn from_string(s: String) -> Self {
        Self(std::sync::Arc::from(s))
    }
}

impl From<&str> for Id {
    fn from(s: &str) -> Self {
        Self(std::sync::Arc::from(s))
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Core node in the bidirectional code graph.
///
/// Each `BlockInfo` represents a structural element extracted from source code —
/// such as a function, struct, enum, trait, or module — along with its source snippet,
/// positional metadata (lines/bytes), and computed properties like content hash and score.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockInfo {
    /// Stable, content-addressed identifier for this block.
    pub id: Id,
    /// Human-readable name (e.g., `"process_request"`, `"HttpClient"`).
    pub name: String,
    /// File path where this block is defined.
    pub file: PathBuf,
    /// Structural kind (e.g., `"function_item"`, `"struct_item"`, `"impl_item"`).
    pub kind: String,
    /// Language identifier (e.g., `"rust"`, `"python"`).
    pub lang: String,
    /// 1-based start line number.
    pub start_line: usize,
    /// 1-based end line number (inclusive).
    pub end_line: usize,
    /// Byte offset of the block's start within the file.
    pub start_byte: usize,
    /// Byte offset of the block's end within the file.
    pub end_byte: usize,
    /// ID of the parent block (e.g., the impl block containing a method).
    pub parent_id: Option<Id>,
    /// IDs of child blocks (nested items within this block).
    pub children: Vec<Id>,
    /// BLAKE3 hash of normalized source content.
    pub content_hash: String,
    /// Signature hash (name-based, params/return reserved for future).
    pub sig_hash: String,
    /// Seconds since last commit that modified this block (lazy-computed).
    pub git_blame_recency: Option<u64>,
    /// Author of the last commit that modified this block.
    pub git_author: Option<String>,
    /// Whether this block participates in a cycle in the call graph.
    pub has_cycle: bool,
    /// Whether this block is the result of macro expansion.
    pub is_macro_expanded: bool,
    /// Source snippet. **May be empty** after slim cache / progressive strip —
    /// hydrate from disk via [`CodeGraph::hydrate_block_sources`] before compose.
    pub source: String,
    /// Computed relevance score for context selection (set by collector/context).
    pub score: f64,
    /// Locations where this block is referenced/called.
    pub usages: Vec<(usize, String)>,
    /// External crates used by this block (extracted from `use` statements and derives).
    pub external_crates: HashSet<String>,
    /// Whether this node is in the top ~5% by total degree (callers + children).
    pub is_highly_connected: bool,
}

impl BlockInfo {
    /// Creates a new block with computed hashes and default values.
    ///
    /// The content hash and signature hash are derived from the source and name respectively.
    /// Git blame information is intentionally left as `None` — it is computed lazily on demand
    /// to avoid thousands of `Repository::discover` + `blame_file` calls during initial scan
    /// of large repos (e.g., rust-lang/rust).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        file: impl Into<PathBuf>,
        kind: &str,
        lang: &str,
        start_line: usize,
        end_line: usize,
        start_byte: usize,
        end_byte: usize,
        source: String,
        name: &str,
        external_crates: HashSet<String>,
    ) -> Self {
        let file = file.into(); // own the PathBuf once

        // Demoscene: streaming hasher. Feed trimmed bytes directly. Zero 10KB+ intermediate String.
        let mut hasher = blake3::Hasher::new();
        let mut last_was_blank = false;
        for line in source.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                if !last_was_blank {
                    hasher.update(b"\n");
                    last_was_blank = true;
                }
            } else {
                hasher.update(trimmed.as_bytes());
                hasher.update(b"\n");
                last_was_blank = false;
            }
        }
        let content_hash = hasher.finalize().to_hex().to_string();
        let sig_hash = blake3::hash(name.as_bytes()).to_hex().to_string();
        let id = Id::new(&file, kind, &content_hash);

        Self {
            id,
            name: name.to_string(),
            file,
            kind: kind.to_string(),
            lang: lang.to_string(),
            start_line,
            end_line,
            start_byte,
            end_byte,
            parent_id: None,
            children: Vec::new(),
            content_hash,
            sig_hash,
            git_blame_recency: None,
            git_author: None,
            score: 0.0, // default; will be computed by collector/context
            has_cycle: false,
            is_macro_expanded: false,
            source,
            usages: vec![],
            external_crates,
            is_highly_connected: false,
        }
    }

    /// Metadata-only copy (no source text) — for slim cache / assembly without the warehouse.
    pub fn without_source(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            file: self.file.clone(),
            kind: self.kind.clone(),
            lang: self.lang.clone(),
            start_line: self.start_line,
            end_line: self.end_line,
            start_byte: self.start_byte,
            end_byte: self.end_byte,
            parent_id: self.parent_id.clone(),
            children: self.children.clone(),
            content_hash: self.content_hash.clone(),
            sig_hash: self.sig_hash.clone(),
            git_blame_recency: self.git_blame_recency,
            git_author: self.git_author.clone(),
            score: self.score,
            has_cycle: self.has_cycle,
            is_macro_expanded: self.is_macro_expanded,
            source: String::new(),
            usages: self.usages.clone(),
            external_crates: self.external_crates.clone(),
            is_highly_connected: self.is_highly_connected,
        }
    }

    /// Drop source text in place (free RAM after edges re-read from disk).
    pub fn strip_source(&mut self) {
        self.source.clear();
        self.source.shrink_to_fit();
    }

    /// Load source span from disk into `self.source` if empty.
    pub fn hydrate_source_from_disk(&mut self, project_root: &Path) -> bool {
        if !self.source.is_empty() {
            return true;
        }
        if self.file.as_os_str().is_empty() {
            return false;
        }
        // Repo-relative warehouse paths → abs under project root.
        let abs = crate::snooper::project_paths::ProjectPaths::new(project_root).to_abs(&self.file);
        let Ok(full) = std::fs::read_to_string(&abs) else {
            return false;
        };
        if self.start_byte < full.len() && self.end_byte <= full.len() && self.start_byte < self.end_byte
        {
            self.source = full[self.start_byte..self.end_byte].to_string();
            true
        } else if self.start_line > 0 && self.end_line >= self.start_line {
            // Fallback: line range (1-based inclusive)
            let lines: Vec<&str> = full.lines().collect();
            let s = self.start_line.saturating_sub(1).min(lines.len());
            let e = self.end_line.min(lines.len());
            if s < e {
                self.source = lines[s..e].join("\n");
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Zero-bloat constructor for synthetic crate nodes (HIT 12).
    /// Replaces the 30+ line manual struct literal in get_or_create_crate_id.
    pub fn new_crate(name: &str) -> Self {
        let id_str = format!("crate:{}", name);
        let id = Id::new("", "crate", &id_str);
        BlockInfo {
            id,
            name: name.to_string(),
            file: PathBuf::new(),
            kind: "crate".to_string(),
            lang: "rust".to_string(),
            start_line: 0,
            end_line: 0,
            start_byte: 0,
            end_byte: 0,
            parent_id: None,
            children: Vec::new(),
            content_hash: String::new(),
            sig_hash: String::new(),
            git_blame_recency: None,
            git_author: None,
            score: 0.0,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            usages: vec![],
            external_crates: Default::default(),
            is_highly_connected: false,
        }
    }
}

/// The bidirectional spine of the code graph.
///
/// One symbol hit for exact-name lookup (rg-shaped; no source text).
/// Persisted in `name_index.bin` and kept in RAM as a secondary index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct NameLocation {
    pub id: Id,
    pub name: String,
    pub file: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
    pub kind: String,
    pub lang: String,
}

/// Result of a full O(n) [`CodeGraph::audit_name_index`] pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameIndexAudit {
    pub nodes_len: usize,
    pub stamp_nodes_len: usize,
    pub name_keys: usize,
    pub named_nodes: usize,
    pub indexed_locs: usize,
    pub missing_from_index: usize,
    pub orphan_locs: usize,
    pub name_mismatches: usize,
    pub sample_missing: Vec<String>,
}

impl NameIndexAudit {
    /// True when stamp matches, every named node is indexed, no orphans/mismatches.
    pub fn is_ok(&self) -> bool {
        self.stamp_nodes_len == self.nodes_len
            && self.missing_from_index == 0
            && self.orphan_locs == 0
            && self.name_mismatches == 0
            && self.named_nodes == self.indexed_locs
    }
}

/// Maintains a collection of [`BlockInfo`] nodes along with forward edges (call/usage)
/// and reverse edges (called-by). Also tracks dependency versions and highly-connected
/// hub nodes for special handling during context composition.
///
/// # Lazy Edge Building
///
/// Edges are built on-demand via [`super::builder`] (ensure_call_graph / run_background_full_edge_build)
/// rather than during initial scanning. When called on a graph with no edges, it re-walks the
/// source files and builds call/usage edges using Tree-sitter queries. This makes the initial
/// scan significantly faster since expensive query operations are deferred.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CodeGraph {
    /// All nodes indexed by their stable [`Id`].
    pub nodes: HashMap<Id, BlockInfo>,
    /// Forward edges: caller → callee (**same-lang CALL** / structural implements).
    /// Cross-stack glue lives in [`bridge_fwd`] — do not mix unlabeled FFI into CALL.
    pub edges: HashMap<Id, Vec<Id>>,
    /// Reverse edges: callee → callers (reverse CALL relationships).
    pub reverse: HashMap<Id, Vec<Id>>,
    /// Typed interconnect bridges: from → [(to, kind)] (Export / Ipc / Twin).
    /// Track P.1 — not CALL; Trace stamps `relation` from kind.
    #[serde(default)]
    pub bridge_fwd: HashMap<Id, Vec<(Id, super::interconnect::BridgeKind)>>,
    /// Reverse bridges: to → [(from, kind)].
    #[serde(default)]
    pub bridge_rev: HashMap<Id, Vec<(Id, super::interconnect::BridgeKind)>>,
    /// Crate name → version mapping, loaded lazily from `cargo metadata`.
    pub dependency_versions: HashMap<String, String>,
    /// Nodes that belong to the top ~5% by total degree (callers + children).
    /// Populated once at scan time by [`CodeGraph::compute_hubs`].
    pub highly_connected_nodes: HashSet<Id>,
    /// Monotonic counter incremented on every successful `update_single_file`.
    /// Consumers (watchers, dashboards, etc.) can watch this to know the graph has changed
    /// without comparing large collections.
    pub version: u64,
    /// Per-file content hashes (computed with std DefaultHasher) for deterministic
    /// cache invalidation on load_graph (replaces fragile mtime checks). Enables
    /// precise detection of adds/mods/deletes even when Butler is not running.
    /// Keyed by the path string (as produced by WalkDir + read during scan).
    #[serde(default)]
    pub file_hashes: HashMap<String, u64>,

    /// Per-module content hashes (folder / package unit inside the repo).
    /// Keyed by parent directory of source files (normalized `/`). Value is a fold of
    /// the sorted `(relpath, file_hash)` pairs under that module. Rebuilt from
    /// [`file_hashes`] via [`CodeGraph::rebuild_module_hashes`]. Used for coarse
    /// invalidation, L0 funnels, and scope keys without re-walking every file.
    #[serde(default)]
    pub module_hashes: HashMap<String, u64>,

    /// Exact symbol name → all locations (secondary index). Rebuilt from `nodes`
    /// or loaded from `name_index.bin`. Skipped in graph.bin serde; reconstruct after load.
    #[serde(skip, default)]
    pub name_index: HashMap<String, Vec<NameLocation>>,

    /// `nodes.len()` at last successful [`rebuild_name_index`]. O(1) stale check:
    /// growth (merge_wave, progressive publish, partial load) without rebuild → mismatch.
    /// Serde-skip: set on rebuild / after load audit; 0 means "unknown / never stamped".
    #[serde(skip, default)]
    pub name_index_nodes_len: usize,

    /// Normalized file path → node ids (secondary index for file-local scope collect).
    /// Built with [`rebuild_name_index`]. Serde-skip; O(files_in_scope) Arch gather.
    #[serde(skip, default)]
    pub file_node_index: HashMap<String, Vec<Id>>,

    /// Stamp: `nodes.len()` when `file_node_index` was last rebuilt (same pass as name_index).
    #[serde(skip, default)]
    pub file_node_index_nodes_len: usize,

    // === Eager Skeleton + Background Edge Build + JIT fields (long-term cold-start architecture) ===
    /// Files for which full call/usage edges have been built (by background or JIT surgical).
    /// Not in graph.bin (serde skip); durable copy in `.butler/cache/edge_status.bin`.
    #[serde(skip)]
    pub files_with_edges: HashSet<PathBuf>,
    /// Set when FullEdge stream has closed every edgeable file slot (O(1) inventory complete).
    /// Avoids re-deriving inventory from 1M+ nodes after collect / at Complete stamp.
    #[serde(skip, default)]
    pub edge_inventory_closed: bool,
    /// True once the background full edge build has completed.
    /// Not in graph.bin; durable in `edge_status.bin` + restored on load.
    #[serde(skip)]
    pub background_edge_build_complete: bool,
    /// Explicit state machine for background edge build.
    #[serde(skip)]
    pub background_edge_build_state: BackgroundEdgeBuildState,
    /// True while a background edge-build thread is actively running for this graph.
    #[serde(skip)]
    pub background_edge_build_active: bool,

    /// Monotonic atomic counter of work units (files/blocks processed) completed inside the
    /// Rayon edge-build par_iter loops (both background full and JIT ensure_call_graph).
    /// Read under the graph lock (or via Arc clone) to compute live progress % for the
    /// "=== Building Graph (XX%) ===" marker returned to MCP clients during polling.
    /// Stored as Arc<AtomicUsize> so concurrent relaxed loads see updates without holding
    /// any graph RwLock across the CPU-heavy rayon work.
    #[serde(skip)]
    pub edges_built_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,

    /// Prompt-heuristic scores captured before lambda-eve overwrites `BlockInfo.score` (diagnostics + blend).
    #[serde(skip, default)]
    pub heuristic_score_cache: HashMap<Id, f64>,
    /// GNN (SmartButler) scores populated in-process by code_graph::gnn.
    #[serde(skip, default)]
    pub neural_score_cache: HashMap<Id, f64>,

    /// Cached per-lang name→Id maps for CALL resolution (built once per node set).
    /// Invalidated when nodes change (`invalidate_call_name_maps`). Edge-only updates keep it.
    /// **`Arc`** so FullEdge batches share maps without cloning multi‑M HashMaps (gecko OOM).
    #[serde(skip, default)]
    pub call_name_maps: Option<std::sync::Arc<CallNameMaps>>,

    /// Hot fingerprint for Trace path-memo keys (file inventory + edge completeness).
    /// Shared via Arc so Clone is cheap; invalidated on structure mutations.
    #[serde(skip)]
    trace_epoch: Arc<TraceEpochCache>,

    /// On-disk product language not Butler-scanned (e.g. Java monorepo → JS crumbs).
    /// When set, serve must refuse product Trace/Arch rather than fake hubs.
    /// See [`crate::snooper::warehouse_lang`].
    #[serde(default)]
    pub warehouse_lang_void: Option<crate::snooper::warehouse_lang::WarehouseLangVoid>,

    /// Session-only: Export/Ipc/Twin interconnect already injected for this process load.
    /// Prevents re-running full FFI/IPC/TS maps on every Trace (single-thread write-lock death).
    #[serde(skip, default)]
    pub interconnect_session_ready: bool,
}

/// Per-language global name → block id maps for same-lang call edge resolution.
/// Built from nodes; safe to reuse across edge-only mutations.
#[derive(Debug, Clone, Default)]
pub struct CallNameMaps {
    pub python: HashMap<String, Id>,
    pub rust: HashMap<String, Id>,
    pub c_family: HashMap<String, Id>,
    pub go: HashMap<String, Id>,
    /// All Go call-target ids per short name (for package-qualified resolve).
    /// Single-winner [`Self::go`] stays for bare-name / method best-effort.
    pub go_all: HashMap<String, Vec<Id>>,
    pub typescript: HashMap<String, Id>,
    pub other: HashMap<String, Id>,
}

/// File extension → call-edge language family (same-lang name maps only).
pub fn call_edge_family_for_ext(ext: &str) -> &'static str {
    match ext {
        "py" => "python",
        "rs" => "rust",
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" | "C" => "c_family",
        "go" => "go",
        "ts" | "tsx" | "js" | "jsx" | "svelte" => "typescript",
        _ => "other",
    }
}

impl CallNameMaps {
    pub fn for_ext(&self, ext: &str) -> &HashMap<String, Id> {
        match call_edge_family_for_ext(ext) {
            "python" => &self.python,
            "rust" => &self.rust,
            "c_family" => &self.c_family,
            "go" => &self.go,
            "typescript" => &self.typescript,
            _ => &self.other,
        }
    }
}

impl CodeGraph {
    /// Creates a new empty code graph.
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: HashMap::new(),
            reverse: HashMap::new(),
            bridge_fwd: HashMap::new(),
            bridge_rev: HashMap::new(),
            dependency_versions: HashMap::new(),
            highly_connected_nodes: HashSet::new(),
            version: 0,
            file_hashes: HashMap::new(),
            module_hashes: HashMap::new(),
            name_index: HashMap::new(),
            name_index_nodes_len: 0,
            file_node_index: HashMap::new(),
            file_node_index_nodes_len: 0,
            files_with_edges: HashSet::new(),
            edge_inventory_closed: false,
            background_edge_build_complete: false,
            background_edge_build_state: BackgroundEdgeBuildState::NotStarted,
            background_edge_build_active: false,
            edges_built_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            heuristic_score_cache: HashMap::new(),
            neural_score_cache: HashMap::new(),
            call_name_maps: None,
            trace_epoch: Arc::new(TraceEpochCache::default()),
            warehouse_lang_void: None,
            interconnect_session_ready: false,
        }
    }

    /// Drop cached CALL name maps (call after any node add/remove/replace).
    pub fn invalidate_call_name_maps(&mut self) {
        self.call_name_maps = None;
    }

    /// Invalidate Trace memo epoch (structure or edge inventory changed).
    #[inline]
    pub fn invalidate_trace_epoch(&self) {
        self.trace_epoch.valid.store(false, Ordering::Release);
    }

    /// O(1) warehouse fingerprint for Trace path-memo keys when warm.
    ///
    /// Includes schema/edge-sem versions, inventory + build completeness, sizes, and
    /// sorted `file_hashes`. Recomputes only after [`invalidate_trace_epoch`].
    pub fn current_trace_epoch(&self) -> u64 {
        if self.trace_epoch.valid.load(Ordering::Acquire) {
            return self.trace_epoch.epoch.load(Ordering::Relaxed);
        }
        let epoch = self.compute_trace_epoch();
        self.trace_epoch.epoch.store(epoch, Ordering::Relaxed);
        self.trace_epoch.valid.store(true, Ordering::Release);
        epoch
    }

    fn compute_trace_epoch(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        GRAPH_SCHEMA_VERSION.hash(&mut h);
        EDGE_SEMANTICS_VERSION.hash(&mut h);
        self.is_edge_inventory_complete().hash(&mut h);
        self.is_edge_build_complete().hash(&mut h);
        self.nodes.len().hash(&mut h);
        self.total_edges().hash(&mut h);
        // L2.2: PostPass typed bridges must bust Trace memo (empty-bridge tours after LTO).
        self.total_bridge_edges().hash(&mut h);
        // Commutative mix — avoid O(files log files) sort of 32k+ paths on first Trace.
        // Epoch only needs change detection, not sorted order.
        self.file_hashes.len().hash(&mut h);
        let mut acc = 0u64;
        for (p, hash) in &self.file_hashes {
            let mut h2 = std::collections::hash_map::DefaultHasher::new();
            p.hash(&mut h2);
            hash.hash(&mut h2);
            acc ^= h2.finish();
        }
        acc.hash(&mut h);
        h.finish()
    }


    /// Force all block.file / file_hashes keys to repo-relative under `project_root`.
    ///
    /// **Identity pass:** also rebuilds [`Id`]s from (rel_path, kind, content_hash) and
    /// remaps edges so path twins (`src/main.c` vs `test_repos/sqlite/src/main.c`) collapse
    /// into one node. Without rekeying, edges hang off a parallel universe id while Trace
    /// prefers another — zero callees despite successful edge build.
    pub fn normalize_paths_to_root(&mut self, project_root: &Path) {
        self.canonize_identity(project_root);
    }

    /// Collapse path twins: one repo-relative file form + one Id per (file, kind, content).
    pub fn canonize_identity(&mut self, project_root: &Path) {
        let pp = crate::snooper::project_paths::ProjectPaths::new(project_root);

        let old_nodes = std::mem::take(&mut self.nodes);
        let mut id_map: HashMap<Id, Id> = HashMap::with_capacity(old_nodes.len());
        let mut new_nodes: HashMap<Id, BlockInfo> = HashMap::with_capacity(old_nodes.len());

        for (old_id, mut b) in old_nodes {
            b.file = pp.to_rel(&b.file);
            // content_hash may be short in tests — pad for Id::new
            let hash = if b.content_hash.len() >= 8 {
                b.content_hash.clone()
            } else {
                format!("{:0<16}", b.content_hash)
            };
            let new_id = Id::new(&b.file, &b.kind, &hash);
            id_map.insert(old_id, new_id.clone());

            if let Some(existing) = new_nodes.get_mut(&new_id) {
                // Twin: keep richer source / higher score; edges merge via id_map
                if b.score > existing.score
                    || (b.score == existing.score && b.source.len() > existing.source.len())
                {
                    let keep_children = std::mem::take(&mut existing.children);
                    *existing = b;
                    existing.id = new_id.clone();
                    existing.children = keep_children;
                }
            } else {
                b.id = new_id.clone();
                new_nodes.insert(new_id, b);
            }
        }

        // Remap parent/children AST links
        for b in new_nodes.values_mut() {
            if let Some(p) = b.parent_id.take() {
                b.parent_id = id_map.get(&p).cloned();
            }
            b.children = b
                .children
                .iter()
                .filter_map(|c| id_map.get(c).cloned())
                .collect();
            b.children.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            b.children.dedup();
        }

        // Remap adjacency (merge twins)
        let mut new_edges: HashMap<Id, Vec<Id>> = HashMap::new();
        for (from, tos) in std::mem::take(&mut self.edges) {
            let nf = id_map.get(&from).cloned().unwrap_or(from);
            for to in tos {
                let nt = id_map.get(&to).cloned().unwrap_or(to);
                if nf == nt {
                    continue;
                }
                new_edges.entry(nf.clone()).or_default().push(nt);
            }
        }
        for outs in new_edges.values_mut() {
            outs.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            outs.dedup();
        }
        let mut new_rev: HashMap<Id, Vec<Id>> = HashMap::new();
        for (from, tos) in &new_edges {
            for to in tos {
                new_rev.entry(to.clone()).or_default().push(from.clone());
            }
        }
        for ins in new_rev.values_mut() {
            ins.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            ins.dedup();
        }

        // file_hashes / files_with_edges keys
        let mut fh = HashMap::with_capacity(self.file_hashes.len());
        for (k, v) in std::mem::take(&mut self.file_hashes) {
            fh.insert(pp.key(k), v);
        }
        self.file_hashes = fh;

        let fwe: HashSet<PathBuf> = std::mem::take(&mut self.files_with_edges)
            .into_iter()
            .map(|p| pp.to_rel(p))
            .collect();
        self.files_with_edges = fwe;

        let hubs: HashSet<Id> = std::mem::take(&mut self.highly_connected_nodes)
            .into_iter()
            .filter_map(|id| id_map.get(&id).cloned())
            .collect();
        self.highly_connected_nodes = hubs;

        self.nodes = new_nodes;
        self.edges = new_edges;
        self.reverse = new_rev;
        self.rebuild_module_hashes();
        self.rebuild_name_index();
        self.version = self.version.saturating_add(1);
    }


    /// Content hash using only std::collections::hash_map::DefaultHasher (no extra crates).
    /// Used for deterministic per-file cache invalidation.
    pub(crate) fn content_hash(content: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        hasher.finish()
    }

    /// Rebuild [`module_hashes`] from current [`file_hashes`].
    ///
    /// Module key = parent directory of each source file (normalized). This is the
    /// "per-module hash inside the repo" used for coarse delta / L0 funnels.
    pub fn rebuild_module_hashes(&mut self) {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use std::path::Path;

        let mut buckets: HashMap<String, Vec<(String, u64)>> = HashMap::new();
        for (path, &h) in &self.file_hashes {
            let p = path.replace('\\', "/");
            let module = Path::new(&p)
                .parent()
                .map(|d| {
                    let s = d.to_string_lossy().replace('\\', "/");
                    if s.is_empty() {
                        ".".to_string()
                    } else {
                        s
                    }
                })
                .unwrap_or_else(|| ".".to_string());
            buckets.entry(module).or_default().push((p, h));
        }

        let mut out = HashMap::with_capacity(buckets.len());
        for (module, mut files) in buckets {
            files.sort_by(|a, b| a.0.cmp(&b.0));
            let mut hasher = DefaultHasher::new();
            module.hash(&mut hasher);
            for (f, h) in &files {
                f.hash(&mut hasher);
                h.hash(&mut hasher);
            }
            out.insert(module, hasher.finish());
        }
        self.module_hashes = out;
    }

    /// Module directory for a source path (parent dir, normalized).
    pub fn module_key_for_path(path: &str) -> String {
        use std::path::Path;
        let p = path.replace('\\', "/");
        Path::new(&p)
            .parent()
            .map(|d| {
                let s = d.to_string_lossy().replace('\\', "/");
                if s.is_empty() {
                    ".".to_string()
                } else {
                    s
                }
            })
            .unwrap_or_else(|| ".".to_string())
    }
}

fn normalize_bridge_list(v: &mut Vec<(Id, super::interconnect::BridgeKind)>) {
    if v.is_empty() {
        return;
    }
    v.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.rank().cmp(&a.1.rank())));
    // Keep first per neighbor id (highest rank after sort).
    let mut w = 0usize;
    for i in 0..v.len() {
        if w == 0 || v[i].0 != v[w - 1].0 {
            v[w] = v[i].clone();
            w += 1;
        }
    }
    v.truncate(w);
}

impl CodeGraph {
    /// Returns the children (callees) of a block — **CALL adjacency only**.
    pub fn children(&self, id: &Id) -> Vec<Id> {
        self.edges.get(id).cloned().unwrap_or_default()
    }

    /// Returns the callers of a block — **CALL adjacency only**.
    pub fn callers(&self, id: &Id) -> Vec<Id> {
        self.reverse.get(id).cloned().unwrap_or_default()
    }


    /// Bridge callees (from → to) with kinds.
    pub fn bridge_children(&self, id: &Id) -> Vec<(Id, super::interconnect::BridgeKind)> {
        self.bridge_fwd.get(id).cloned().unwrap_or_default()
    }

    /// Bridge callers (to ← from) with kinds.
    pub fn bridge_callers(&self, id: &Id) -> Vec<(Id, super::interconnect::BridgeKind)> {
        self.bridge_rev.get(id).cloned().unwrap_or_default()
    }

    /// Kind of a direct bridge edge between two nodes, if any.
    pub fn bridge_kind_between(
        &self,
        from: &Id,
        to: &Id,
    ) -> Option<super::interconnect::BridgeKind> {
        self.bridge_fwd.get(from).and_then(|v| {
            v.iter()
                .filter(|(id, _)| id == to)
                .max_by_key(|(_, k)| k.rank())
                .map(|(_, k)| *k)
        })
    }

    /// Drop all typed bridges (edge-sem rebuild / clear).
    pub fn clear_bridges(&mut self) {
        self.bridge_fwd.clear();
        self.bridge_rev.clear();
        self.invalidate_trace_epoch();
    }

    /// Insert a typed interconnect bridge (not CALL).
    pub fn add_bridge_edge(
        &mut self,
        from: Id,
        to: Id,
        kind: super::interconnect::BridgeKind,
    ) {
        self.bridge_fwd
            .entry(from.clone())
            .or_default()
            .push((to.clone(), kind));
        self.bridge_rev.entry(to).or_default().push((from, kind));
        self.invalidate_trace_epoch();
    }

    /// Batch-insert typed bridges (blind append; normalize at LTO boundary).
    pub fn add_bridge_edges_batch(
        &mut self,
        edges: impl IntoIterator<Item = (Id, Id, super::interconnect::BridgeKind)>,
    ) {
        let mut n = 0usize;
        for (from, to, kind) in edges {
            self.bridge_fwd
                .entry(from.clone())
                .or_default()
                .push((to.clone(), kind));
            self.bridge_rev.entry(to).or_default().push((from, kind));
            n += 1;
        }
        if n > 0 {
            self.invalidate_trace_epoch();
        }
    }

    /// Retrieves a block by its ID, if it exists.
    pub fn get_block(&self, id: Id) -> Option<&BlockInfo> {
        self.nodes.get(&id)
    }

    /// Inserts a block into the graph.
    pub fn add_block(&mut self, block: BlockInfo) {
        self.nodes.insert(block.id.clone(), block);
    }

    /// Adds a directed edge from one block to another.
    ///
    /// **Blind append** into contiguous `Vec` adjacency (data-oriented). Duplicates
    /// are allowed until [`normalize_adjacency`] (sort + dedup) at an LTO boundary.
    /// This is **not** RAG embedding math — just dense integer/string neighbor lists.
    pub fn add_edge(&mut self, from: Id, to: Id) {
        self.edges.entry(from.clone()).or_default().push(to.clone());
        self.reverse.entry(to).or_default().push(from);
        self.invalidate_trace_epoch();
    }

    /// Batch-insert edges via **blind append** (no online HashSet / `contains` dedup).
    ///
    /// Map phase may already unique edges; reduce must stay O(batch). Call
    /// [`normalize_adjacency`] before Complete / save when uniqueness is required.
    pub fn add_edges_batch(&mut self, edges: impl IntoIterator<Item = (Id, Id)>) {
        let edges: Vec<(Id, Id)> = edges.into_iter().collect();
        self.add_edges_batch_vec(edges);
    }

    /// Preferred entry for background builds that already hold a `Vec`.
    pub fn add_edges_batch_vec(&mut self, edges: Vec<(Id, Id)>) {
        if edges.is_empty() {
            return;
        }
        // Blind push — CPUs love contiguous append; avoid O(deg) contains / HashSet rebuild.
        for (from, to) in edges {
            self.edges.entry(from.clone()).or_default().push(to.clone());
            self.reverse.entry(to).or_default().push(from);
        }
        self.invalidate_trace_epoch();
    }

    /// Sort-and-squish every adjacency list (parallel over nodes).
    ///
    /// Game-engine / compiler LTO boundary: after all PostPass reduces, once, before
    /// Complete + disk save. Contiguous sorted `Vec` is what Trace / GNN / flat maps walk.
    pub fn normalize_adjacency(&mut self) {
        let t0 = std::time::Instant::now();
        let mut fwd: Vec<&mut Vec<Id>> = self.edges.values_mut().collect();
        fwd.par_iter_mut().for_each(|v| {
            v.sort_unstable();
            v.dedup();
        });
        let mut rev: Vec<&mut Vec<Id>> = self.reverse.values_mut().collect();
        rev.par_iter_mut().for_each(|v| {
            v.sort_unstable();
            v.dedup();
        });
        // Bridges: sort by neighbor id; keep highest-rank kind per neighbor.
        for v in self.bridge_fwd.values_mut() {
            normalize_bridge_list(v);
        }
        for v in self.bridge_rev.values_mut() {
            normalize_bridge_list(v);
        }
        println!(
            "📐 Adjacency sort-and-squish: {} fwd / {} rev lists (bridges {}/{}) in {:.2?}",
            self.edges.len(),
            self.reverse.len(),
            self.bridge_fwd.len(),
            self.bridge_rev.len(),
            t0.elapsed()
        );
        self.invalidate_trace_epoch();
    }

    /// Returns the actual total number of directed edges in the graph
    /// (sum of all adjacency list lengths). Use this instead of `edges.len()`
    /// (which only counts distinct source nodes with outgoing edges).
    pub fn total_edges(&self) -> usize {
        self.edges.values().map(|v| v.len()).sum()
    }

    /// Count of typed interconnect bridges (forward list lengths).
    pub fn total_bridge_edges(&self) -> usize {
        self.bridge_fwd.values().map(|v| v.len()).sum()
    }

    /// Detects cycles in the call graph using iterative DFS over weakly-connected
    /// components in **parallel** (rayon). Marks `has_cycle` on back-edge sources.
    pub fn detect_cycles(&mut self) {
        if self.nodes.is_empty() {
            return;
        }

        let node_ids: Vec<Id> = self.nodes.keys().cloned().collect();
        let n = node_ids.len();
        let id_to_idx: HashMap<Id, usize> = node_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i))
            .collect();

        // Parallel adjacency (indices only).
        let adj: Vec<Vec<usize>> = (0..n)
            .into_par_iter()
            .map(|i| {
                let id = &node_ids[i];
                self.edges.get(id).map_or_else(Vec::new, |outs| {
                    outs.iter()
                        .filter_map(|nid| id_to_idx.get(nid).copied())
                        .collect()
                })
            })
            .collect();

        // Union-Find on undirected view → independent components for parallel DFS.
        let mut parent: Vec<usize> = (0..n).collect();
        let mut rank = vec![0u8; n];
        #[inline]
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]];
                x = parent[x];
            }
            x
        }
        #[inline]
        fn union(parent: &mut [usize], rank: &mut [u8], a: usize, b: usize) {
            let mut ra = find(parent, a);
            let mut rb = find(parent, b);
            if ra == rb {
                return;
            }
            if rank[ra] < rank[rb] {
                std::mem::swap(&mut ra, &mut rb);
            }
            parent[rb] = ra;
            if rank[ra] == rank[rb] {
                rank[ra] = rank[ra].saturating_add(1);
            }
        }
        for i in 0..n {
            for &j in &adj[i] {
                union(&mut parent, &mut rank, i, j);
            }
        }
        let mut comps: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..n {
            let r = find(&mut parent, i);
            comps.entry(r).or_default().push(i);
        }
        let components: Vec<Vec<usize>> = comps.into_values().collect();

        let in_cycle: Vec<AtomicBool> = (0..n).map(|_| AtomicBool::new(false)).collect();

        components.par_iter().for_each(|comp| {
            if comp.is_empty() {
                return;
            }
            // Local membership for O(1) neighbor filter within component.
            let member: HashSet<usize> = comp.iter().copied().collect();
            let mut visited = HashSet::new();
            let mut rec_stack = HashSet::new();

            for &start in comp {
                if visited.contains(&start) {
                    continue;
                }
                let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
                let mut path: Vec<usize> = vec![start];
                visited.insert(start);
                rec_stack.insert(start);

                while let Some((curr, child_idx)) = stack.last_mut() {
                    let neighbors = &adj[*curr];
                    if *child_idx < neighbors.len() {
                        let neigh = neighbors[*child_idx];
                        *child_idx += 1;
                        if !member.contains(&neigh) {
                            continue;
                        }
                        if rec_stack.contains(&neigh) {
                            in_cycle[*curr].store(true, Ordering::Relaxed);
                            continue;
                        }
                        if !visited.contains(&neigh) {
                            visited.insert(neigh);
                            rec_stack.insert(neigh);
                            path.push(neigh);
                            stack.push((neigh, 0));
                        }
                    } else {
                        stack.pop();
                        if let Some(p) = path.pop() {
                            rec_stack.remove(&p);
                        }
                    }
                }
            }
        });

        for (i, flag) in in_cycle.iter().enumerate() {
            if flag.load(Ordering::Relaxed) {
                if let Some(block) = self.nodes.get_mut(&node_ids[i]) {
                    block.has_cycle = true;
                }
            }
        }
    }

    /// Computes highly-connected hubs (top `top_percent` by total degree).
    ///
    /// Marks nodes in the top tier by total connections (in-degree + out-degree) as
    /// "highly connected". These are flagged on both the [`BlockInfo`] (`is_highly_connected`)
    /// and in the graph's `highly_connected_nodes` set. The composer uses this to apply
    /// special handling that prevents context explosion when traversing into library-wide hubs.
    pub fn compute_hubs(&mut self, top_percent: f64) {
        if self.nodes.is_empty() {
            return;
        }

        // Purify plumbing nodes (serial — mut on values).
        for node in self.nodes.values_mut() {
            let k = node.kind.to_lowercase();
            if k.contains("primitive")
                || k.contains("builtin")
                || k.contains("macro")
                || node.name.len() <= 3
            {
                node.score = 0.0;
                node.is_highly_connected = false;
            }
        }

        // Degree collection in parallel (read-only maps).
        let mut degs: Vec<(Id, usize)> = self
            .nodes
            .par_iter()
            .filter_map(|(id, node)| {
                let k = node.kind.to_lowercase();
                if k.contains("primitive")
                    || k.contains("builtin")
                    || k.contains("macro")
                    || node.name.len() <= 3
                {
                    return None;
                }
                let in_deg = self.reverse.get(id).map_or(0, |v| v.len());
                let out_deg = self.edges.get(id).map_or(0, |v| v.len());
                Some((id.clone(), in_deg + out_deg))
            })
            .collect();

        if degs.is_empty() {
            return;
        }

        degs.par_sort_unstable_by(|a, b| b.1.cmp(&a.1));

        let cutoff = ((top_percent * degs.len() as f64) as usize)
            .max(1)
            .min(degs.len());
        let threshold = if cutoff > 0 { degs[cutoff - 1].1 } else { 0 };

        self.highly_connected_nodes.clear();

        for (id, degree) in &degs {
            if let Some(block) = self.nodes.get_mut(id) {
                block.score = *degree as f64;
                if *degree >= threshold {
                    self.highly_connected_nodes.insert(id.clone());
                    block.is_highly_connected = true;
                }
            }
        }
    }

    /// Unified finalization step for full build lifecycles.
    /// Replaces scattered sequential detect_cycles + compute_hubs calls (HIT 10)
    /// to prevent lifecycle ordering bugs.
    pub fn finalize_build(&mut self) {
        self.detect_cycles();
        self.compute_hubs(0.05);
        self.rebuild_name_index();
    }

    /// Drop all source blobs (RAM). Edge build re-reads files from disk.
    pub fn strip_all_sources(&mut self) {
        for b in self.nodes.values_mut() {
            b.strip_source();
        }
    }

    /// True when no block still holds source text (post–Phase-1 strip / slim snapshot).
    #[inline]
    pub fn sources_stripped(&self) -> bool {
        self.nodes.values().all(|b| b.source.is_empty())
    }

    /// Slim clone for cache/save: full structure, empty sources (no multi-GB photocopy).
    pub fn slim_for_cache(&self) -> Self {
        // Already stripped (progressive scan waves): one structure clone beats per-node
        // without_source rebuilds on multi‑M pytorch-class graphs.
        if self.sources_stripped() {
            return self.snapshot_for_publish();
        }
        let mut g = Self {
            nodes: HashMap::with_capacity(self.nodes.len()),
            edges: self.edges.clone(),
            reverse: self.reverse.clone(),
            bridge_fwd: self.bridge_fwd.clone(),
            bridge_rev: self.bridge_rev.clone(),
            dependency_versions: self.dependency_versions.clone(),
            highly_connected_nodes: self.highly_connected_nodes.clone(),
            version: self.version,
            file_hashes: self.file_hashes.clone(),
            module_hashes: self.module_hashes.clone(),
            name_index: self.name_index.clone(),
            name_index_nodes_len: self.name_index_nodes_len,
            file_node_index: self.file_node_index.clone(),
            file_node_index_nodes_len: self.file_node_index_nodes_len,
            files_with_edges: self.files_with_edges.clone(),
            edge_inventory_closed: self.edge_inventory_closed,
            background_edge_build_complete: self.background_edge_build_complete,
            background_edge_build_state: self.background_edge_build_state,
            background_edge_build_active: self.background_edge_build_active,
            edges_built_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(
                self.edges_built_count
                    .load(std::sync::atomic::Ordering::Relaxed),
            )),
            heuristic_score_cache: HashMap::new(),
            neural_score_cache: HashMap::new(),
            // Don't photocopy multi‑M name maps into slim cache clones.
            call_name_maps: None,
            // Fresh epoch cache (structure may share stamp once computed).
            trace_epoch: Arc::new(TraceEpochCache::default()),
            warehouse_lang_void: self.warehouse_lang_void.clone(),
            interconnect_session_ready: false,
        };
        for (id, b) in &self.nodes {
            g.nodes.insert(id.clone(), b.without_source());
        }
        if g.name_index.is_empty() || !g.file_node_index_is_warm() {
            g.rebuild_name_index();
        }
        g
    }

    /// Serve/publish snapshot when sources are already empty (Phase-1 strip).
    ///
    /// Cheaper than a second per-block `without_source` walk: structure clone + drop
    /// transient caches. If any source remains, falls back to [`slim_for_cache`].
    pub fn snapshot_for_publish(&self) -> Self {
        if !self.sources_stripped() {
            // Avoid infinite recursion: force the node walk path.
            let mut g = Self {
                nodes: HashMap::with_capacity(self.nodes.len()),
                edges: self.edges.clone(),
                reverse: self.reverse.clone(),
                bridge_fwd: self.bridge_fwd.clone(),
                bridge_rev: self.bridge_rev.clone(),
                dependency_versions: self.dependency_versions.clone(),
                highly_connected_nodes: self.highly_connected_nodes.clone(),
                version: self.version,
                file_hashes: self.file_hashes.clone(),
                module_hashes: self.module_hashes.clone(),
                name_index: self.name_index.clone(),
                name_index_nodes_len: self.name_index_nodes_len,
                file_node_index: self.file_node_index.clone(),
                file_node_index_nodes_len: self.file_node_index_nodes_len,
                files_with_edges: self.files_with_edges.clone(),
                edge_inventory_closed: self.edge_inventory_closed,
                background_edge_build_complete: self.background_edge_build_complete,
                background_edge_build_state: self.background_edge_build_state,
                background_edge_build_active: self.background_edge_build_active,
                edges_built_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(
                    self.edges_built_count
                        .load(std::sync::atomic::Ordering::Relaxed),
                )),
                heuristic_score_cache: HashMap::new(),
                neural_score_cache: HashMap::new(),
                call_name_maps: None,
                trace_epoch: Arc::new(TraceEpochCache::default()),
                warehouse_lang_void: self.warehouse_lang_void.clone(),
                interconnect_session_ready: false,
            };
            for (id, b) in &self.nodes {
                g.nodes.insert(id.clone(), b.without_source());
            }
            if g.name_index.is_empty() || !g.file_node_index_is_warm() {
                g.rebuild_name_index();
            }
            return g;
        }
        let mut g = self.clone();
        g.heuristic_score_cache.clear();
        g.neural_score_cache.clear();
        g.call_name_maps = None;
        g.edges_built_count = Arc::new(std::sync::atomic::AtomicUsize::new(
            self.edges_built_count
                .load(std::sync::atomic::Ordering::Relaxed),
        ));
        g.trace_epoch = Arc::new(TraceEpochCache::default());
        g
    }

    /// Hydrate `source` for the given blocks from disk (project root absolute).
    pub fn hydrate_block_sources(&self, project_root: &Path, blocks: &mut [BlockInfo]) {
        for b in blocks.iter_mut() {
            let _ = b.hydrate_source_from_disk(project_root);
        }
    }

    /// Merge another graph's nodes/hashes into self (progressive wave-2 fill).
    pub fn merge_wave(&mut self, other: CodeGraph) {
        self.nodes.extend(other.nodes);
        self.file_hashes.extend(other.file_hashes);
        self.module_hashes.clear();
        self.rebuild_module_hashes();
        self.rebuild_name_index();
        self.version = self.version.saturating_add(1);
        self.invalidate_trace_epoch();
    }
}


#[cfg(test)]
mod identity_canon_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn id_new_strips_test_repos_project_prefix() {
        let id = Id::new(
            "test_repos/sqlite/src/main.c",
            "function_definition",
            "deadbeefdeadbeef",
        );
        assert!(
            id.as_str().starts_with("src/main.c:"),
            "got {}",
            id.as_str()
        );
    }

    #[test]
    fn id_new_keeps_package_dir_when_named_like_project() {
        let id = Id::new(
            "test_repos/typer/typer/main.py",
            "class_definition",
            "deadbeefdeadbeef",
        );
        assert!(
            id.as_str().starts_with("typer/main.py:"),
            "got {}",
            id.as_str()
        );
        let id2 = Id::new("typer/main.py", "class_definition", "deadbeefdeadbeef");
        assert!(
            id2.as_str().starts_with("typer/main.py:"),
            "got {}",
            id2.as_str()
        );
    }

    #[test]
    fn canonize_collapses_path_twins_and_merges_edges() {
        let mut g = CodeGraph::new();
        let hash = "aabbccddeeff0011";
        let mut good = BlockInfo::new(
            PathBuf::from("src/main.c"),
            "function_definition",
            "c",
            10,
            20,
            0,
            10,
            "int foo(void) { bar(); return 0; }".into(),
            "foo",
            HashSet::new(),
        );
        good.content_hash = hash.into();
        good.id = Id::new("src/main.c", "function_definition", hash);

        let mut twin = good.clone();
        twin.file = PathBuf::from("test_repos/sqlite/src/main.c");
        // Simulate pre-fix Id that kept the twin prefix
        twin.id = Id::from_string(format!(
            "test_repos/sqlite/src/main.c:function_definition:{}",
            &hash[0..8]
        ));

        let mut bar = BlockInfo::new(
            PathBuf::from("src/main.c"),
            "function_definition",
            "c",
            30,
            40,
            100,
            120,
            "int bar(void) { return 1; }".into(),
            "bar",
            HashSet::new(),
        );
        let bar_hash = "1122334455667788";
        bar.content_hash = bar_hash.into();
        bar.id = Id::new("src/main.c", "function_definition", bar_hash);

        // Edge hangs off the twin universe
        g.nodes.insert(twin.id.clone(), twin);
        g.nodes.insert(bar.id.clone(), bar.clone());
        g.add_edge(
            Id::from_string(format!(
                "test_repos/sqlite/src/main.c:function_definition:{}",
                &hash[0..8]
            )),
            bar.id.clone(),
        );

        // Also insert the "good" path form without edges (Trace preferred)
        g.nodes.insert(good.id.clone(), good.clone());

        let root = PathBuf::from("/projects/test_repos/sqlite");
        g.canonize_identity(&root);

        assert_eq!(
            g.nodes
                .values()
                .filter(|b| b.name == "foo")
                .count(),
            1,
            "twins must collapse"
        );
        let foo = g
            .nodes
            .values()
            .find(|b| b.name == "foo")
            .expect("foo");
        assert_eq!(foo.file, PathBuf::from("src/main.c"));
        let kids = g.children(&foo.id);
        assert!(
            kids.iter().any(|id| g.nodes.get(id).map(|b| b.name.as_str()) == Some("bar")),
            "edges must reattach to preferred/canonical id; kids={kids:?}"
        );
    }

    #[test]
    fn preferred_sqlite_open_holds_open_database_child() {
        let Some(path) = crate::resolve_optional_test_repo("sqlite/src/main.c") else {
            eprintln!("skip: no sqlite fixture on disk");
            return;
        };
        if !path.exists() {
            eprintln!("skip: no sqlite fixture on disk");
            return;
        }
        let source = std::fs::read_to_string(&path).expect("read main.c");
        let Some(root) = crate::resolve_optional_test_repo("sqlite") else {
            return;
        };
        let rel = PathBuf::from("src/main.c");
        let parsed = crate::snooper::lang::c::parse(rel.clone(), &source).expect("parse");
        let mut g = CodeGraph::new();
        for b in &parsed.blocks {
            if b.kind == "function_definition" || b.kind == "function_declaration" {
                g.nodes.insert(b.id.clone(), b.clone());
            }
        }
        // Inject a twin preferred-looking node path form without edges
        if let Some(open) = parsed
            .blocks
            .iter()
            .find(|b| b.name == "sqlite3_open" && b.kind == "function_definition")
        {
            let mut twin = open.clone();
            twin.file = PathBuf::from("test_repos/sqlite/src/main.c");
            twin.id = Id::from_string(format!(
                "test_repos/sqlite/src/main.c:function_definition:{}",
                &open.content_hash[..8.min(open.content_hash.len())]
            ));
            g.nodes.insert(twin.id.clone(), twin);
        }
        let tree = parsed.tree.as_ref().unwrap();
        let edges = crate::snooper::lang::c::collect_call_edges(
            &parsed.blocks,
            &source,
            tree,
            None,
        );
        g.add_edges_batch(edges);
        g.canonize_identity(&root);

        let preferred = g
            .nodes
            .values()
            .filter(|b| b.name == "sqlite3_open" && b.kind == "function_definition")
            .max_by_key(|b| b.source.len())
            .expect("sqlite3_open def");
        assert_eq!(preferred.file, PathBuf::from("src/main.c"));
        let child_names: Vec<_> = g
            .children(&preferred.id)
            .iter()
            .filter_map(|id| g.nodes.get(id).map(|b| b.name.as_str()))
            .collect();
        assert!(
            child_names.contains(&"openDatabase"),
            "preferred id must hold openDatabase edge after identity pass; kids={child_names:?}"
        );
        assert_eq!(
            g.nodes
                .values()
                .filter(|b| b.name == "sqlite3_open" && b.kind == "function_definition")
                .count(),
            1,
            "exactly one sqlite3_open def after collapse"
        );
    }
}

#[cfg(test)]
mod cycle_hub_tests {
    use super::*;

    fn blk(name: &str) -> BlockInfo {
        let hash = format!("{name:0<16}"); // Id::new needs ≥8 hash chars
        BlockInfo {
            id: Id::new("t.rs", "function_item", &hash),
            name: name.into(),
            file: PathBuf::from("t.rs"),
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

    #[test]
    fn blind_batch_append_then_sort_and_squish() {
        let mut g = CodeGraph::new();
        let a = blk("aaa_fn");
        let b = blk("bbb_fn");
        let ia = a.id.clone();
        let ib = b.id.clone();
        g.add_block(a);
        g.add_block(b);
        // Duplicates allowed until normalize.
        g.add_edges_batch(vec![
            (ia.clone(), ib.clone()),
            (ia.clone(), ib.clone()),
            (ia.clone(), ib.clone()),
        ]);
        assert_eq!(g.total_edges(), 3);
        g.normalize_adjacency();
        assert_eq!(g.total_edges(), 1);
        assert_eq!(g.children(&ia).len(), 1);
        assert_eq!(g.callers(&ib).len(), 1);
    }

    #[test]
    fn detect_cycles_marks_back_edge_source() {
        let mut g = CodeGraph::new();
        let a = blk("aaa_fn");
        let b = blk("bbb_fn");
        let ia = a.id.clone();
        let ib = b.id.clone();
        g.add_block(a);
        g.add_block(b);
        g.add_edge(ia.clone(), ib.clone());
        g.add_edge(ib, ia); // cycle
        g.detect_cycles();
        assert!(
            g.nodes.values().any(|n| n.has_cycle),
            "expected a cycle mark on a↔b"
        );
    }

    #[test]
    fn compute_hubs_sets_degree_scores() {
        let mut g = CodeGraph::new();
        let hub = blk("hub_fn");
        let ih = hub.id.clone();
        g.add_block(hub);
        for i in 0..10 {
            let x = blk(&format!("leaf{i}_fn"));
            let xid = x.id.clone();
            g.add_block(x);
            g.add_edge(ih.clone(), xid);
        }
        g.compute_hubs(0.2);
        assert!(g.nodes.get(&ih).is_some_and(|b| b.score > 0.0));
    }
}

/// Formats a git blame recency value (in seconds) into a human-readable string.
pub fn format_recency(recency: Option<u64>) -> String {
    recency
        .map(|secs| {
            if secs < 60 {
                "just now".to_string()
            } else if secs < 3600 {
                format!("{} mins ago", secs / 60)
            } else if secs < 86400 {
                format!("{} hours ago", secs / 3600)
            } else {
                format!("{} days ago", secs / 86400)
            }
        })
        .unwrap_or("unknown".to_string())
}

/// Compute a stable hex string key for GNN plan cache (and similar scope caches).
///
/// Uses the existing per-file content hashes (file_hashes in the CodeGraph / snooper)
/// for files whose paths fall under the provided scope_paths.
/// Combines with normalized project root and the scope_paths themselves.
///
/// This provides a consistent, content-based (not just mtime) key so that
/// Butler's per-file hashing drives Eve's plan invalidation.
pub fn compute_scope_hash(project: &str, scope_paths: &[String]) -> String {
    // When called from contexts that have the live CodeGraph, use its file_hashes
    // via the CodeGraph method below. This free fn provides a default using
    // empty hashes (for early bootstrap or toy cases); real calls go through
    // the graph instance or server state that holds the authoritative file_hashes.
    compute_scope_hash_with_hashes(project, scope_paths, &std::collections::HashMap::new())
}

fn compute_scope_hash_with_hashes(
    project: &str,
    scope_paths: &[String],
    file_hashes: &std::collections::HashMap<String, u64>,
) -> String {
    let proj_norm = project.replace('\\', "/");
    let scope_norm: Vec<String> = scope_paths.iter().map(|s| s.replace('\\', "/")).collect();
    let scope_key = scope_norm.join("|");

    // Filter to entries under any of the scopes (or all if no/empty scope)
    let mut selected: Vec<(String, u64)> = file_hashes
        .iter()
        .filter(|(p, _)| {
            let pp = p.replace('\\', "/");
            if scope_norm.is_empty()
                || scope_norm
                    .iter()
                    .any(|s| s.is_empty() || s == "." || s == "./" || s == "/")
            {
                true
            } else {
                scope_norm.iter().any(|s| pp.starts_with(s))
            }
        })
        .map(|(p, h)| (p.clone(), *h))
        .collect();

    selected.sort();

    // Canonical data: project | scope_key | (relpath: content_hash) pairs
    let mut data: Vec<u8> = vec![];
    data.extend(proj_norm.as_bytes());
    data.push(0);
    data.extend(scope_key.as_bytes());
    data.push(0);
    for (f, h) in &selected {
        data.extend(f.as_bytes());
        data.push(0);
        data.extend(&h.to_le_bytes());
        data.push(0);
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

impl CodeGraph {
    /// Instance method that uses this CodeGraph's authoritative per-file content hashes.
    pub fn compute_scope_hash(&self, project: &str, scope_paths: &[String]) -> String {
        compute_scope_hash_with_hashes(project, scope_paths, &self.file_hashes)
    }

    /// Coarse scope key from per-module hashes (faster than full file fold when scopes are wide).
    pub fn compute_module_scope_hash(&self, project: &str, scope_paths: &[String]) -> String {
        compute_scope_hash_with_hashes(project, scope_paths, &self.module_hashes)
    }
}
