use serde::{Deserialize, Serialize};

/// Request payload for `POST /warm` — register project roots in the live graph map.
#[derive(Deserialize, Clone, Default)]
pub struct WarmRequest {
    /// Single root (alias of first entry in `roots`).
    #[serde(default)]
    pub root: Option<String>,
    /// One or more project roots to warm (absolute paths preferred).
    #[serde(default)]
    pub roots: Vec<String>,
}

/// Response for `POST /warm`.
#[derive(Serialize)]
pub struct WarmResponse {
    pub ok: bool,
    pub warmed: Vec<String>,
    pub message: String,
}

/// Request payload for the `POST /context` endpoint.
///
/// This struct defines all parameters the LLM client can provide to control context retrieval.
/// All fields are optional except that at least one of [`root`](ContextRequest::root) or
/// [`project`](ContextRequest::project) should be provided (enforced in [`run_context_logic`]).
///
/// # Surgical Mode
/// When both [`target_file`](ContextRequest::target_file) and [`target_line`](ContextRequest::target_line)
/// are set, the server bypasses keyword-based selection entirely and returns context for the
/// exact source line specified. The [`prompt`](ContextRequest::prompt) field is ignored in this case.
///
/// # Example (keyword search)
/// ```json
/// {
///   "project": "/path/to/your-repo",
///   "prompt": "rate_limit middleware",
///   "depth": 2,
///   "max_tokens": 4000
/// }
/// ```
///
/// # Example (surgical mode)
/// ```json
/// {
///   "project": "/home/user/my-project",
///   "target_file": "src/handlers/auth.rs",
///   "target_line": 17,
///   "mode": "surgical",
///   "depth": 1
/// }
/// ```
#[derive(Deserialize, Clone)]
pub struct ContextRequest {
    /// Keywords for context retrieval (not full natural language).
    /// For surgical tracing (mod/line), this field is **completely optional**.
    /// The whole point of surgical mode is that you can target a specific line
    /// without knowing what is there.
    #[serde(default)]
    pub prompt: String,

