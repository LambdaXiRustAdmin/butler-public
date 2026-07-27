//! Post-edge interconnect passes: map + reduce typed bridges (+ a few CALL maps).
//!
//! Owns the **orchestration** of Export / Ipc / Twin. Discovery stays in `lang/*/ffi`
//! and `ipc_engine`. C decl↔def stays in `linker` (same-lang structural).

use super::detect;
use super::presence::LangPresence;
use super::BridgeKind;
use crate::snooper::ipc_engine::{self, IpcRule};
use crate::snooper::model::{CodeGraph, Id};
use std::path::Path;

/// CALL edges + typed interconnect bridges from a post-edge map phase.
#[derive(Debug, Default)]
pub struct PostEdgeMaps {
    /// Same-lang-ish CALL (e.g. TS relative imports).
    pub call: Vec<(Id, Id)>,
    /// Typed bridges (Export / Ipc / Twin) — not CALL.
    pub bridge: Vec<(Id, Id, BridgeKind)>,
}

impl PostEdgeMaps {
    pub fn total_len(&self) -> usize {
        self.call.len() + self.bridge.len()
    }
}

/// Reduce map-phase results under write lock.
pub fn apply_post_edge_maps(graph: &mut CodeGraph, maps: PostEdgeMaps) {
    if !maps.call.is_empty() {
        graph.add_edges_batch(maps.call);
    }
    if !maps.bridge.is_empty() {
        let n = maps.bridge.len();
        graph.add_bridge_edges_batch(maps.bridge);
        println!("⚡ Interconnect reduce: {n} typed bridge edge(s)");
    }
}

/// Run interconnect post-edge map+reduce (no C decl↔def).
pub fn run_without_decl_def(
    graph: &mut CodeGraph,
    rules: Option<&[IpcRule]>,
    project_root: Option<&Path>,
) {
    let maps = map_without_decl_def(graph, rules, project_root);
    apply_post_edge_maps(graph, maps);
}

/// Read-only map of post-edge CALL + typed bridges. Safe under `RwLock::read`.
///
/// Independent maps (Export / TS imports / Twin / Ipc) run in parallel via rayon
/// so interconnect inject is not single-core serial.
pub fn map_without_decl_def(
    graph: &CodeGraph,
    rules: Option<&[IpcRule]>,
    project_root: Option<&Path>,
) -> PostEdgeMaps {
    let mut default_rules = Vec::new();
    let rules = rules.unwrap_or_else(|| {
        default_rules = ipc_engine::default_ipc_rules();
        &default_rules[..]
    });

    // Peer schedule (P.3) — log only; does not invent edges.
    let schedule = detect::detect_peer_schedule(graph, project_root);
    schedule.log_if_needed();

    let presence = LangPresence::scan(graph);
    let want_export = presence.wants_ffi_export_map();
    let want_ts = presence.ts_js;
    let want_twin = polyglot_ac_enabled();
    let want_ipc = presence.wants_ipc_map();
    if !want_twin {
        // Once per process — Twin is product-opt-in; missing cross-lang “twins” are expected.
        static TWIN_OFF_LOG: std::sync::Once = std::sync::Once::new();
        TWIN_OFF_LOG.call_once(|| {
            println!(
                "📡 Twin bridges default OFF (weak name-coincidence); \
                 set BUTLER_POLYGLOT_AC=1 to enable — never default gold (P.10)"
            );
        });
    }

    // Four independent read-only maps in parallel (nested join = 4-way fan-out, not serial).
    // Empty branch when gate is false — cheap; keeps one shape for all hosts.
    let ((export_edges, ts_imports), (twin_edges, ipc_edges)) = rayon::join(
        || {
            rayon::join(
                || {
                    if want_export {
                        crate::snooper::linker::map_ffi_export_edges_gated(
                            graph,
                            project_root,
                            &presence,
                        )
                    } else {
                        Vec::new()
                    }
                },
                || {
                    if want_ts {
                        crate::snooper::lang::typescript::link_relative_imports(
                            graph,
                            project_root,
                        )
                    } else {
                        Vec::new()
                    }
                },
            )
        },
        || {
            rayon::join(
                || {
                    if want_twin {
                        crate::snooper::linker::map_polyglot_edges(graph, project_root)
                    } else {
                        Vec::new()
                    }
                },
                || {
                    if want_ipc {
                        ipc_engine::map_ipc_edges_with_root(graph, rules, project_root)
                    } else {
                        Vec::new()
                    }
                },
            )
        },
    );

    let mut maps = PostEdgeMaps::default();
    for (from, to) in export_edges {
        maps.bridge.push((from, to, BridgeKind::Export));
    }
    if !ts_imports.is_empty() {
        println!(
            "⚡ TS/JS relative import edges: {} (CALL map)",
            ts_imports.len()
        );
        maps.call.extend(ts_imports);
    }
    for (from, to) in twin_edges {
        maps.bridge.push((from, to, BridgeKind::Twin));
    }
    for (from, to) in ipc_edges {
        maps.bridge.push((from, to, BridgeKind::Ipc));
    }

    maps
}

/// Weak cross-lang **Twin** bridges (name-coincidence / AC) — **default OFF**.
///
/// Set `BUTLER_POLYGLOT_AC=1` (or `true`/`yes`/`on`) to enable. Product policy: never
/// default gold labels on Twin; Export/Ipc are the honest dual-stack floor.
/// See `plans/interconnect-steward.md` P.10.
pub fn polyglot_ac_enabled() -> bool {
    match std::env::var("BUTLER_POLYGLOT_AC") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        }
        Err(_) => false,
    }
}
