//! Lock-free background edge-build status for HTTP fast-fail (Sprint 6/7).
//!
//! Progress and state live outside the main [`CodeGraph`] `RwLock` so status checks
//! never block behind the background writer. Sprint 7: post-complete paths always
//! serve real content when telemetry or graph says ready.
//!
//! First-use: progress messages include a meter + hang-on copy so agents/humans
//! know cold open is working (not dead).

use std::sync::Arc;

use code_graph::snooper::{BackgroundEdgeBuildState, BgBuildProgress, CodeGraph};

use super::state::{AppState, BuildProgress};

/// ASCII progress bar for cold-start feedback.
pub fn progress_bar(percent: usize, width: usize) -> String {
    let p = percent.min(100);
    let filled = (p * width) / 100;
    let empty = width.saturating_sub(filled);
    format!(
        "[{}{}] {:>3}%",
        "█".repeat(filled),
        "░".repeat(empty),
        p
    )
}

/// Cap on “wait until hydrate is done **or** this many ms, whichever first.”
///
/// Hot Trace is tens–hundreds of ms; many hydrates finish in that window.
/// Saying “hydrating” immediately forces MCP onto `retry_after_ms` (~1.5s)
/// for work that already finished. Env `BUTLER_HYDRATE_GRACE_MS`, default **500**.
/// `0` disables the wait (tests / never-block).
pub fn hydrate_answer_grace_ms() -> u64 {
    std::env::var("BUTLER_HYDRATE_GRACE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(|n: u64| n.min(5_000))
        .unwrap_or(500)
}

/// Soft wall for cold open / hydrate (seconds). Env `BUTLER_SOFT_WALL_SECS`, default **900** (15 min).
pub fn soft_wall_secs() -> u64 {
    std::env::var("BUTLER_SOFT_WALL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n >= 60)
        .unwrap_or(900)
}

/// Adaptive poll interval for agents (ms) — backs off as wait lengthens.
pub fn adaptive_retry_after_ms(elapsed_s: u64) -> u64 {
    match elapsed_s {
        0..=15 => 1_500,
        16..=45 => 3_000,
        46..=120 => 5_000,
        121..=300 => 10_000,
        301..=900 => 15_000,
        _ => 30_000,
    }
}

/// Best-effort cold/edge build age for soft-wall + adaptive retry.
///
/// Prefers phase-1 `in_progress` clock, then FullEdge `job_started_unix`.
pub fn elapsed_build_secs(state: &AppState, root: &str) -> u64 {
    if let Ok(map) = state.in_progress.try_read() {
        if let Some(p) = map.get(root) {
            return p.start_time.elapsed().as_secs();
        }
    }
    if let Some(t) = get_telemetry(state, root) {
        use std::sync::atomic::Ordering;
        let started = t.job_started_unix.load(Ordering::Relaxed);
        if started > 0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return now.saturating_sub(started);
        }
    }
    0
}

/// Machine + human wait contract for BUILDING responses.
pub fn wait_policy_block(
    elapsed_s: u64,
    soft_wall_s: u64,
    confirm_long_wait: bool,
    progress_note: Option<&str>,
) -> (String, serde_json::Value) {
    let retry_ms = adaptive_retry_after_ms(elapsed_s);
    let soft_exceeded = elapsed_s >= soft_wall_s;
    let advice = if soft_exceeded && !confirm_long_wait {
        "confirm_continue"
    } else if soft_exceeded && confirm_long_wait {
        "continue_confirmed"
    } else {
        "retry"
    };
    let status = if soft_exceeded && !confirm_long_wait {
        "BUILDING_SOFT_WALL"
    } else {
        "BUILDING"
    };
    // T.3: never leave agents with a silent meter — always a concrete next step.
    let next_action = if soft_exceeded && !confirm_long_wait {
        "set confirm_long_wait=true to keep polling, or abort and re-open with tighter scope_paths"
            .to_string()
    } else {
        format!(
            "retry same request after ~{}ms; use toc as scope_paths when present; do not poll edge_builds for hydrate",
            retry_ms
        )
    };
    let mut lines = vec![
        "=== Wait policy (adaptive) ===".into(),
        format!("status: {status}"),
        format!("elapsed_s: {elapsed_s}"),
        format!("soft_wall_s: {soft_wall_s}"),
        format!("retry_after_ms: {retry_ms}"),
        format!("advice: {advice}"),
        format!("confirm_long_wait: {confirm_long_wait}"),
        // Agent meter contract — prevents waiting on FullEdge for hydrate readiness
        "wait_on: retry_same_context_request".into(),
        "do_not: poll /mcp/health edge_builds for hydrate readiness (FullEdge progress only)".into(),
        "ready_when: same Trace returns non-BUILDING (or usable partial with target/toc)".into(),
        format!("next: {next_action}"),
    ];
    if let Some(n) = progress_note {
        if !n.is_empty() {
            lines.push(format!("progress_note: {n}"));
        }
    }
    if soft_exceeded && !confirm_long_wait {
        lines.push(String::new());
        lines.push(format!(
            "SOFT WALL: cold open has run ~{elapsed_s}s (soft limit {soft_wall_s}s / 15 min default)."
        ));
        lines.push(
            "Butler is still working (or stuck under pressure) — not a silent hang, but continuing may burn CPU/RAM."
                .into(),
        );
        lines.push(
            "To continue polling: resend the same request with confirm_long_wait=true (operator \"are you sure?\")."
                .into(),
        );
        lines.push(
            "To stop: abort retries; free RAM / drop other warms; prefer scope_paths; or butler warm offline."
                .into(),
        );
    } else if soft_exceeded && confirm_long_wait {
        lines.push(format!(
            "soft wall confirmed — continuing past {soft_wall_s}s (retry_after_ms={retry_ms})."
        ));
    } else {
        lines.push(format!(
            "adaptive: retry in ~{}s (backs off as wait grows; soft wall at {}s).",
            retry_ms / 1000,
            soft_wall_s
        ));
    }
    let structured = serde_json::json!({
        "status": status,
        "next_action": next_action,
        "wait_policy": {
            "status": status,
            "elapsed_s": elapsed_s,
            "soft_wall_s": soft_wall_s,
            "retry_after_ms": retry_ms,
            "advice": advice,
            "confirm_long_wait": confirm_long_wait,
            "progress_note": progress_note,
            "next_action": next_action,
            // Machine contract: wrong-meter class of bugs
            "wait_on": "retry_same_context_request",
            "do_not_poll": ["mcp_health.edge_builds_for_hydrate"],
            "edge_builds_means": "FullEdge_progress_only",
            "ready_when": "context_returns_non_BUILDING",
            "loaded_graphs_on_health": "mcp_health.loaded",
        }
    });
    (lines.join("\n"), structured)
}

/// First-use friendly building message with meter + phase + hang-on.
pub fn building_graph_message(percent: usize) -> String {
    building_progress_message(percent, "working (cold)", None, None)
}

/// Complete (or partial) cache on SSD — rehydrate into RAM, not a first-time scan.
///
/// Keeps the `=== Building Graph` prefix so MCP cold-retry still fires, but labels
/// the phase honestly so agents/humans do not think FullEdge is re-running.
pub fn hydrating_graph_message(percent: usize) -> String {
    let bar = progress_bar(percent, 20);
    format!(
        "=== Building Graph (hydrating cache) ({percent}%) ===\n\
         {bar}\n\
         phase: hydrating from disk cache\n\n\
         Warehouse is Complete (or partial) on SSD — loading into RAM (parallel shards).\n\
         Not a full rebuild. Retry the **same** Trace/context request (see wait_policy.retry_after_ms).\n\
         Do **not** wait for /mcp/health edge_builds — that meter is FullEdge only; hydrate often never lists there.\n\
         Once resident: hot Trace is milliseconds (first inject may cost ~1s once)."
    )
}

/// Cheap top-level directory TOC from a partial warehouse (O(files) or sampled nodes).
///
/// Used so agents can start scoping while FullEdge / Phase-1 still runs.
pub fn cheap_toc_dirs(graph: &CodeGraph, max: usize) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    let take_path = |path: &str, dirs: &mut BTreeSet<String>| {
        let p = path.replace('\\', "/");
        let p = p.trim_start_matches("./");
        // Skip absolute mount noise — keep repo-relative when possible.
        let rel = p
            .rsplit_once("/test_repos/")
            .map(|(_, r)| r)
            .or_else(|| p.rsplit_once("/projects/").map(|(_, r)| r))
            .unwrap_or(p);
        // Drop project root segment when path is `gecko-dev/xpcom/...`
        let mut parts = rel.split('/').filter(|s| !s.is_empty());
        let first = parts.next().unwrap_or("");
        let second = parts.next();
        // Prefer first product dir: `xpcom/`, `src/`, `crates/` — if first looks like repo name
        // and second exists, use second for monorepo checkouts under test_repos/NAME/...
        let seg = if second.is_some()
            && (first.contains('-') || first == "gecko-dev" || first.starts_with("lambda"))
        {
            second.unwrap_or(first)
        } else {
            first
        };
        if seg.is_empty() || seg.starts_with('.') {
            return;
        }
        // Noise dirs never go in cold TOC (agent should not scope into them).
        const SKIP: &[&str] = &[
            "tests", "test", "docs", "doc", "examples", "benches", "vendor", "third_party",
            "third-party", "node_modules", "target", ".git",
        ];
        if SKIP.iter().any(|s| seg.eq_ignore_ascii_case(s)) {
            return;
        }
        dirs.insert(format!("{seg}/"));
    };
    if !graph.file_hashes.is_empty() {
        for path in graph.file_hashes.keys() {
            take_path(path, &mut dirs);
            if dirs.len() >= max * 2 {
                break;
            }
        }
    } else {
        for (i, b) in graph.nodes.values().enumerate() {
            if i > 20_000 {
                break;
            }
            take_path(&b.file.to_string_lossy(), &mut dirs);
            if dirs.len() >= max * 2 {
                break;
            }
        }
    }
    dirs.into_iter().take(max).collect()
}

