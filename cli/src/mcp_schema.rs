//! JSON schemas for all Butler MCP tools (the single source of truth for manifests).

use serde_json::Value;

/// Returns the butler_context tool JSON schema.
/// This is the single source of truth used by both the stdio MCP bridge
/// and the HTTP server's /mcp/manifest endpoint.
pub fn butler_context_tool_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "description": "OPTIONAL free text. NOT natural language. Server keeps only strong Ident|snake|camel|Path|foo::bar ∈ CodeGraph; prose→miss. Prefer target_symbol. e.g. load_graph | src/gnn/forward.rs"
            },
            "root": {
                "type": "string",
                "description": "Path to the project root directory to analyze. Can be a relative subdirectory (e.g. 'backend/') or an **absolute path** to any external Rust/Python repository on disk (e.g. '/home/user/my-crate' or '/projects/test_repos/fd' in Docker). When using an absolute path, you may still put any identifier in 'project' or omit heavy reliance on the registry. Defaults to current directory '.'.",
                "default": "."
            },
            "project": {
                "type": "string",
                "description": "The absolute path to the ROOT DIRECTORY of the project. CRITICAL: This must be a directory, NEVER a specific file path."
            },
            "depth": {
                "type": "integer",
                "description": "How many levels of callers and callees to follow in the call graph. 1 = direct connections only. 2 or 3 is usually best. Default: 2",
                "default": 2
            },
            "max_tokens": {
                "type": "integer",
                "description": "Maximum number of tokens the response is allowed to use. Lower this if you need less context. Default: 4000",
                "default": 4000
            },
            "compress_tests": {
                "type": "boolean",
                "description": "When true, test functions are heavily summarized to save tokens. Almost always leave this true. Default: true",
                "default": true
            },
            "mode": {
                "type": "string",
                "enum": ["balanced", "architecture", "implementation", "surgical", "compressed", "mini"],
                "description": "Context mode to return. Use 'mini' for a compact signature + direct edges (best for initial exploration). Use 'surgical' or 'balanced' for full implementations."
            },
            "target_file": {
                "type": "string",
                "description": "(Optional) Used for Workflow A. The specific file path (e.g., 'code_graph/src/snooper/composer.rs'). Omit if using Workflow B."
            },
            "target_line": {
                "type": "integer",
                "minimum": 1,
                "maximum": 1000000,
                "description": "(Optional) Used for Workflow A. The exact line number to inspect. CRITICAL: Do not guess line numbers. If you are unsure of the line, omit this parameter entirely and use Workflow B instead."
            }
        },
        "required": []
    })
}

/// Schema for the `butler_search` MCP tool.
pub fn butler_search_tool_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "project": {
                "type": "string",
                "description": "The project root (absolute path or registry name). Required."
            },
            "query": {
                "type": "string",
                "description": "Structural needle: Ident|Path ∈ CodeGraph (e.g. parse_file, src/snooper/). Prose ignored; empty hits→miss."
            },
            "max_results": {
                "type": "integer",
                "description": "How many top results to return. Default 8.",
                "default": 8
            },
            "scope_paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional Working Set: only search inside these path prefixes (e.g. [\"src/snooper/\"])."
            },
            "ignore_paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional paths to exclude from the Working Set."
            }
        },
        "required": ["project"]
    })
}

/// Schema for the `butler_inspect` MCP tool.
pub fn butler_inspect_tool_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "project": { "type": "string", "description": "Project root. Required." },
            "target_file": { "type": "string", "description": "File to inspect (for surgical/implementation)." },
            "target_line": { "type": "integer", "description": "Optional exact line inside the file." },
            "mode": {
                "type": "string",
                "enum": ["surgical", "implementation"],
                "description": "Only surgical or implementation allowed for butler_inspect."
            },
            "max_tokens": { "type": "integer", "default": 2000 },
            "scope_paths": { "type": "array", "items": {"type":"string"}, "description": "Working Set limit." },
            "ignore_paths": { "type": "array", "items": {"type":"string"} }
        },
        "required": ["project"]
    })
}

