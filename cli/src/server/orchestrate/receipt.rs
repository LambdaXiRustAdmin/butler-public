//! Trust receipt, next_action, why-edge (Track T.1 / T.1c / T.3).
use super::disambiguate::is_homonym_risk_name;
use crate::server::dto::*;

// ---------------------------------------------------------------------------
// Track T.3 — Invocation: always leave a concrete next step
// ---------------------------------------------------------------------------

/// Next step when Trace/Find cannot resolve a symbol (tutor-copy: command-shaped).
pub(crate) fn next_action_symbol_miss(
    symbol: &str,
    edges_complete: bool,
    percent: usize,
) -> String {
    if !edges_complete {
        return format!(
            "retry same who_calls/Trace when edges climb (now {}%); do not conclude missing; do not switch to grep",
            percent.min(99)
        );
    }
    if is_homonym_risk_name(symbol) {
        return format!(
            "call who_calls again with scope_paths pinned to a dir/file that owns '{symbol}' (short names collide), or a longer path-qualified name — not grep"
        );
    }
    "call who_calls again with wider/changed scope_paths or mode=arch to orient; verify spelling — do not abandon for grep"
        .into()
}

pub(crate) fn next_action_missing_target_symbol() -> String {
    "call who_calls with symbol/target_symbol set to an exact identifier (function/type name); avoid prose"
        .into()
}

pub(crate) fn next_action_building(percent: usize, soft_wall: bool) -> String {
    if soft_wall {
        "set confirm_long_wait=true and retry same who_calls, or abort and re-open with tighter scope_paths"
            .into()
    } else {
        format!(
            "retry same who_calls (progress {}%); use toc as scope_paths when present — usable partial, not a hang",
            percent.min(99)
        )
    }
}

pub(crate) fn next_action_disambiguate() -> String {
    "call who_calls again with scope_paths set to exactly ONE path from locations/suggested_scopes above, then re-Trace — do not proceed without pinning"
        .into()
}

/// Stamp `next_action` + mirror into telemetry for agents that only read telemetry.
pub(crate) fn set_next_action(st: &mut StructuredReport, action: impl Into<String>) {
    let a = action.into();
    if let Some(obj) = st.telemetry.as_object_mut() {
        obj.insert("next_action".into(), serde_json::Value::String(a.clone()));
    }
    st.next_action = Some(a);
}

/// T.1c why-edge: honest proof only (bridge relation / transitive hop). Silence on bare CALL.
pub(crate) fn why_edge_for(seed: &str, arrow: &str, cc: &CallerCallee) -> Option<String> {
    match cc.relation.as_deref() {
        Some("export") => Some(format!("{seed} {arrow} {} via export bridge", cc.name)),
        Some("ipc") => Some(format!("{seed} {arrow} {} via ipc bridge", cc.name)),
        Some("twin") => Some(format!("{seed} {arrow} {} via twin link", cc.name)),
        Some(r) if !r.is_empty() && !r.eq_ignore_ascii_case("call") && !r.eq_ignore_ascii_case("ffi") => {
            Some(format!("{seed} {arrow} {} via {r}", cc.name))
        }
        Some("ffi") => Some(format!("{seed} {arrow} {} via cross-lang bridge", cc.name)),
        _ if cc.hop >= 2 => Some(format!(
            "{seed} {arrow} {} (transitive hop {})",
            cc.name, cc.hop
        )),
        _ => None,
    }
}

/// Fill why on top-3 of each neighbor list (silence when no honest signal).
pub(crate) fn attach_why_edges(
    seed: &str,
    callers: &mut [CallerCallee],
    callees: &mut [CallerCallee],
    bridge_callers: &mut [CallerCallee],
    bridge_callees: &mut [CallerCallee],
) {
    for cc in callers.iter_mut().take(3) {
        cc.why = why_edge_for(seed, "←", cc);
    }
    for cc in callees.iter_mut().take(3) {
        cc.why = why_edge_for(seed, "→", cc);
    }
    for cc in bridge_callers.iter_mut().take(3) {
        cc.why = why_edge_for(seed, "←", cc);
    }
    for cc in bridge_callees.iter_mut().take(3) {
        cc.why = why_edge_for(seed, "→", cc);
    }
}