/// Usable-while-building body: progress + TOC + agent action (never empty adventure).
///
/// Keeps the `=== Building Graph (cold)` prefix so MCP bridges still detect cold open,
/// then adds a **BUILDING** / **BUILDING_SOFT_WALL** contract agents can parse.
/// Soft wall: after [`soft_wall_secs`] without `confirm_long_wait`, status flips to
/// `BUILDING_SOFT_WALL` (operator "are you sure?").
pub fn usable_building_message_with_confirm(
    percent: usize,
    phase: &str,
    detail: Option<&str>,
    elapsed_secs: Option<u64>,
    toc: &[String],
    provisional_seed: Option<&str>,
    confirm_long_wait: bool,
) -> (String, serde_json::Value) {
    let elapsed = elapsed_secs.unwrap_or(0);
    let soft_wall = soft_wall_secs();
    let (wait_txt, wait_json) =
        wait_policy_block(elapsed, soft_wall, confirm_long_wait, detail);
    let soft_block = elapsed >= soft_wall && !confirm_long_wait;

    let mut lines = vec![building_progress_message(
        percent,
        phase,
        detail,
        elapsed_secs,
    )];
    lines.push(String::new());
    lines.push("=== Usable while building ===".into());
    if soft_block {
        lines.push("status: BUILDING_SOFT_WALL".into());
    } else {
        lines.push("status: BUILDING".into());
    }
    lines.push(format!("progress: {}%", percent.min(99)));
    lines.push(format!("phase: {phase}"));
    if let Some(d) = detail {
        if !d.is_empty() {
            lines.push(format!("detail: {d}"));
        }
    }
    if toc.is_empty() {
        lines.push(format!(
            "toc: (paths still collecting — retry in ~{}s)",
            adaptive_retry_after_ms(elapsed) / 1000
        ));
    } else {
        lines.push(format!("toc ({} top-level dirs so far):", toc.len()));
        for d in toc {
            lines.push(format!("  - {d}"));
        }
        lines.push(
            "action: pass scope_paths to one TOC dir for Trace/Arch while edges finish; rewalk when progress climbs."
                .into(),
        );
    }
    if let Some(seed) = provisional_seed {
        lines.push(format!("provisional_seed: {seed}"));
        lines.push(
            "note: seed is inventory-only (callers may grow). Do not treat 0 callers as dead code."
                .into(),
        );
    }
    lines.push(
        "contract: intentional work-while-cold — not a hang. Honor wait_policy.retry_after_ms; at soft wall require confirm_long_wait."
            .into(),
    );
    lines.push(String::new());
    lines.push(wait_txt);
    (lines.join("\n"), wait_json)
}