    #[serde(default = "default_root")]
    pub root: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default = "default_depth")]
    pub depth: usize,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    #[serde(default = "default_compress")]
    pub compress_tests: bool,
    #[serde(default)]
    pub full_module: bool,

    /// For mod/line (surgical) input: the file containing the target line.
    /// Must be used with target_line.
    #[serde(default)]
    pub target_file: Option<String>,

    /// For mod/line (surgical) input: the exact line number.
    /// The response will contain the actual source text of that line + its direct call graph edges.
    #[serde(default)]
    pub target_line: Option<usize>,

    /// Context retrieval mode.
    /// Use "surgical" together with target_file + target_line for mod/line tracing.
    /// Other modes: balanced, implementation, architecture, compressed, mini.
    #[serde(default)]
    pub mode: Option<String>,

    /// For butler_orchestrate: the goal from JSON schema ("goal"). Properly mapped by mcp.rs forwarding
    /// (preserved as "goal" + copied to "mode" for BC). Strict case-insens matching used in orchestrate arm.
    #[serde(default, alias = "goal")]
    pub goal: Option<String>,

    /// Symbol to Trace/Find. MCP `butler_ask` may send `symbol` or `target_symbol`.
    #[serde(default, alias = "symbol")]
    pub target_symbol: Option<String>,

    /// Working Set: restrict to these path prefixes (e.g. ["src/", "crates/core/"]).
    #[serde(default)]
    pub scope_paths: Option<Vec<String>>,
    /// Working Set: exclude these path prefixes.
    #[serde(default)]
    pub ignore_paths: Option<Vec<String>>,

    /// Hop continuity (Soft I4): previous seed when chaining A→B→Trace(B).
    /// If that name is a real CALL parent of ★, Butler force-includes it in the
    /// callers **sample** (does not dump the full hub reverse).
    /// Aliases: `origin_symbol`, `focus` (string).
    #[serde(default, alias = "origin_symbol", alias = "focus")]
    pub focus_symbol: Option<String>,

    /// Optional multi-focus parents (same inject rule as [`Self::focus_symbol`]).
    #[serde(default)]
    pub focus_symbols: Option<Vec<String>>,

    /// Explicit multi-hop neighborhood depth (1–2 only). When set, overrides
    /// default depth for Trace blast; values &gt;2 are clamped to 2 (not full BFS).
    /// Omit for existing `depth` behavior (also hard-capped at 2).
    #[serde(default)]
    pub expand_hops: Option<u8>,

    /// Soft I4 sample window: skip N ranked candidates (per side) before packing.
    /// Use when the first sample is wrong for the job — not a full reverse dump.
    /// Clamped (server max 500). Banner reports offset + omitted honesty.
    #[serde(default)]
    pub sample_offset: Option<u32>,

    /// Drop these neighbor names from the sample window (e.g. prior sample).
    /// Exact name or `::name` suffix. Cap 64. Does not remove warehouse edges.
    #[serde(default, alias = "exclude_callers")]
    pub exclude_symbols: Option<Vec<String>>,

    /// Sample ranking strategy: `score` (default) or `diverse` (stronger parent-dir diversity).
    #[serde(default)]
    pub sample_mode: Option<String>,

    /// Content + neighbor-sample length (agent chooses — no mind-reading).
    /// - **short** / `compact` (default): trust dossier + tight Trace sample (orient/pin).
    /// - **long** / `dense` / `full` / `verbose`: full dump + larger neighbor sample (edit under pin).
    /// Honesty identical both ways (degrees, omitted, mega-hub notes). Machine report always in structured.
    #[serde(default, alias = "verbosity")]
    pub detail: Option<String>,

    /// Free-form structural hint from `butler_ask` (alias of prompt / optional query field).
    #[serde(default)]
    pub query: Option<String>,

    /// Continue past the cold-open **soft wall** (default 15 minutes; `BUTLER_SOFT_WALL_SECS`).
    /// Without this, long builds return `status: BUILDING_SOFT_WALL` instead of silent forever-retry.
    /// Agents: only set true after operator/policy consent ("are you sure?").
    #[serde(default)]
    pub confirm_long_wait: Option<bool>,

    /// Max results for search tool (default 8).
    #[serde(default = "default_max_results")]
    pub max_results: usize,

    /// Internal: the MCP tool name that originated the call (set by mcp.rs bridge for dispatch).
    /// Allows server to know whether this is butler_search / inspect / map / context.
    #[serde(default, alias = "mcp_tool_name")]
    pub mcp_tool_name: Option<String>,
}

fn default_root() -> String {
    std::env::var("BUTLER_ROOT").unwrap_or_else(|_| ".".to_string())
}
/// Returns the default root path, falling back to `BUTLER_ROOT` env var or `"."`.
fn default_depth() -> usize {
    2
}
/// Default call-graph depth for context retrieval.
fn default_max_tokens() -> usize {
    4000
}
/// Maximum token budget for context output (conservative estimate).
fn default_compress() -> bool {
    true
}
/// Default max results for the `butler_search` tool.
fn default_max_results() -> usize {
    8
}

/// JSON response body for the `POST /context` endpoint.
///
/// Always returns HTTP 200 OK — errors and warnings are communicated through structured fields
/// rather than HTTP status codes. This allows MCP clients to inspect the warning field and
/// handle issues gracefully without network-level error handling.
///
/// # Fields
/// - `content`: The main response text (context output, error message, or instructions)
/// - `selected_count`: Number of code blocks included in the context
/// - `warning`: Optional diagnostic information (e.g., `"graph_building"`, `"missing_project"`)
/// - `token_count`: Actual token count from the composer (for client-side budgeting)
/// - `mode`: The effective retrieval mode used (e.g., `"Balanced"`, `"Surgical"`)
/// - `blocks_omitted`: Number of blocks that were considered but excluded due to token limits
#[derive(Serialize, Clone)]
pub struct ContextResponse {
    pub content: String,
    pub selected_count: usize,
    pub warning: Option<String>,