/// Schema for the `butler_map` MCP tool — architecture overview of a scope.
pub fn butler_map_tool_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "project": { "type": "string", "description": "Project root. Required." },
            "scope_paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "REQUIRED for map: the directories/modules to map (e.g. [\"cli/src/\", \"code_graph/src/snooper/\"])."
            },
            "max_tokens": { "type": "integer", "default": 2000, "description": "Token budget (capped at 4000)." },
            "ignore_paths": { "type": "array", "items": {"type":"string"} }
        },
        "required": ["project", "scope_paths"]
    })
}

/// Primary agent façade: one tool, auto-routes Trace / Find / Arch.
///
/// Prefer this over `butler_orchestrate` / search / map / inspect for day-to-day use.
/// Response: `content` (human) + `structuredContent` (machine). Cold open may return
/// `status: BUILDING` with TOC — usable partial, not a hang. After ~15 min soft wall:
/// `BUILDING_SOFT_WALL` unless `confirm_long_wait=true`.
pub fn butler_ask_tool_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "project": {
                "type": "string",
                "description": "Project root (absolute path or registry name). Required. Directory preferred; a file path self-heals upward to the nearest project root."
            },
            "query": {
                "type": "string",
                "description": "Optional free-form structural hint: symbol name, path (src/foo.rs), or short Ident/Path tokens. Prefer `symbol` for Trace. Not natural-language chat."
            },
            "symbol": {
                "type": "string",
                "description": "Alias for target_symbol. Symbol to Trace/Find (e.g. mozilla::Mutex, createClient, App)."
            },
            "target_symbol": {
                "type": "string",
                "description": "Symbol to Trace/Find. Same as symbol. Preferred when you know the Ident."
            },
            "mode": {
                "type": "string",
                "enum": ["auto", "trace", "find", "arch", "map"],
                "description": "Routing hint. auto (default): symbol/query Ident → Trace; scope-only or overview words → Arch; mode find → FindImplementation. map/arch → ArchitecturalSummary."
            },
            "scope_paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Working set path prefixes (e.g. [\"src/\", \"xpcom/\"]). Strongly recommended on large repos and mega-homonyms."
            },
            "ignore_paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Paths to exclude from the working set."
            },
            "detail": {
                "type": "string",
                "enum": ["short", "long", "compact", "dense"],
                "description": "Length mode (agent chooses). short|compact (default) = trust dossier + tight neighbor sample — orient/pin/bridges. long|dense = full text dump + larger neighbor sample under a pin. Honesty same both ways (degrees/omitted/hub notes). structuredContent always has the machine report. Prefer short first; re-ask detail=long same scope if sample thin."
            },
            "focus_symbol": {
                "type": "string",
                "description": "Hop continuity (Soft I4): previous seed when chaining A→B→Trace(B). If that name is a real CALL parent of ★, Butler force-includes it in the callers sample (does not dump the full hub reverse). Aliases: origin_symbol, focus."
            },
            "focus_symbols": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional multi-focus parents (same inject rule as focus_symbol)."
            },
            "expand_hops": {
                "type": "integer",
                "minimum": 1,
                "maximum": 2,
                "description": "Explicit multi-hop Trace depth (1–2 only; hard-capped). When set, overrides depth. Values >2 clamp to 2 (not full BFS)."
            },
            "sample_offset": {
                "type": "integer",
                "minimum": 0,
                "maximum": 500,
                "description": "Soft I4 sample window: skip N ranked candidates (per side) before packing. Use when the first sample is wrong — not a full reverse dump. Banner reports offset + omitted."
            },
            "exclude_symbols": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Drop these neighbor names from the sample window (e.g. prior sample). Exact or ::name suffix. Cap 64. Alias: exclude_callers."
            },
            "sample_mode": {
                "type": "string",
                "enum": ["score", "diverse"],
                "description": "Sample ranking: score (default) or diverse (stronger parent-dir diversity for a different window)."
            },
            "depth": {
                "type": "integer",
                "minimum": 1,
                "maximum": 2,
                "description": "CALL neighborhood depth for Trace. Hard-capped at 2. Prefer expand_hops for explicit multi-hop. Default follows server depth (also capped at 2).",
                "default": 2
            },
            "target_file": {
                "type": "string",
                "description": "Optional file for surgical inspect (with target_line)."
            },
            "target_line": {
                "type": "integer",
                "description": "Optional line for surgical inspect (with target_file)."
            },
            "confirm_long_wait": {
                "type": "boolean",
                "description": "Continue past the cold-open soft wall (default 15 min / BUTLER_SOFT_WALL_SECS). Only set true after operator consent (\"are you sure?\"). Without it, long builds return status BUILDING_SOFT_WALL + wait_policy instead of silent forever-retry."
            }
        },
        "required": ["project"]
    })
}