/// Rich progress for phase-1 scan / edge build / lock contention.
pub fn building_progress_message(
    percent: usize,
    phase: &str,
    detail: Option<&str>,
    elapsed_secs: Option<u64>,
) -> String {
    let bar = progress_bar(percent, 20);
    let mut lines = vec![
        format!("=== Building Graph (cold) ({percent}%) ==="),
        bar,
        format!("phase: {phase}"),
    ];
    if let Some(d) = detail {
        if !d.is_empty() {
            lines.push(format!("detail: {d}"));
        }
    }
    if let Some(s) = elapsed_secs {
        lines.push(format!("elapsed: {s}s"));
    }
    lines.push(String::new());
    lines.push(
        "Cold open: Butler is building the CodeGraph for this project (first open / empty cache)."
            .into(),
    );
    lines.push(
        "Depending on repo size this may take a while — small repos seconds; large monorepos minutes."
            .into(),
    );
    lines.push(
        "Work: AST scan (Tree-sitter) → symbols → call/usage edges. Progress % / file detail should climb."
            .into(),
    );
    lines.push(
        "Not stuck if the meter or current file changes. Retry in a few seconds (MCP bridge auto-retries)."
            .into(),
    );
    lines.push(
        "Tip: scope_paths:[\"src/\"] is root-anchored (<project>/src only); nest monorepos as cli/src/. Prefer ignore tests/docs."
            .into(),
    );
    lines.join("\n")
}

/// Phase-1 (cold scan) message from in-progress tracker.
pub fn phase1_progress_message(root: &str, progress: &BuildProgress) -> String {
    phase1_progress_message_with_toc(root, progress, &[])
}

/// Phase-1 message with optional partial TOC (progressive L1 publish).
pub fn phase1_progress_message_with_toc(
    root: &str,
    progress: &BuildProgress,
    toc: &[String],
) -> String {
    phase1_progress_message_with_toc_confirm(root, progress, toc, false).0
}

/// Phase-1 + soft-wall confirm + structured wait_policy.
pub fn phase1_progress_message_with_toc_confirm(
    root: &str,
    progress: &BuildProgress,
    toc: &[String],
    confirm_long_wait: bool,
) -> (String, serde_json::Value) {
    let elapsed = progress.start_time.elapsed().as_secs();
    let current = progress
        .current_file
        .as_ref()
        .and_then(|m| m.lock().ok().and_then(|g| g.clone()))
        .unwrap_or_else(|| "…".to_string());
    let deferred = {
        let n = current.to_ascii_lowercase();
        n.starts_with("deferred:")
            || n.contains("host memory pressure")
            || n.contains("deferred warehouse")
            || n.contains("deferred cold")
            || n.contains("deferred loading")
    };
    if deferred {
        let mut lines = vec![
            "=== Warehouse open deferred (host memory pressure) ===".to_string(),
            format!("detail: {current}"),
            format!("elapsed: {elapsed}s"),
            String::new(),
            "Butler refused a heavy cold scan / multi-GiB cache install to avoid OOM on a stressed host."
                .into(),
            "This is intentional — not a hang. Free RAM (or stop co-tenants), then retry the same request."
                .into(),
            "Tip: prefer root-anchored scope_paths like [\"dom/base/\"] once scanning is allowed; avoid dual-warming leviathans."
                .into(),
            format!("project: {root}"),
        ];
        let p = code_graph::snapshot();
        lines.push(format!("pressure: {}", p.summary_line()));
        return (
            lines.join("\n"),
            serde_json::json!({"wait_policy": {"status": "DEFERRED", "advice": "free_ram_then_retry"}}),
        );
    }
    // Phase-1 has no fine % until edges; show indeterminate-ish but rising with time cap 40%
    let soft = ((elapsed as usize) * 2).min(40);
    let (mut msg, wait_json) = usable_building_message_with_confirm(
        soft,
        "scan (Tree-sitter / first open)",
        Some(&format!("file: {current}")),
        Some(elapsed),
        toc,
        None,
        confirm_long_wait,
    );
    msg.push_str(&format!("\nproject: {root}\n"));
    (msg, wait_json)
}