    // Rich metadata from the new composer (exposed for MCP / advanced clients)
    pub token_count: Option<usize>,
    pub mode: Option<String>,
    pub blocks_omitted: Option<usize>,

    // Benchmark / telemetry fields (populated during context/orchestrate for clients)
    pub graph_time_ms: Option<u64>,
    pub cached: Option<bool>,
    pub total_time_ms: Option<u64>,

    // Plain text Mermaid markup generated by server (for diagrams)
    pub mermaid: Option<String>,

    // Native JSON object for MCP/clients (orchestrate report). Never a JSON string.
    pub structured: Option<serde_json::Value>,
}

/// Serde default for `CallerCallee::hop` (rustc does not see serde uses).
#[inline]
pub(crate) fn default_hop() -> u8 {
    1
}

#[derive(Serialize, Clone)]
pub struct CallerCallee {
    pub name: String,
    pub file: String,
    pub line: usize,
    /// Call-graph hop from Trace seed: **1 = direct edge**, **2+ = transitive** (L2 blast).
    /// Flattened multi-hop lists must not be read as all-direct callees/callers.
    #[serde(default = "default_hop")]
    pub hop: u8,
    /// Normalized language (python, rust, c, go, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Workbench cluster badge (shell:py, core:c, core:rs, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
    /// Cross-lang interconnect label for Trace UI (`ffi` when neighbor lang ≠ target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
    /// Short source snippet for cite-to-user trust (≤6 lines / ~400 chars). Empty when stripped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cite: Option<String>,
    /// T.1c why-edge: short proof for top neighbors when honest signal exists (silence otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// Native orchestrate report (HTTP `structured` / MCP `structuredContent`).
#[derive(Serialize)]
pub struct StateInfo {
    pub edge_build: String,
    pub jit: String,
    /// Agent confidence ladder: `inventory` | `index_exact` | `edges_partial` | `edges_full`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    /// Edge-build percent (0–100). Cache/rewalk key material for honest partials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<usize>,
}

/// Track T.1 — agent **trust receipt** on every Trace (and when useful on errors).
///
/// *Faster to trust than grep is to run* — make confidence explicit, not lore.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TraceReceipt {
    /// Agent trust band: `high` | `medium` | `low`.
    pub confidence: String,
    /// Completeness ladder: `edges_full` | `edges_partial` | `index_exact` | `inventory`.
    pub ladder: String,
    /// How the neighborhood was justified (honest; never invent):
    /// Graph-level: `bare-name` | `type-neighborhood` | `location-only` | `disambiguate` |
    /// `bridge-export` | `bridge-ipc` | `bridge-twin` | `error`.
    /// Neighbor-level (compact text): `call` | `transitive` | `export` | `ipc` | `twin` | `ffi`
    /// | `name_peer` (same-name peer reverse — not CALL into ★).
    /// Reserved until stored: `import-bound` | `barrel-walk` (do not emit without edge tag).
    pub basis: String,
    /// `complete` | `partial@N%` | `building`.
    pub edges: String,
}

#[derive(Serialize)]
pub struct TargetInfo {
    pub name: String,
    pub file: String,
    pub line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
}

/// One exact-name hit (rg-shaped). From name_index / graph nodes.
#[derive(Serialize, Clone)]
pub struct SymbolLocation {
    pub name: String,
    pub file: String,
    pub line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    pub kind: String,
    /// True when this row is the preferred Find/Trace target.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preferred: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct Hub {
    pub name: String,
    pub file: String,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
}

/// One language workbench over the shared warehouse (not a separate graph).
#[derive(Serialize, Clone)]
pub struct ClusterInfo {
    /// Stable id: c_cpp, rust, go, python, typescript, other
    pub id: String,
    /// Human label: "Python shell", "C/C++ core", …
    pub label: String,
    /// Short badge: shell:py, core:c, …
    pub badge: String,
    pub nodes: usize,
    pub files: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<String>,
}

