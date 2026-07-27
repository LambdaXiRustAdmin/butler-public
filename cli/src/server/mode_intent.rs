//! Single source of truth for goal/mode string → product intent.
//!
//! Pack A: all `goal`/`mode` parsing for orchestrate routing and `ContextMode` should go through
//! [`resolve_mode_intent`] / [`intent_from_request`] — not ad-hoc `.contains("architect")` copies.

use crate::server::dto::ContextRequest;
use code_graph::snooper::context::ContextMode;

/// Canonical intent derived from `goal` / `mode` strings (before MCP tool-name overrides).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeIntent {
    TraceBlastRadius,
    FindImplementation,
    ArchitecturalSummary,
    Surgical,
    Implementation,
    Architecture,
    Compressed,
    Mini,
    Balanced,
    /// Empty or unrecognized raw string (composer falls back to Balanced).
    Unknown,
}

impl ModeIntent {
    /// Trace / Find / Arch product goals that take the orchestrate path.
    pub fn wants_orchestrate(self) -> bool {
        matches!(
            self,
            Self::TraceBlastRadius
                | Self::FindImplementation
                | Self::ArchitecturalSummary
                | Self::Architecture
        )
    }

    /// ArchitecturalSummary spine (hubs/skeleton TOC) — any architecture-shaped goal.
    pub fn is_architectural_summary(self) -> bool {
        matches!(
            self,
            Self::ArchitecturalSummary | Self::Architecture
        )
    }

    /// Map to composer [`ContextMode`].
    ///
    /// Trace/Find → Surgical (structural neighborhood). Arch* → Architecture.
    /// **Unknown → Balanced** (legacy silent fallback; prefer spelling goals correctly).
    pub fn to_context_mode(self) -> ContextMode {
        match self {
            Self::TraceBlastRadius | Self::FindImplementation | Self::Surgical => {
                ContextMode::Surgical
            }
            Self::ArchitecturalSummary | Self::Architecture => ContextMode::Architecture,
            Self::Implementation => ContextMode::Implementation,
            Self::Compressed => ContextMode::Compressed,
            Self::Mini => ContextMode::Mini,
            Self::Balanced | Self::Unknown => ContextMode::Balanced,
        }
    }
}

/// Normalize for table lookup: lowercase, strip `_` / space / `-`.
pub fn normalize_intent_key(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| *c != '_' && *c != ' ' && *c != '-')
        .collect()
}

/// Exact synonym table (after [`normalize_intent_key`]).
///
/// Keep in sync with butler_ask short forms + orchestrate goal names + ContextMode labels.
const EXACT_SYNONYMS: &[(&str, ModeIntent)] = &[
    // Orchestrate goals
    ("traceblastradius", ModeIntent::TraceBlastRadius),
    ("trace", ModeIntent::TraceBlastRadius),
    ("findimplementation", ModeIntent::FindImplementation),
    ("findimpl", ModeIntent::FindImplementation),
    ("find", ModeIntent::FindImplementation),
    ("architecturalsummary", ModeIntent::ArchitecturalSummary),
    ("architecture", ModeIntent::Architecture),
    ("architectural", ModeIntent::ArchitecturalSummary),
    ("arch", ModeIntent::ArchitecturalSummary),
    ("map", ModeIntent::ArchitecturalSummary),
    // Composer modes
    ("surgical", ModeIntent::Surgical),
    ("implementation", ModeIntent::Implementation),
    ("compressed", ModeIntent::Compressed),
    ("mini", ModeIntent::Mini),
    ("balanced", ModeIntent::Balanced),
    ("auto", ModeIntent::Unknown), // façade short form before route_ask_goal
];

/// Resolve a raw goal/mode string to [`ModeIntent`].
///
/// Order: exact synonym table → substring heuristics (legacy `.contains` behavior) → Unknown.
///
/// Substring rules (preserve historical routing):
/// - contains `architect` → ArchitecturalSummary (so `"architect"` is not silent Balanced)
/// - contains `findimpl` / `findimplementation` → FindImplementation
/// - exact-ish `find` already in table; bare contains `trace` → TraceBlastRadius
pub fn resolve_mode_intent(raw: &str) -> ModeIntent {
    let g = normalize_intent_key(raw);
    if g.is_empty() {
        return ModeIntent::Unknown;
    }
    for (key, intent) in EXACT_SYNONYMS {
        if g == *key {
            return *intent;
        }
    }
    // Partial / misspelling-tolerant (was scattered `.contains` checks)
    if g.contains("architect") {
        return ModeIntent::ArchitecturalSummary;
    }
    if g.contains("findimplementation") || g.contains("findimpl") {
        return ModeIntent::FindImplementation;
    }
    if g.contains("trace") {
        return ModeIntent::TraceBlastRadius;
    }
    // symbol_trace_partial_ok used contains("find") — keep for partial-serve only via helper
    if g.contains("find") {
        return ModeIntent::FindImplementation;
    }
    ModeIntent::Unknown
}

/// Prefer `goal`, then `mode` (same as historical `.or(req.mode)`).
pub fn intent_from_request(req: &ContextRequest) -> ModeIntent {
    let raw = req
        .goal
        .as_deref()
        .or(req.mode.as_deref())
        .unwrap_or("");
    resolve_mode_intent(raw)
}