/// Edge/cold building pack for orchestrate + HTTP.
///
/// Returns `(content, percent, phase, toc, provisional, wait_policy_json)`.
pub fn usable_building_pack_confirm(
    state: &AppState,
    root: &str,
    graph: Option<&CodeGraph>,
    live: bool,
    symbol: Option<&str>,
    confirm_long_wait: bool,
) -> (
    String,
    usize,
    String,
    Vec<String>,
    Option<String>,
    serde_json::Value,
) {
    use std::sync::atomic::Ordering;
    let percent = percent_for_status(state, root, graph);
    let elapsed = elapsed_build_secs(state, root);
    let (phase, detail) = if let Some(t) = get_telemetry(state, root) {
        let done = t.files_processed.load(Ordering::Relaxed);
        let total = t.files_total.load(Ordering::Relaxed).max(1);
        let phase = match t.state() {
            BackgroundEdgeBuildState::Running if percent >= 95 => "finalize (polyglot / hubs)",
            BackgroundEdgeBuildState::Running => "edges (multi-core)",
            BackgroundEdgeBuildState::Complete => "complete",
            BackgroundEdgeBuildState::Incomplete => "edges incomplete",
            BackgroundEdgeBuildState::Cancelled => "cancelled",
            BackgroundEdgeBuildState::Error => "error",
            BackgroundEdgeBuildState::NotStarted => {
                if live {
                    "scan / starting"
                } else {
                    "starting"
                }
            }
        };
        (phase.to_string(), format!("files {done}/{total}"))
    } else if live {
        ("working (cold)".into(), String::new())
    } else {
        ("edges incomplete".into(), String::new())
    };
    let toc = graph
        .map(|g| cheap_toc_dirs(g, 12))
        .unwrap_or_default();
    let provisional = symbol.and_then(|sym| {
        let sym = sym.trim();
        if sym.is_empty() {
            return None;
        }
        let g = graph?;
        // O(hits) name index only — never scan.
        let files = g.files_for_name(if sym.contains("::") {
            sym.rsplit("::").next().unwrap_or(sym)
        } else {
            sym
        });
        if files.is_empty() {
            return None;
        }
        Some(format!(
            "{} ({} inventory file hit(s); edges may still grow)",
            files[0].display(),
            files.len()
        ))
    });
    let (content, wait_json) = usable_building_message_with_confirm(
        percent,
        &phase,
        if detail.is_empty() {
            None
        } else {
            Some(detail.as_str())
        },
        Some(elapsed),
        &toc,
        provisional.as_deref(),
        confirm_long_wait,
    );
    (content, percent, phase, toc, provisional, wait_json)
}

/// Edge-build message from telemetry.
pub fn edge_progress_message(state: &AppState, root: &str, graph: Option<&CodeGraph>) -> String {
    let percent = percent_for_status(state, root, graph);
    let (phase, detail) = if let Some(t) = get_telemetry(state, root) {
        use std::sync::atomic::Ordering;
        let done = t.files_processed.load(Ordering::Relaxed);
        let total = t.files_total.load(Ordering::Relaxed).max(1);
        let phase = match t.state() {
            BackgroundEdgeBuildState::Running if percent >= 95 => "finalize (polyglot / hubs)",
            BackgroundEdgeBuildState::Running => "edges (multi-core)",
            BackgroundEdgeBuildState::Complete => "complete",
            BackgroundEdgeBuildState::Incomplete => "edges incomplete",
            BackgroundEdgeBuildState::Cancelled => "cancelled",
            BackgroundEdgeBuildState::Error => "error",
            BackgroundEdgeBuildState::NotStarted => "starting",
        };
        (phase, format!("files {done}/{total}"))
    } else {
        ("edges", String::new())
    };
    building_progress_message(percent, phase, Some(&detail), None)
}

/// Lookup decoupled telemetry without blocking on the status map writer.
pub fn get_telemetry(state: &AppState, root: &str) -> Option<Arc<BgBuildProgress>> {
    state
        .edge_build_status
        .try_read()
        .ok()
        .and_then(|m| m.get(root).cloned())
}