/// Cross-cluster edge (polyglot / FFI interconnect).
#[derive(Serialize, Clone)]
pub struct BridgeInfo {
    pub from_name: String,
    pub from_file: String,
    pub from_lang: String,
    pub from_cluster: String,
    pub to_name: String,
    pub to_file: String,
    pub to_lang: String,
    pub to_cluster: String,
}

/// One of the top interior picks when a module shell was resolved (dense / structured only).
#[derive(Debug, Clone, Serialize)]
pub struct ModuleInteriorCandidate {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub kind: String,
    pub rank_score: f64,
}

#[derive(Serialize)]
pub struct StructuredReport {
    pub state: StateInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub target: Option<TargetInfo>,
    /// Same-lang **CALL** neighbors (hop-aware). Signature-change blast radius.
    /// Direct reverse into the ★ seed only — not same-name peers.
    pub callers: Vec<CallerCallee>,
    pub callees: Vec<CallerCallee>,
    /// Reverse CALL spine: seed's parent…ancestors toward entry (not including seed).
    /// Compact: "call path (reverse spine)". CALL edges only; empty when no tight pipeline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caller_path: Vec<CallerCallee>,
    /// Callers of **other** same-name defs (`relation=name_peer`) — **not** CALL into ★.
    /// Twin-id recovery for agents; do not treat as signature-change parents of the pin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peer_callers: Vec<CallerCallee>,
    /// Typed interconnect neighbors (Export / Ipc / Twin) — **not** CALL.
    /// `relation` is `export` | `ipc` | `twin`. Track P.4.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bridge_callers: Vec<CallerCallee>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bridge_callees: Vec<CallerCallee>,
    /// Product domain: `call` (function edges) | `type_neighborhood` (not full ABI/layout).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blast_domain: Option<String>,
    /// Seed AST kind (`function_item`, `struct_item`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_kind: Option<String>,
    /// Track T.1 trust receipt — always set on Trace emission via `attach_trace_receipt`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<TraceReceipt>,
    /// Track T.3 — concrete next step for agents (miss / BUILDING / disambiguate / empty symbol).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    pub telemetry: serde_json::Value,
    pub suggested_scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hubs: Option<Vec<Hub>>,
    /// e.g. `"walk"` when `mod walk;` was opened into walk.rs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_resolved_from: Option<String>,
    /// Top interior candidates (winner first); only when module resolve ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_interior_candidates: Option<Vec<ModuleInteriorCandidate>>,
    /// Exact-name hit list (rg-shaped). Preferred target also in `target`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<SymbolLocation>>,
    /// Language workbenches present in scope (size-sorted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clusters: Option<Vec<ClusterInfo>>,
    /// Cross-cluster bridges (polyglot fabric / Arch view).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridges: Option<Vec<BridgeInfo>>,
    /// Active cluster for Trace (target's workbench).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_cluster: Option<String>,
}

/// JSON response body for the `GET /projects` endpoint.
///
/// Lists all discovered projects (directories containing a `Cargo.toml`) under
/// `BUTLER_PROJECTS_ROOT`. Used by LLM clients to discover available codebases.
#[derive(Serialize)]
pub struct ProjectsResponse {
    pub projects: Vec<String>,
    pub count: usize,
}

/// MCP tool manifest response for LLM discovery.
///
/// Served at `GET /mcp/manifest`. Describes the available Butler tools, their schemas,
/// and how to call them. LLM clients (Claude Desktop, Cursor) fetch this automatically
/// during MCP connection setup to learn what tools are available.
#[derive(Serialize)]
pub struct McpManifest {
    pub name: String,
    pub description: String,
    pub version: String,
    pub base_url: String,
    pub tools: Vec<McpTool>,
}