/// Schema for the new `butler_orchestrate` high-level tool (recommended for local models).
/// Response: human summary in `content`; machine-readable report in `structured` (HTTP) or `structuredContent` (MCP).
pub fn butler_orchestrate_tool_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "project": {
                "type": "string",
                "description": "The project root (absolute path or registry name). Required. For butler_orchestrate, a file path is accepted and will self-heal upward (max 3 levels) to the nearest project root (Cargo.toml/pyproject.toml); the file's path is automatically injected into scope_paths to preserve the user's intent."
            },
            "goal": {
                "type": "string",
                "enum": ["TraceBlastRadius", "ArchitecturalSummary", "FindImplementation"],
                "description": "The high-level goal. 'TraceBlastRadius' for impact analysis of a symbol. 'ArchitecturalSummary' for scoped overview. 'FindImplementation' for deep dive into a symbol. Case-insensitive matching supported."
            },
            "target_symbol": {
                "type": "string",
                "description": "The symbol or concept to focus on. Required for TraceBlastRadius and FindImplementation. Sufficient on its own — no prompt field needed."
            },
            "scope_paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Strongly recommended Working Set: restrict analysis to these path prefixes (e.g. [\"src/\"]). Improves relevance, speed, and token use."
            },
            "ignore_paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional paths to exclude from the Working Set."
            },
            "detail": {
                "type": "string",
                "enum": ["short", "long", "compact", "dense"],
                "description": "Length mode. short|compact (default) = dossier + tight sample. long|dense|full|verbose = full dump + larger sample. Prefer short→long under same scope_paths. structuredContent always full machine report."
            },
            "focus_symbol": {
                "type": "string",
                "description": "Hop continuity: previous seed when chaining A→B→Trace(B). Injected into callers sample if it is a real CALL parent of ★. Aliases: origin_symbol, focus."
            },
            "focus_symbols": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional multi-focus parents (same rule as focus_symbol)."
            },
            "expand_hops": {
                "type": "integer",
                "minimum": 1,
                "maximum": 2,
                "description": "Explicit multi-hop depth (1–2; hard-capped). Overrides depth when set."
            },
            "sample_offset": {
                "type": "integer",
                "minimum": 0,
                "maximum": 500,
                "description": "Skip N ranked candidates before packing (different sample window)."
            },
            "exclude_symbols": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Drop names from sample window (prior sample). Alias: exclude_callers."
            },
            "sample_mode": {
                "type": "string",
                "enum": ["score", "diverse"],
                "description": "score (default) or diverse parent-dir ranking."
            },
            "depth": {
                "type": "integer",
                "minimum": 1,
                "maximum": 2,
                "description": "CALL neighborhood depth. Hard-capped at 2 (not full BFS).",
                "default": 2
            },
            "confirm_long_wait": {
                "type": "boolean",
                "description": "Continue past the cold-open soft wall (default 15 min). Only set true after operator consent. See wait_policy on BUILDING / BUILDING_SOFT_WALL responses."
            }
        },
        "required": ["project", "goal"]
    })
}