pub fn get_or_create_telemetry(
    state: &AppState,
    root: &str,
    files_total: usize,
) -> Arc<BgBuildProgress> {
    if let Some(t) = get_telemetry(state, root) {
        if files_total > 0 {
            t.files_total
                .store(files_total, std::sync::atomic::Ordering::Relaxed);
        }
        return t;
    }
    let t = BgBuildProgress::new(files_total);
    if let Ok(mut m) = state.edge_build_status.try_write() {
        m.insert(root.to_string(), Arc::clone(&t));
    } else {
        let mut m = state.edge_build_status.blocking_write();
        m.insert(root.to_string(), Arc::clone(&t));
    }
    t
}

/// Authoritative **edge-map complete** check (telemetry + graph inventory).
pub fn is_telemetry_complete(state: &AppState, root: &str) -> bool {
    get_telemetry(state, root)
        .map(|t| t.state() == BackgroundEdgeBuildState::Complete)
        .unwrap_or(false)
}

/// True when full edge inventory is mapped (not merely "skeleton ready" or "has some edges").
pub fn is_edge_build_complete(state: &AppState, root: &str, graph: Option<&CodeGraph>) -> bool {
    if let Some(g) = graph {
        if g.is_edge_build_complete() {
            return true;
        }
        // Telemetry Complete alone is not enough if inventory still open (stale stamp).
        return false;
    }
    is_telemetry_complete(state, root)
}

/// Skeleton loaded — may serve Arch / partial Trace. Prefer `is_edge_build_complete` for "done".
#[allow(dead_code)] // used by status helpers / future Trace gates
pub fn is_ready_to_serve(state: &AppState, root: &str, graph: Option<&CodeGraph>) -> bool {
    if is_edge_build_complete(state, root, graph) {
        return true;
    }
    graph.map(|g| g.is_ready_to_serve()).unwrap_or(false)
}

/// Align lock-free telemetry only when the finite edge inventory is fully mapped.
pub fn sync_telemetry_if_graph_ready(state: &AppState, root: &str, graph: &CodeGraph) {
    if !graph.is_edge_build_complete() {
        // Keep progress honest while inventory is open.
        if let Some(t) = get_telemetry(state, root) {
            graph.sync_bg_telemetry(Some(&t));
        }
        return;
    }
    // O(1): never rewalk edgeable_file_inventory (1M PathBuf clones) just to stamp telemetry.
    let files_done = if graph.edge_inventory_closed {
        graph.files_with_edges.len().max(1)
    } else {
        let (done, total) = graph.edge_inventory_progress();
        done.max(total).max(1)
    };
    let telemetry = get_or_create_telemetry(state, root, files_done);
    telemetry.mark_fully_complete(files_done);
}

/// Instant response when the graph `RwLock` writer is active (never block on `.read()`).
///
/// Stream-merge edge batches hold the write lock only briefly. Returning a hollow
/// "Building Graph" here was the pytorch blackout: skeleton already in RAM, product
/// unusable for the entire full-edge grind. Callers should brief-block on
/// [`read_graph_for_serve`] instead.
pub fn try_lock_contention_building(
    state: &AppState,
    root: &str,
    graph_rw: &Arc<std::sync::RwLock<CodeGraph>>,
) -> Option<String> {
    if is_telemetry_complete(state, root) {
        return None;
    }

    if let Ok(g) = graph_rw.try_read() {
        if g.is_edge_build_complete() {
            sync_telemetry_if_graph_ready(state, root, &g);
        }
        // Readable snapshot: never Building for lock reasons (skeleton or edged).
        return None;
    }

    // Writer held (edge batch merge / JIT). Do **not** surface Building — merge is
    // short; serve path waits on the lock. Only Phase-1 empty shell should hang-on
    // (handled by try_phase1_scan_building / in_progress).
    if let Some(t) = get_telemetry(state, root) {
        use std::sync::atomic::Ordering;
        if t.thread_active.load(Ordering::Relaxed)
            || matches!(
                t.state(),
                BackgroundEdgeBuildState::Running
                    | BackgroundEdgeBuildState::Incomplete
                    | BackgroundEdgeBuildState::Complete
            )
        {
            return None;
        }
    }

    // No telemetry + lock held: still prefer wait-to-serve over a fake progress page.
    None
}

/// Phase 1 AST scan in flight (async cold load).
pub fn try_phase1_scan_building(
    state: &AppState,
    root: &str,
    graph: Option<&CodeGraph>,
) -> Option<String> {
    // Skeleton already in RAM → serve (progressive publish / finished install).
    // Sticky 40% "scan" while in_progress still set was the torch warm-load blackout.
    if let Some(g) = graph {
        if !g.nodes.is_empty() {
            return None;
        }
        if g.nodes.is_empty() && !g.is_ready_to_serve() {
            // Fall through to in_progress message if any; else starting shell.
            if state
                .in_progress
                .try_read()
                .ok()
                .is_some_and(|m| m.contains_key(root))
            {
                // empty shell + scanning
            } else {
                return Some(building_progress_message(
                    0,
                    "scan (starting)",
                    Some("empty graph shell"),
                    None,
                ));
            }
        }
    }
    if let Ok(map) = state.in_progress.try_read() {
        if let Some(progress) = map.get(root) {
            return Some(phase1_progress_message(root, progress));
        }
    }
    None
}