/// Describes a single MCP tool within the manifest.
///
/// Contains the tool's name, description, HTTP method/path, and JSON Schema for its input parameters.
/// LLM clients use this to construct valid API calls to the server.
#[derive(Serialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub method: String,
    pub path: String,
    pub input_schema: serde_json::Value,
}

/// Per-workspace background edge-build status (authoritative decoupled telemetry).
#[derive(Serialize)]
pub struct EdgeBuildHealthEntry {
    pub percent: usize,
    pub state: String,
    pub live: bool,
    /// Coarse FullEdge phase (`inventory`, `streaming`, `write_wait:merge_batch_3`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Why the worker is not progressing (write wait, heartbeat stale, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
    /// Seconds since last heartbeat (or job start).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_age_s: Option<u64>,
}

/// Global FullEdge concurrency governor (per-root lanes share this cap).
#[derive(Serialize)]
pub struct FullEdgeGovernorHealth {
    /// Max concurrent FullEdge jobs process-wide (`BUTLER_FULLEDGE_PARALLEL`).
    pub max: usize,
    /// Currently running FullEdge jobs.
    pub active: usize,
    /// Lanes waiting for a FullEdge slot (approx).
    pub waiters: usize,
}

/// Graph already resident in RAM (hydrate done). **Not** the same as FullEdge progress.
///
/// Agents MUST NOT treat absence from `edge_builds` as “not ready”. After trusted
/// Complete hydrate, FullEdge may never re-run → warehouse is ready but not listed there.
#[derive(Serialize)]
pub struct LoadedGraphHealth {
    pub nodes: usize,
    /// Warehouse edge build stamped complete (O(1) graph flag).
    pub edges_complete: bool,
    /// Safe for Trace/Find (nodes present). Prefer rewalk /context over inventing health polls.
    pub ready: bool,
}

/// MCP health check response for `GET /mcp/health`.
///
/// Returns the server's operational status, version, and unique fingerprint.
/// Used by LLM clients to verify the Butler backend is reachable before making context requests.
#[derive(Serialize)]
pub struct McpHealth {
    pub status: &'static str,
    pub version: &'static str,
    pub fingerprint: String,
    /// **FullEdge progress only.** Empty when no live/known edge job — does **not** mean
    /// “warehouse not loaded”. See `loaded` for RAM residency.
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub edge_builds: std::collections::HashMap<String, EdgeBuildHealthEntry>,
    /// Roots with a non-empty graph in RAM (hydrate/warm). Use this for “is it loaded?”.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub loaded: std::collections::HashMap<String, LoadedGraphHealth>,
    /// Per-root WarehousePolice + global FullEdge slot governor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fulledge_governor: Option<FullEdgeGovernorHealth>,
    /// Always-on ring of recent `/context` calls (see `BUTLER_REQUEST_LOG`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_log: Option<String>,
    /// Newest-last sample (capped) for quick `curl /mcp/health` without opening the file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_requests: Vec<String>,
    /// Machine hint so agents never invent the wrong poll target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_wait_hint: Option<serde_json::Value>,
}

/// Analyzes the user's prompt and decides the intent.
/// Supports bypass mechanisms so users can still search for meta-keywords in their code.
/// Classifies the intent of a `butler_context` prompt for smart routing.
///
/// Three possible intents:
/// - [`PromptIntent::NormalSearch`]: Standard keyword-based code search (after applying any bypass stripping)
/// - [`PromptIntent::MetaQuestion`]: User is asking about how to use Butler itself → server serves instructions
/// - [`PromptIntent::LocationTargeting`]: User references a specific line/module → prefer surgical mode
#[derive(Debug)]
pub enum PromptIntent {
    /// Normal code search (after applying any bypass stripping)
    NormalSearch { cleaned_prompt: String },
    /// User is asking about how to use Butler itself → should redirect
    MetaQuestion,
    /// User wants a specific location (line number or module) → prefer surgical
    LocationTargeting,
}