/// Effective composer mode with **MCP tool overrides** (precedence is intentional).
///
/// 1. `butler_map` → Architecture  
/// 2. `butler_inspect` → Implementation | Surgical  
/// 3. `force_surgical` (file:line / surgical flags)  
/// 4. goal/mode via [`intent_from_request`]
pub fn compute_effective_mode(req: &ContextRequest, force_surgical: bool) -> ContextMode {
    // butler_map: structural map always returns file-level architecture overview
    if req.mcp_tool_name.as_deref() == Some("butler_map") {
        return ContextMode::Architecture;
    }
    // butler_inspect: defaults to surgical; implementation variant for body inspection
    if req.mcp_tool_name.as_deref() == Some("butler_inspect") {
        let m = req
            .mode
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        return if m == "implementation" {
            ContextMode::Implementation
        } else {
            ContextMode::Surgical
        };
    }
    if force_surgical {
        return ContextMode::Surgical;
    }
    intent_from_request(req).to_context_mode()
}

/// True when goal/mode should take Trace/Find/Arch orchestrate (not plain select_blocks).
pub fn wants_orchestrate_path(req: &ContextRequest) -> bool {
    if matches!(
        req.mcp_tool_name.as_deref(),
        Some("butler_orchestrate") | Some("butler_ask")
    ) {
        return true;
    }
    intent_from_request(req).wants_orchestrate()
}

/// ArchitecturalSummary (or architect-shaped) orchestrate goal.
pub fn is_architectural_summary_orchestrate(req: &ContextRequest) -> bool {
    wants_orchestrate_path(req) && intent_from_request(req).is_architectural_summary()
}

/// Short façade modes accepted by `normalize_butler_ask_request` before `route_ask_goal`.
///
/// Full words like `"architecture"` currently bypass façade remap (historical); use table
/// when expanding the gate.
pub fn is_butler_ask_facade_mode(mode: &str) -> bool {
    matches!(
        mode.to_ascii_lowercase().as_str(),
        "auto" | "trace" | "find" | "arch" | "map" | ""
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::dto::ContextRequest;

    fn req_goal(goal: &str) -> ContextRequest {
        ContextRequest {
            prompt: String::new(),
            root: ".".into(),
            project: None,
            depth: 2,
            max_tokens: 4000,
            compress_tests: true,
            full_module: false,
            target_file: None,
            target_line: None,
            mode: None,
            goal: Some(goal.into()),
            target_symbol: None,
            scope_paths: None,
            ignore_paths: None,
            focus_symbol: None,
            focus_symbols: None,
            expand_hops: None,
            sample_offset: None,
            exclude_symbols: None,
            sample_mode: None,
            detail: None,
            query: None,
            confirm_long_wait: None,
            max_results: 8,
            mcp_tool_name: None,
        }
    }

    #[test]
    fn architect_typo_is_not_silent_balanced() {
        // Historical bug: effective_mode exact-match only → Balanced while wants_orchestrate true
        let i = resolve_mode_intent("architect");
        assert!(i.is_architectural_summary());
        assert_eq!(i.to_context_mode(), ContextMode::Architecture);
    }

    #[test]
    fn orchestrate_goal_synonyms() {
        assert_eq!(
            resolve_mode_intent("TraceBlastRadius"),
            ModeIntent::TraceBlastRadius
        );
        assert_eq!(
            resolve_mode_intent("trace_blast_radius"),
            ModeIntent::TraceBlastRadius
        );
        assert_eq!(
            resolve_mode_intent("FindImplementation"),
            ModeIntent::FindImplementation
        );
        assert_eq!(
            resolve_mode_intent("find_implementation"),
            ModeIntent::FindImplementation
        );
        assert_eq!(resolve_mode_intent("find"), ModeIntent::FindImplementation);
        assert_eq!(
            resolve_mode_intent("ArchitecturalSummary"),
            ModeIntent::ArchitecturalSummary
        );
        assert_eq!(
            resolve_mode_intent("architecture"),
            ModeIntent::Architecture
        );
    }

    #[test]
    fn wants_orchestrate_from_goal() {
        assert!(wants_orchestrate_path(&req_goal("TraceBlastRadius")));
        assert!(wants_orchestrate_path(&req_goal("architect")));
        assert!(!wants_orchestrate_path(&req_goal("balanced")));
        assert!(!wants_orchestrate_path(&req_goal("")));
    }

    #[test]
    fn mcp_map_overrides_goal() {
        let mut r = req_goal("TraceBlastRadius");
        r.mcp_tool_name = Some("butler_map".into());
        assert_eq!(
            compute_effective_mode(&r, false),
            ContextMode::Architecture
        );
    }

    #[test]
    fn force_surgical_beats_goal() {
        let r = req_goal("ArchitecturalSummary");
        assert_eq!(
            compute_effective_mode(&r, true),
            ContextMode::Surgical
        );
    }

    #[test]
    fn facade_short_forms() {
        assert!(is_butler_ask_facade_mode("auto"));
        assert!(is_butler_ask_facade_mode("TRACE"));
        assert!(!is_butler_ask_facade_mode("architecture"));
    }

    #[test]
    fn architect_normalizes_to_arch_summary_for_orchestrate() {
        // handle_orchestrate match arms need ArchitecturalSummary, not raw "architect"
        assert!(matches!(
            resolve_mode_intent("architect"),
            ModeIntent::ArchitecturalSummary | ModeIntent::Architecture
        ));
        assert!(resolve_mode_intent("architect").is_architectural_summary());
    }
}