/// Fast-fail for live background builds (context / balanced polling paths).
///
/// **Product rule (monster-usable):** once a skeleton is in RAM, serve Arch / Trace / Find
/// while full edges grind in the background. JIT fills symbol files. Never black out the
/// agent for the entire edge pass (`block_on_live_build` only matters when there is still
/// **no** skeleton — empty shell / phase-1).
pub fn try_building_fast_fail(
    state: &AppState,
    root: &str,
    graph_rw: &Arc<std::sync::RwLock<CodeGraph>>,
    block_on_live_build: bool,
) -> Option<String> {
    if let Some(msg) = try_phase1_scan_building(state, root, graph_rw.try_read().ok().as_deref()) {
        return Some(msg);
    }

    if is_edge_build_complete(state, root, graph_rw.try_read().ok().as_deref()) {
        return None;
    }

    if let Ok(g) = graph_rw.try_read() {
        if g.is_edge_build_complete() {
            sync_telemetry_if_graph_ready(state, root, &g);
            return None;
        }
        // Skeleton present → always serve (Trace JIT / Arch hubs). Full edge is background.
        if !g.nodes.is_empty() || g.is_ready_to_serve() {
            return None;
        }
    }

    // Lock held mid edge-merge: do not Building-blackout (see try_lock_contention_building).
    if let Some(msg) = try_lock_contention_building(state, root, graph_rw) {
        return Some(msg);
    }

    // No readable nodes yet. Only then optionally wait on live edge/scan.
    if !block_on_live_build {
        return None;
    }

    let telemetry = get_telemetry(state, root);
    if let Some(t) = &telemetry {
        use std::sync::atomic::Ordering;
        // Edge worker alive but skeleton may already exist under a brief write lock —
        // prefer serve, not hang-on, once inventory is known.
        if t.files_total.load(Ordering::Relaxed) > 0 && t.is_live_build() {
            return None;
        }
        if t.is_live_build() {
            return Some(edge_progress_message(
                state,
                root,
                graph_rw.try_read().ok().as_deref(),
            ));
        }
    }

    if let Ok(gg) = graph_rw.try_read() {
        if gg.nodes.is_empty() && gg.is_background_edge_build_in_progress() {
            return Some(edge_progress_message(state, root, Some(&gg)));
        }
    }

    None
}

pub fn is_live_build(state: &AppState, root: &str, graph: Option<&CodeGraph>) -> bool {
    if is_edge_build_complete(state, root, graph) {
        return false;
    }
    if is_stale_dead_worker(state, root) {
        // Dead worker but inventory open — not "live", but should resuscitate.
        return false;
    }
    if let Some(t) = get_telemetry(state, root) {
        if t.is_live_build() {
            return true;
        }
    }
    graph
        .map(|g| g.is_background_edge_build_in_progress())
        .unwrap_or(false)
}

/// True when the bg worker is gone but telemetry never reached `Complete` (panic/cancel).
pub fn is_stale_dead_worker(state: &AppState, root: &str) -> bool {
    use std::sync::atomic::Ordering;
    let Some(t) = get_telemetry(state, root) else {
        return false;
    };
    if t.thread_active.load(Ordering::Relaxed) {
        return false;
    }
    matches!(
        t.state(),
        BackgroundEdgeBuildState::Error
            | BackgroundEdgeBuildState::Incomplete
            | BackgroundEdgeBuildState::Cancelled
    )
}

pub fn percent_for_status(state: &AppState, root: &str, graph: Option<&CodeGraph>) -> usize {
    if is_edge_build_complete(state, root, graph) {
        return 100;
    }
    // Prefer lock-free telemetry while FullEdge is live: `files_processed` advances
    // mid-batch (parse), whereas `edge_inventory_progress` only moves after merge —
    // torch sat at 0% for minutes with one core burning and no honest meter.
    if let Some(t) = get_telemetry(state, root) {
        let live = t.is_live_build()
            || matches!(
                t.state(),
                BackgroundEdgeBuildState::Running | BackgroundEdgeBuildState::Incomplete
            );
        if live {
            // BgBuildProgress::percent floors at 1% after first file (honest mid-batch).
            return t.percent().min(99);
        }
    }
    if let Some(g) = graph {
        let (done, total) = g.edge_inventory_progress();
        if total > 0 {
            return ((done as u64 * 100) / total as u64).min(99) as usize;
        }
    }
    if let Some(t) = get_telemetry(state, root) {
        // Cap below 100 until inventory complete — avoid false "done" from JIT counters.
        return t.percent().min(99);
    }
    if let Some(g) = graph {
        if g.is_edge_build_complete() {
            return 100;
        }
        let total = g.nodes.len();
        let done = g
            .edges_built_count
            .load(std::sync::atomic::Ordering::Relaxed);
        if total == 0 {
            0
        } else {
            ((done as u64 * 100) / total as u64).min(100) as usize
        }
    } else {
        0
    }
}

pub fn state_label(
    state: &AppState,
    root: &str,
    graph: Option<&CodeGraph>,
    live: bool,
) -> (usize, String) {
    if is_edge_build_complete(state, root, graph) {
        return (100, "Complete".to_string());
    }
    let percent = percent_for_status(state, root, graph);
    let status = if live {
        "Building".to_string()
    } else if let Some(g) = graph {
        let (done, total) = g.edge_inventory_progress();
        if total > 0 {
            format!("Mapping {done}/{total} files")
        } else {
            format!("{:?}", g.background_edge_build_state)
        }
    } else if let Some(t) = get_telemetry(state, root) {
        format!("{:?}", t.state())
    } else {
        "Unknown".to_string()
    };
    (percent, status)
}