// ---------------------------------------------------------------------------
// Track T.1 — Trust receipt (faster to trust than grep is to run)
// ---------------------------------------------------------------------------

/// Build the agent-facing trust receipt from report fields (no side effects).
pub(crate) fn compute_trace_receipt(st: &StructuredReport) -> crate::server::dto::TraceReceipt {
    use crate::server::dto::TraceReceipt;

    // Error / repair paths (scope_not_found, symbol miss, etc.): never paint
    // high|complete from warehouse edge-build ladder — agents treat receipt as trust.
    if st.error.is_some() && st.target.is_none() {
        let basis = if st.blast_domain.as_deref() == Some("scope_not_found") {
            "scope_not_found"
        } else {
            "error"
        };
        return TraceReceipt {
            confidence: "low".into(),
            ladder: "error".into(),
            basis: basis.into(),
            edges: "n/a".into(),
        };
    }

    let ladder = st
        .state
        .confidence
        .as_deref()
        .unwrap_or("inventory")
        .to_string();
    let confidence = match ladder.as_str() {
        "edges_full" => "high",
        "edges_partial" | "index_exact" => "medium",
        _ => "low",
    }
    .to_string();

    let edges_complete = st
        .telemetry
        .get("edges_complete")
        .and_then(|v| v.as_bool())
        .unwrap_or(ladder == "edges_full");
    let pct = st.state.percent.unwrap_or(0).min(99);
    let building = st
        .state
        .edge_build
        .to_ascii_lowercase()
        .contains("building")
        || st.state.edge_build.to_ascii_lowercase().contains("mapping");
    let edges = if edges_complete || ladder == "edges_full" {
        "complete".to_string()
    } else if building && pct < 100 {
        format!("building@{pct}%")
    } else {
        format!("partial@{pct}%")
    };

    let basis = derive_receipt_basis(st).to_string();

    TraceReceipt {
        confidence,
        ladder,
        basis,
        edges,
    }
}

/// Honest neighborhood basis — approximate until per-edge provenance exists.
pub(super) fn derive_receipt_basis(st: &StructuredReport) -> &'static str {
    if st.error.is_some() && st.target.is_none() {
        return "error";
    }
    if st.blast_domain.as_deref() == Some("type_neighborhood") {
        return "type-neighborhood";
    }
    if st.blast_domain.as_deref() == Some("disambiguate")
        || st
            .telemetry
            .get("disambiguate")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        return "disambiguate";
    }
    let has_call = !st.callers.is_empty() || !st.callees.is_empty();
    let has_bridge = !st.bridge_callers.is_empty() || !st.bridge_callees.is_empty();
    if !has_call && has_bridge {
        let rel = st
            .bridge_callers
            .iter()
            .chain(st.bridge_callees.iter())
            .find_map(|c| c.relation.as_deref());
        return match rel {
            Some("ipc") => "bridge-ipc",
            Some("twin") => "bridge-twin",
            Some("export") | Some("ffi") => "bridge-export",
            _ => "bridge-export",
        };
    }
    if !has_call && !has_bridge {
        // Index hit / ★ seed with empty CALL neighborhood
        if st.target.is_some() {
            return "location-only";
        }
        return "error";
    }
    // Same-lang CALL present. Without per-edge tags we cannot claim import-bound/barrel yet.
    "bare-name"
}

/// Fill `receipt` + mirror into telemetry (idempotent).
pub(crate) fn attach_trace_receipt(st: &mut StructuredReport) {
    let r = compute_trace_receipt(st);
    if let Some(obj) = st.telemetry.as_object_mut() {
        if let Ok(v) = serde_json::to_value(&r) {
            obj.insert("receipt".into(), v);
        }
    }
    st.receipt = Some(r);
}

pub(super) fn receipt_compact_bit(st: &StructuredReport) -> String {
    let r = st
        .receipt
        .clone()
        .unwrap_or_else(|| compute_trace_receipt(st));
    format!(
        " receipt: {} | {} | {}",
        r.confidence, r.basis, r.edges
    )
}
