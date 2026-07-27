//! Surgical JIT + Phase-4 elaboration (P3.1 stage peel S5).
//!
//! Symbol JIT, Phase-4 file lists, interconnect session, scoped parse.
//! Ends before final serve read (compose_path owns graph_guard).
//! Zero intentional behavior change.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::{http::StatusCode, Json};
use code_graph::scoped_block_refs;

use crate::server::build_status;
use crate::server::dto::*;
use crate::server::mode_intent::is_architectural_summary_orchestrate;
use crate::server::state::*;
use crate::vprintln;

use super::building::building_graph_response_with_policy;
use super::graph_admit::ensure_background_edge_build;
use super::serve_prep::ServePrepReady;
use super::surgical::{ensure_interconnect_session, run_surgical_jit_nonblocking};

pub(super) enum SurgicalPhaseOutcome {
    Early(Result<(StatusCode, Json<ContextResponse>), String>),
    /// JIT / Phase-4 done; caller must take serve read + compose.
    Continue,
}

/// Symbol JIT + Phase-4 elaboration. Does not hold a final graph read guard.
pub(super) fn run_surgical_phase(
    state: &AppState,
    req: &ContextRequest,
    root: &str,
    graph_rw: &Arc<std::sync::RwLock<code_graph::CodeGraph>>,
    ipc_rules: &[code_graph::snooper::ipc_engine::IpcRule],
    force_surgical: bool,
    prep: &ServePrepReady,
    graph_time_ms: u64,
    is_cached: bool,
    overall_start: Instant,
) -> SurgicalPhaseOutcome {
    let ServePrepReady {
        effective_mode,
        is_orchestrate,
        symbol_surgical_trace,
        symbol_trace_partial_ok,
        ..
    } = prep;

    // JIT for surgical *or* any explicit target_symbol (Balanced + Trace goal included).
    // Serve gate: if edges already exist for the exact-symbol files, skip rebuild.
    let want_symbol_jit = force_surgical
        || matches!(*effective_mode, code_graph::ContextMode::Surgical)
        || req
            .target_symbol
            .as_deref()
            .map(str::trim)
            .is_some_and(|s| !s.is_empty());
    if want_symbol_jit {
        if let Some(sym) = &req.target_symbol {
            if !sym.is_empty() {
                let relevant: Vec<PathBuf> = {
                    // Brief block on write-lock release — never hollow *cold* Building.
                    // Skeleton already installed + FullEdge merge: skip JIT list, continue Trace.
                    match build_status::read_graph_for_serve(state, root, graph_rw) {
                        None => {
                            if build_status::skeleton_present_for_serve(state, root, graph_rw) {
                                vec![]
                            } else {
                                return SurgicalPhaseOutcome::Early(
                                    building_graph_response_with_policy(
                                        state,
                                        root,
                                        build_status::building_graph_message(
                                            build_status::percent_for_status(state, root, None),
                                        ),
                                        graph_time_ms,
                                        is_cached,
                                        overall_start,
                                        req.confirm_long_wait.unwrap_or(false),
                                    ),
                                );
                            }
                        }
                        Some(gg) => {
                            // Index enforcer: O(hits) via name_index — never scan 1M nodes.
                            let files = gg.files_for_name(sym);
                            // Serve gate only when warehouse has real edges AND this symbol's
                            // files were edged. Empty warehouse (0 CALL edges) never skips JIT —
                            // false Complete must not return zero callers.
                            let warehouse_has_edges = gg.total_edges() > 0;
                            if !files.is_empty()
                                && warehouse_has_edges
                                && files.iter().all(|f| gg.file_has_edges(f))
                            {
                                vec![]
                            } else {
                                files
                                    .into_iter()
                                    .filter(|f| !warehouse_has_edges || !gg.file_has_edges(f))
                                    .collect()
                            }
                        }
                    }
                };
                if !relevant.is_empty() {
                    // Do **not** park FullEdge on the police lane for JIT.
                    // Brief try_write / short police wait only — grind keeps collecting.
                    let full_edge_live = build_status::is_live_build(
                        state,
                        root,
                        build_status::try_read_graph(graph_rw).as_deref(),
                    );
                    let ok = run_surgical_jit_nonblocking(
                        graph_rw,
                        root,
                        &state.settings.analysis.skip_directories,
                        &relevant,
                        full_edge_live,
                    );
                    vprintln!(
                        "🔗 JIT surgical edges (non-parking) for {} file(s) relevant to '{}' (ok={} live_edge={})",
                        relevant.len(),
                        sym,
                        ok,
                        full_edge_live
                    );
                } else if *symbol_surgical_trace {
                    vprintln!(
                        "⚡ Serve gate: edges ready for '{}' — skip surgical JIT",
                        sym
                    );
                }
            }
        }
    }

    // Phase 4 (write then read guard). Protected by QUERY_SEMAPHORE + spawn_blocking.
    // Full edge grind may still run in background — serve path no longer waits for Complete.
    ensure_background_edge_build(state, root, graph_rw);

    // Symbol Trace: never return Building solely because bg holds the lock mid-batch.
    if !symbol_trace_partial_ok {
        if let Some(msg) = build_status::try_lock_contention_building(state, root, graph_rw) {
            return SurgicalPhaseOutcome::Early(building_graph_response_with_policy(
                state,
                root,
                msg,
                graph_time_ms,
                is_cached,
                overall_start,
                req.confirm_long_wait.unwrap_or(false),
            ));
        }
    }

    // Prefer try_write; symbol Trace falls back to blocking write for short JIT/elab.
    // Phase 4: collect JIT file lists under read (or brief write for parse/IPC only).
    // Heavy ensure_call_graph always goes through WarehousePolice.
    let mut phase4_jit: Vec<PathBuf> = Vec::new();
    let mut elaborated = false;
    {
        let g_read = build_status::read_graph_for_serve(state, root, graph_rw);
        if let Some(g) = g_read.as_deref() {
            if force_surgical || matches!(*effective_mode, code_graph::ContextMode::Surgical) {
                if !g.is_edge_build_complete() {
                    let mut relevant: HashSet<PathBuf> = HashSet::new();
                    if let Some(sym) = &req.target_symbol {
                        if !sym.is_empty() {
                            for f in g.files_for_name(sym) {
                                if !g.file_has_edges(&f) {
                                    relevant.insert(f);
                                }
                            }
                        }
                    }
                    if *is_orchestrate && !symbol_surgical_trace {
                        for b in scoped_block_refs(g, &req.scope_paths, &req.ignore_paths) {
                            if !g.file_has_edges(&b.file) {
                                relevant.insert(b.file.clone());
                            }
                        }
                    }
                    phase4_jit.extend(relevant);
                }
            }
        } else if !symbol_trace_partial_ok {
            if let Some(msg) = build_status::try_lock_contention_building(state, root, graph_rw) {
                return SurgicalPhaseOutcome::Early(building_graph_response_with_policy(
                    state,
                    root,
                    msg,
                    graph_time_ms,
                    is_cached,
                    overall_start,
                    req.confirm_long_wait.unwrap_or(false),
                ));
            }
        }
    }

    if !phase4_jit.is_empty() {
        phase4_jit.sort();
        phase4_jit.dedup();
        let full_edge_live = build_status::is_live_build(
            state,
            root,
            build_status::try_read_graph(graph_rw).as_deref(),
        );
        let _ = run_surgical_jit_nonblocking(
            graph_rw,
            root,
            &state.settings.analysis.skip_directories,
            &phase4_jit,
            full_edge_live,
        );
        elaborated = true;
    }

    // Light write work only (scoped parse, IPC, cargo metadata) — **try_write only**.
    // Never block on `write()` for Trace/orchestrate: post-FullEdge `save_graph_async`
    // holds a multi‑GiB read (slim clone); a blocking writer queues behind it and on
    // writer-preferring RwLocks starves every Trace lobby (`try_read_busy` for minutes).
    // Ingested files needing edges are JIT'd via police (not under this write).
    //
    // Execution planner: ArchitecturalSummary on a warm Complete / sources-stripped
    // warehouse is **read-only TOC** — never rebuild IPC or re-parse on agent queries.
    let mut ingested_for_jit: Vec<PathBuf> = Vec::new();
    {
        let is_arch = matches!(*effective_mode, code_graph::ContextMode::Architecture)
            || is_architectural_summary_orchestrate(req);
        let write_guard = graph_rw.try_write().ok();
        if let Some(mut g) = write_guard {
            let warm_complete = g.background_edge_build_complete || g.sources_stripped();
            if is_arch && warm_complete {
                vprintln!(
                    "⚡ Arch warm warehouse: skip Phase-4 parse/IPC/cargo (read-only) root={}",
                    root
                );
            } else if *is_orchestrate && *symbol_surgical_trace {
                // Surgical Trace is **read-only**. Interconnect lives in FullEdge PostPass
                // / load finalize — never re-pay multi-second IPC inject on first butler_ask.
                if warm_complete && !g.interconnect_session_ready {
                    // Complete warehouse: PostPass already ran (or empty bridges are final).
                    g.interconnect_session_ready = true;
                }
            } else if *is_orchestrate && !symbol_surgical_trace {
                ingested_for_jit = g.ensure_scoped_files_parsed(
                    Path::new(root),
                    &req.scope_paths,
                    &req.ignore_paths,
                    &state.settings.analysis.skip_directories,
                );
                // Only while warehouse is still building — never block warm Complete.
                if !warm_complete {
                    ensure_interconnect_session(&mut g, ipc_rules, Path::new(root));
                } else if !g.interconnect_session_ready {
                    g.interconnect_session_ready = true;
                }
                if is_arch && !warm_complete {
                    g.ensure_dependency_versions(Path::new(root));
                    elaborated = true;
                }
            } else if is_arch {
                g.ensure_dependency_versions(Path::new(root));
                elaborated = true;
            }
        } else if *is_orchestrate {
            vprintln!(
                "⚡ Phase 4 write skipped (graph busy — save/merge); serve structural-only under {}",
                root
            );
        }
    }
    if !ingested_for_jit.is_empty() {
        let full_edge_live = build_status::is_live_build(
            state,
            root,
            build_status::try_read_graph(graph_rw).as_deref(),
        );
        let _ = run_surgical_jit_nonblocking(
            graph_rw,
            root,
            &state.settings.analysis.skip_directories,
            &ingested_for_jit,
            full_edge_live,
        );
        elaborated = true;
    }
    if elaborated {
        vprintln!(
            "🔍 Phase 4: Query-driven elaboration triggered for mode {:?}",
            effective_mode
        );
    }

    SurgicalPhaseOutcome::Continue
}