// ── Honest Partial (agent–machine contract) ──────────────────────────────────

/// Confidence ladder for Trace/Find while the warehouse is still grinding.
///
/// Agents adapt: incomplete + 0 callers ⇒ rewalk later, **not** "dead code".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfidenceRung {
    /// Phase-1 / nodes present; weak or no name_index use.
    Inventory,
    /// Exact `name_index` hit; edges thin or missing for seed.
    IndexExact,
    /// Some callers/callees for the seed; FullEdge not done.
    EdgesPartial,
    /// Full edge inventory complete — counts may be treated as final.
    EdgesFull,
}

impl ConfidenceRung {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inventory => "inventory",
            Self::IndexExact => "index_exact",
            Self::EdgesPartial => "edges_partial",
            Self::EdgesFull => "edges_full",
        }
    }

    pub fn is_full(self) -> bool {
        matches!(self, Self::EdgesFull)
    }
}

/// Pick confidence from graph completeness + whether the seed was index-exact and edged.
pub fn confidence_rung(
    state: &AppState,
    root: &str,
    graph: Option<&CodeGraph>,
    exact_name_hits: bool,
    seed_has_edges: bool,
) -> ConfidenceRung {
    if is_edge_build_complete(state, root, graph) {
        return ConfidenceRung::EdgesFull;
    }
    if seed_has_edges {
        return ConfidenceRung::EdgesPartial;
    }
    if exact_name_hits {
        return ConfidenceRung::IndexExact;
    }
    ConfidenceRung::Inventory
}

/// Multi-line banner for dense content / HTTP body. Empty when `edges_full`.
pub fn honest_partial_banner(
    percent: usize,
    confidence: ConfidenceRung,
    detail: Option<&str>,
) -> String {
    if confidence.is_full() {
        return String::new();
    }
    let bar = progress_bar(percent.min(99), 20);
    let mut lines = vec![
        format!(
            "=== Honest partial · {}% · confidence: {} · edges incomplete ===",
            percent.min(99),
            confidence.as_str()
        ),
        bar,
    ];
    if let Some(d) = detail {
        if !d.is_empty() {
            lines.push(format!("phase: {d}"));
        }
    }
    lines.push(
        "Rewalk later for denser callers/callees. Counts below are so-far, not final.".into(),
    );
    lines.push(
        "Do not treat 0 callers as dead code while confidence ≠ edges_full.".into(),
    );
    lines.join("\n")
}

/// One-line stamp for compact Trace (7B-safe).
pub fn honest_partial_tag(percent: usize, confidence: ConfidenceRung) -> String {
    if confidence.is_full() {
        return String::new();
    }
    format!(
        "[partial {}% · {} · rewalk]",
        percent.min(99),
        confidence.as_str()
    )
}

/// Warning / mode token for provisional miss (never `no_structural_hits`).
pub fn provisional_miss_token(percent: usize) -> String {
    format!("symbol_not_seen_yet@{}%", percent.min(99))
}

/// Non-blocking graph read; returns `None` when the writer lock is held.
pub fn try_read_graph<'a>(
    graph_rw: &'a Arc<std::sync::RwLock<CodeGraph>>,
) -> Option<std::sync::RwLockReadGuard<'a, CodeGraph>> {
    graph_rw.try_read().ok()
}

/// Post-complete serving path: non-blocking read first, then brief blocking read.
/// Stream-merge edge batches hold the write lock only briefly — waiting beats a hollow
/// "Building Graph" response for Trace when the skeleton is already in RAM.
pub fn read_graph_for_serve<'a>(
    state: &AppState,
    root: &str,
    graph_rw: &'a Arc<std::sync::RwLock<CodeGraph>>,
) -> Option<std::sync::RwLockReadGuard<'a, CodeGraph>> {
    if let Some(g) = try_read_graph(graph_rw) {
        return Some(g);
    }
    if is_telemetry_complete(state, root) {
        return graph_rw.read().ok();
    }
    // Live edge build or symbol Trace: wait for the batch merge to release.
    graph_rw.read().ok()
}

/// True when the warehouse skeleton is known present (serve Trace/Arch, never cold Phase-1).
///
/// Handles the gecko/torch case: FullEdge holds write briefly → `try_read` fails, but
/// 4M+ nodes are already installed. Prefer brief serve-read; fall back to edge telemetry.
pub fn skeleton_present_for_serve(
    state: &AppState,
    root: &str,
    graph_rw: &Arc<std::sync::RwLock<CodeGraph>>,
) -> bool {
    if let Some(g) = try_read_graph(graph_rw) {
        return !g.nodes.is_empty() || g.is_ready_to_serve();
    }
    if let Some(g) = read_graph_for_serve(state, root, graph_rw) {
        return !g.nodes.is_empty() || g.is_ready_to_serve();
    }
    use std::sync::atomic::Ordering;
    if let Some(t) = get_telemetry(state, root) {
        // Inventory opened / FullEdge running ⇒ progressive or cache install already published.
        if t.files_total.load(Ordering::Relaxed) > 0 {
            return true;
        }
        if t.is_live_build() && t.files_processed.load(Ordering::Relaxed) > 0 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod usable_building_tests {
    use super::*;
    use code_graph::snooper::model::{BlockInfo, CodeGraph, Id};
    use std::path::PathBuf;

    fn block(file: &str, name: &str) -> BlockInfo {
        let hash = format!("{name:0<16}");
        BlockInfo {
            id: Id::new(file, "function_item", &hash),
            name: name.into(),
            file: PathBuf::from(file),
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
            external_crates: Default::default(),
            is_highly_connected: false,
        }
    }

    #[test]
    fn usable_building_message_has_building_contract() {
        let toc = vec!["xpcom/".into(), "dom/".into()];
        let (msg, _) = usable_building_message_with_confirm(
            28,
            "edges (multi-core)",
            Some("files 10/100"),
            Some(12),
            &toc,
            Some("xpcom/threads/Mutex.h (3 inventory file hit(s); edges may still grow)"),
            false,
        );
        assert!(msg.contains("=== Building Graph (cold) (28%) ==="), "{msg}");
        assert!(msg.contains("status: BUILDING"), "{msg}");
        assert!(msg.contains("progress: 28%"), "{msg}");
        assert!(msg.contains("toc (2 top-level dirs so far):"), "{msg}");
        // No silent meter: wait policy always names next step
        let (_, wait) = wait_policy_block(12, 900, false, None);
        assert!(
            wait.get("next_action").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
            "BUILDING wait_policy must stamp next_action: {wait}"
        );
        assert!(
            wait.pointer("/wait_policy/next_action")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("retry")),
            "{wait}"
        );
        assert!(msg.contains("  - xpcom/"), "{msg}");
        assert!(msg.contains("provisional_seed:"), "{msg}");
        assert!(msg.contains("usable while building") || msg.contains("Usable while building"), "{msg}");
        assert!(msg.contains("Not a hang") || msg.contains("not a hang"), "{msg}");
        assert!(msg.contains("retry_after_ms:"), "{msg}");
        assert!(msg.contains("soft_wall_s:"), "{msg}");
    }

    #[test]
    fn hydrate_answer_grace_default_is_answer_shaped() {
        // Don't inherit a caller env (cargo test process).
        std::env::remove_var("BUTLER_HYDRATE_GRACE_MS");
        assert_eq!(hydrate_answer_grace_ms(), 500);
        std::env::set_var("BUTLER_HYDRATE_GRACE_MS", "0");
        assert_eq!(hydrate_answer_grace_ms(), 0);
        std::env::set_var("BUTLER_HYDRATE_GRACE_MS", "99999");
        assert_eq!(hydrate_answer_grace_ms(), 5_000);
        std::env::remove_var("BUTLER_HYDRATE_GRACE_MS");
    }

    #[test]
    fn soft_wall_requires_confirm_after_threshold() {
        let toc = vec!["src/".into()];
        let (msg, wp) = usable_building_message_with_confirm(
            40,
            "edges (multi-core)",
            Some("files 50/200"),
            Some(901),
            &toc,
            None,
            false,
        );
        assert!(msg.contains("status: BUILDING_SOFT_WALL"), "{msg}");
        assert!(msg.contains("confirm_long_wait=true"), "{msg}");
        assert_eq!(
            wp["wait_policy"]["status"].as_str(),
            Some("BUILDING_SOFT_WALL")
        );
        assert_eq!(
            wp["wait_policy"]["advice"].as_str(),
            Some("confirm_continue")
        );

        let (msg2, wp2) = usable_building_message_with_confirm(
            40,
            "edges (multi-core)",
            Some("files 50/200"),
            Some(901),
            &toc,
            None,
            true,
        );
        assert!(msg2.contains("status: BUILDING"), "{msg2}");
        assert!(!msg2.contains("status: BUILDING_SOFT_WALL"), "{msg2}");
        assert_eq!(wp2["wait_policy"]["status"].as_str(), Some("BUILDING"));
        assert_eq!(
            wp2["wait_policy"]["advice"].as_str(),
            Some("continue_confirmed")
        );
    }

    #[test]
    fn adaptive_retry_backs_off() {
        assert_eq!(adaptive_retry_after_ms(5), 1_500);
        assert_eq!(adaptive_retry_after_ms(60), 5_000);
        assert_eq!(adaptive_retry_after_ms(200), 10_000);
        assert_eq!(adaptive_retry_after_ms(600), 15_000);
        assert_eq!(adaptive_retry_after_ms(1200), 30_000);
    }

    #[test]
    fn cheap_toc_skips_tests_and_collects_product_dirs() {
        let mut g = CodeGraph::new();
        g.file_hashes.insert("src/lib.rs".into(), 1);
        g.file_hashes.insert("xpcom/base/nsCOMPtr.h".into(), 1);
        g.file_hashes.insert("tests/unit/foo.rs".into(), 1);
        g.file_hashes.insert("dom/base/nsINode.h".into(), 1);
        let toc = cheap_toc_dirs(&g, 12);
        assert!(toc.iter().any(|d| d == "src/" || d == "xpcom/" || d == "dom/"), "{toc:?}");
        assert!(!toc.iter().any(|d| d == "tests/"), "{toc:?}");
        let b = block("crates/bevy_app/src/app.rs", "App");
        g.nodes.insert(b.id.clone(), b);
        // file_hashes already preferred; still ok
        let _ = cheap_toc_dirs(&g, 8);
    }
}
