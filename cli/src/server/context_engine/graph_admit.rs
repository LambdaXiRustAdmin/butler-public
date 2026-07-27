//! Graph admit / warm / watcher / LRU / background edge (P3 peel).
//! Zero intentional behavior change.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use code_graph::snooper::normalize_path;
use code_graph::snooper::scanner::save_graph_owned;
use code_graph::{graph_cache_exists, load_graph, scan_workspace_with_waves, start_watcher};

use crate::server::build_status::{get_or_create_telemetry, sync_telemetry_if_graph_ready};
use crate::server::paths::*;
use crate::server::state::*;
use crate::vprintln;

static WATCHED_ROOTS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Register live FS watcher **off the request critical path**.
///
/// First gecko/inotify setup can take many seconds; never block Trace/Arch lobby on it.
/// Dedup via `WATCHED_ROOTS` still happens synchronously so we only spawn once.
pub(super) fn ensure_watcher(
    root: &str,
    graph: Arc<std::sync::RwLock<code_graph::CodeGraph>>,
    config_skip_directories: Vec<String>,
) {
    let watched = WATCHED_ROOTS.get_or_init(|| Mutex::new(HashSet::new()));
    let should_start = {
        let mut guard = watched.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(root.to_string())
    };
    if !should_start {
        return;
    }
    let root_owned = root.to_string();
    let graph_for_thread = Arc::clone(&graph);
    let skips_for_thread = config_skip_directories.clone();
    match std::thread::Builder::new()
        .name("butler-watcher-boot".into())
        .spawn(move || {
            let t0 = std::time::Instant::now();
            vprintln!("👀 Starting live watcher (detached) for root: {}", root_owned);
            start_watcher(&root_owned, graph_for_thread, skips_for_thread);
            vprintln!(
                "👀 Watcher boot finished for {} in {:.1}ms",
                root_owned,
                t0.elapsed().as_secs_f64() * 1000.0
            );
        }) {
        Ok(_) => {}
        Err(e) => {
            // Fall back to sync so we still get watches if thread spawn fails.
            eprintln!(
                "⚠️ watcher-boot spawn failed ({e}); starting sync for {}",
                root
            );
            start_watcher(root, graph, config_skip_directories);
        }
    }
}

/// Roots that must stay warm (boot warm_roots + BUTLER_WARM_ROOTS).
pub(super) fn pinned_warm_roots(state: &AppState) -> std::collections::HashSet<String> {
    collect_warm_roots(&state.settings)
        .into_iter()
        .map(|r| {
            let p = Path::new(&r);
            std::fs::canonicalize(p)
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_else(|_| normalize_path(&r))
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// True if this root is unsafe to drop from RAM (scan / live FullEdge / pinned warm).
pub(super) fn must_keep_graph_resident(state: &AppState, root: &str, pinned: &std::collections::HashSet<String>) -> bool {
    if pinned.iter().any(|p| p == root || root.starts_with(p) || p.starts_with(root)) {
        return true;
    }
    if state.in_progress.blocking_read().contains_key(root) {
        return true;
    }
    // Live edge grind — dropping RAM forces multi-minute re-collect.
    if let Ok(m) = state.edge_build_status.try_read() {
        if let Some(t) = m.get(root) {
            use std::sync::atomic::Ordering;
            if t.thread_active.load(Ordering::Relaxed) {
                return true;
            }
            if matches!(
                t.state(),
                code_graph::snooper::BackgroundEdgeBuildState::Running
            ) {
                return true;
            }
        }
    }
    false
}

/// Touch graph root in the warm LRU (most-recent at back). Evict cold roots over cap.
///
/// Never evicts: boot warm roots, Phase-1 in_progress, or live FullEdge roots.
pub(super) fn touch_graph_lru(state: &AppState, root: &str) {
    let max = state.settings.server.max_cached_graphs;
    let pinned = pinned_warm_roots(state);
    let mut lru = match state.graph_lru.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some(pos) = lru.iter().position(|r| r == root) {
        lru.remove(pos);
    }
    lru.push_back(root.to_string());
    if max == 0 {
        return;
    }
    let mut guard = 0usize;
    while lru.len() > max && guard < lru.len().saturating_add(4) {
        guard += 1;
        let Some(evict) = lru.pop_front() else {
            break;
        };
        if evict == root {
            lru.push_back(evict);
            break;
        }
        if must_keep_graph_resident(state, &evict, &pinned) {
            lru.push_back(evict);
            // All remaining protected → stop (avoid spin).
            if lru.iter().all(|r| must_keep_graph_resident(state, r, &pinned)) {
                vprintln!(
                    "🧊 Graph cache over cap ({} > {}) but all roots pinned/live — holding {}",
                    lru.len(),
                    max,
                    lru.len()
                );
                break;
            }
            continue;
        }
        let mut cache = state.graphs.blocking_write();
        if cache.remove(&evict).is_some() {
            vprintln!("🧊 Evicted cold graph from memory: {}", evict);
        }
        drop(cache);
        // Drop telemetry / cancel tokens for evicted root (best-effort)
        if let Ok(mut m) = state.edge_build_status.try_write() {
            m.remove(&evict);
        }
        if let Ok(mut m) = state.edge_build_cancels.try_write() {
            if let Some(tok) = m.remove(&evict) {
                tok.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}

/// True when `in_progress` holds a pressure-deferred open (not an active scan thread).
pub(super) fn is_pressure_deferred_note(note: &str) -> bool {
    let n = note.to_ascii_lowercase();
    n.starts_with("deferred:")
        || n.contains("host memory pressure")
        || n.contains("deferred warehouse")
        || n.contains("deferred cold")
        || n.contains("deferred loading")
}

/// Spawn non-blocking graph load (disk cache or full scan). Inserts empty shell + in_progress.
/// Never runs `load_graph` on the request thread.
///
/// **Pressure admission (P0):** under Red/Black host memory pressure, defers cold full-tree
/// scans and multi‑GiB cache installs instead of user-invoked OOM. Retry on later requests
/// when [`code_graph::may_retry_deferred_open`] allows.
pub(super) fn spawn_async_graph_load(
    state: &AppState,
    root: &str,
    graph_rw: Arc<std::sync::RwLock<code_graph::CodeGraph>>,
) {
    // Active load already running → do not double-spawn.
    if let Ok(map) = state.in_progress.try_read() {
        if let Some(p) = map.get(root) {
            let note = p
                .current_file
                .as_ref()
                .and_then(|m| m.lock().ok())
                .and_then(|g| g.clone())
                .unwrap_or_default();
            if !note.is_empty() && !is_pressure_deferred_note(&note) {
                return;
            }
        }
    }

    let force_rescan = std::env::var("BUTLER_FORCE_RESCAN").is_ok();
    let cold_load = force_rescan || !graph_cache_exists(root);
    let root_path = Path::new(root);

    match code_graph::admit_warehouse_open(root_path, cold_load) {
        code_graph::AdmitDecision::Defer { reason } => {
            let p = code_graph::snapshot();
            vprintln!(
                "🛑 Warehouse open deferred for {} ({}) — {}",
                root,
                p.summary_line(),
                reason
            );
            let current_file = Arc::new(std::sync::Mutex::new(Some(format!(
                "deferred: {}",
                reason
            ))));
            let mut in_progress = state.in_progress.blocking_write();
            in_progress.insert(
                root.to_string(),
                BuildProgress {
                    start_time: Instant::now(),
                    current_file: Some(current_file),
                },
            );
            return;
        }
        code_graph::AdmitDecision::Allow { max_scan_threads } => {
            vprintln!(
                "⚡ Admission allow for {} (cold={} max_scan_threads={} {})",
                root,
                cold_load,
                max_scan_threads,
                code_graph::snapshot().summary_line()
            );
        }
    }

    let current_file = Arc::new(std::sync::Mutex::new(None::<String>));
    {
        let mut in_progress = state.in_progress.blocking_write();
        in_progress.insert(
            root.to_string(),
            BuildProgress {
                start_time: Instant::now(),
                current_file: Some(Arc::clone(&current_file)),
            },
        );
    }

    let root_spawn = root.to_string();
    let skips = state.settings.analysis.skip_directories.clone();
    let in_progress_arc = Arc::clone(&state.in_progress);
    let graph_arc = Arc::clone(&graph_rw);
    let current_file_spawn = Arc::clone(&current_file);
    let thread_name = if cold_load {
        "butler-phase1-scan"
    } else {
        "butler-cache-load"
    };

    vprintln!(
        "⚡ Async {} started for {} (never blocks /context)",
        if cold_load {
            "cold Phase-1 scan"
        } else {
            "disk-cache load"
        },
        root
    );

    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .name(thread_name.into())
        .spawn(move || {
            let loaded = if cold_load {
                // Progressive L1: publish for serve-while-scan, but do **not** clone the
                // full mountain on every wave (pytorch: multi‑M nodes). First real wave +
                // 2× / +100k growth only; final handoff is the move into graph_arc below.
                let graph_pub = Arc::clone(&graph_arc);
                let root_pub = root_spawn.clone();
                let mut last_pub_nodes = 0usize;
                let mut publish = move |g: &code_graph::CodeGraph| {
                    let n = g.nodes.len();
                    if n == 0 {
                        return;
                    }
                    let grew = last_pub_nodes == 0
                        || n >= last_pub_nodes.saturating_mul(2)
                        || n.saturating_sub(last_pub_nodes) >= 100_000;
                    if !grew {
                        return;
                    }
                    last_pub_nodes = n;
                    // Sources already stripped per wave — avoid full slim walk.
                    let snap = g.snapshot_for_publish();
                    let modules = snap.module_hashes.len();
                    if let Ok(mut guard) = graph_pub.write() {
                        *guard = snap;
                    }
                    vprintln!(
                        "📡 Progressive L1 publish for {} ({} blocks, {} modules)",
                        root_pub, n, modules
                    );
                };
                scan_workspace_with_waves(
                    &root_spawn,
                    Some(current_file_spawn),
                    &skips,
                    Some(&mut publish),
                )
            } else {
                load_graph(&root_spawn, Some(current_file_spawn), &skips)
            };
            let n = loaded.nodes.len();
            let modules = loaded.module_hashes.len();
            // Disk snapshot **before** serve install so background save never holds the
            // graph RwLock during multi‑M slim (that blocked FullEdge write enqueue ~30s).
            let disk_snap = if cold_load && n > 0 {
                Some(loaded.slim_for_cache())
            } else {
                None
            };
            // Serve path first — do not wait on multi‑GB bincode/disk (P0 cold ready).
            if let Ok(mut guard) = graph_arc.write() {
                *guard = loaded;
            }
            vprintln!(
                "✅ Async graph ready for {} ({} blocks, {} modules)",
                root_spawn, n, modules
            );
            in_progress_arc.blocking_write().remove(&root_spawn);
            // Persist off the ready path without locking the live warehouse.
            if let Some(snap) = disk_snap {
                let root_save = PathBuf::from(&root_spawn);
                vprintln!(
                    "💾 Queue background graph save for {} (ready path not blocked, lock-free)",
                    root_spawn
                );
                let _ = std::thread::Builder::new()
                    .name("butler-graph-save".into())
                    .spawn(move || {
                        let t0 = std::time::Instant::now();
                        let n = snap.nodes.len();
                        match save_graph_owned(snap, &root_save) {
                            Ok(()) => vprintln!(
                                "💾 Background graph save finished for {} ({} blocks) in {:.1?}",
                                root_save.display(),
                                n,
                                t0.elapsed()
                            ),
                            Err(e) => eprintln!(
                                "⚠️  background graph save failed for {}: {e}",
                                root_save.display()
                            ),
                        }
                    });
            }
        })
        .expect("failed to spawn async graph load thread");
}

/// If the root is an empty shell held for pressure-defer, retry admission when freer.
pub(super) fn maybe_retry_pressure_deferred_load(
    state: &AppState,
    root: &str,
    graph_rw: &Arc<std::sync::RwLock<code_graph::CodeGraph>>,
) {
    let nodes = graph_rw
        .try_read()
        .ok()
        .map(|g| g.nodes.len())
        .unwrap_or(0);
    if nodes > 0 {
        return;
    }
    let note = {
        let map = state.in_progress.blocking_read();
        map.get(root).and_then(|p| {
            p.current_file
                .as_ref()
                .and_then(|m| m.lock().ok())
                .and_then(|g| g.clone())
        })
    };
    let Some(note) = note else {
        return;
    };
    if !is_pressure_deferred_note(&note) {
        return;
    }
    let force_rescan = std::env::var("BUTLER_FORCE_RESCAN").is_ok();
    let cold_load = force_rescan || !graph_cache_exists(root);
    if !code_graph::may_retry_deferred_open(Path::new(root), cold_load) {
        // Refresh deferred stamp with latest snapshot (agent sees current avail).
        if let Ok(mut map) = state.in_progress.try_write() {
            if let Some(p) = map.get_mut(root) {
                if let Some(ref cf) = p.current_file {
                    if let Ok(mut g) = cf.lock() {
                        let psnap = code_graph::snapshot();
                        *g = Some(format!(
                            "deferred: still under pressure ({})",
                            psnap.summary_line()
                        ));
                    }
                }
            }
        }
        return;
    }
    vprintln!(
        "🔄 Retrying pressure-deferred warehouse open for {} ({})",
        root,
        code_graph::snapshot().summary_line()
    );
    state.in_progress.blocking_write().remove(root);
    spawn_async_graph_load(state, root, Arc::clone(graph_rw));
}

/// Boot-time warm: ensure root is in the graph map and loading/watching.
/// Safe to call multiple times; no-ops if already present.
pub fn warm_project_root(state: &AppState, root: &str) {
    // Docker: host `/home/…/projects/foo` → container `/projects/foo` (same as /context).
    // Without this, `butler warm --server` with a host path registers a non-existent root
    // and Phase-1 can publish **0 blocks** while health never shows the real warehouse.
    let root = {
        let translated = translate_client_path(root);
        let p = Path::new(&translated);
        std::fs::canonicalize(p)
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_else(|_| normalize_path(&translated))
    };
    if root.is_empty() {
        return;
    }
    {
        let cache = state.graphs.blocking_read();
        if cache.contains_key(&root) {
            touch_graph_lru(state, &root);
            if let Some(g) = cache.get(&root) {
                maybe_retry_pressure_deferred_load(state, &root, g);
                ensure_watcher(
                    &root,
                    Arc::clone(g),
                    state.settings.analysis.skip_directories.clone(),
                );
                ensure_background_edge_build(state, &root, g);
            }
            return;
        }
    }
    let mut cache = state.graphs.blocking_write();
    if cache.contains_key(&root) {
        drop(cache);
        touch_graph_lru(state, &root);
        return;
    }
    let g_rw = Arc::new(std::sync::RwLock::new(code_graph::CodeGraph::new()));
    cache.insert(root.clone(), Arc::clone(&g_rw));
    drop(cache);
    touch_graph_lru(state, &root);
    spawn_async_graph_load(state, &root, Arc::clone(&g_rw));
    ensure_watcher(
        &root,
        Arc::clone(&g_rw),
        state.settings.analysis.skip_directories.clone(),
    );
    vprintln!("🔥 Warm root registered: {}", root);
}

/// Parse `BUTLER_WARM_ROOTS` (colon or comma separated) plus config `server.warm_roots`.
pub fn collect_warm_roots(settings: &cli::config::ButlerSettings) -> Vec<String> {
    let mut roots: Vec<String> = settings.server.warm_roots.clone();
    if let Ok(env) = std::env::var("BUTLER_WARM_ROOTS") {
        for part in env.split(|c| c == ':' || c == ',') {
            let t = part.trim();
            if !t.is_empty() {
                roots.push(t.to_string());
            }
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

/// Start or resuscitate the background edge builder (Sprint 5/6/7).
///
/// Also used by the **idle warehouse reaper**: Incomplete + free lane → re-enqueue FullEdge
/// instead of sitting at 4% with a truck of excuses.
pub(crate) fn ensure_background_edge_build(
    state: &AppState,
    root: &str,
    graph_rw: &Arc<std::sync::RwLock<code_graph::CodeGraph>>,
) {
    use std::sync::atomic::Ordering;

    let telemetry = get_or_create_telemetry(state, root, 0);

    // Zombie claim: enqueued / stuck before first real progress. Free the lane so we can resume.
    if telemetry.clear_if_heartbeat_stale(90) {
        vprintln!(
            "🔄 FullEdge zombie cleared for {} — lane free for resume",
            root
        );
    }

    // Only stop when the finite edge inventory is fully mapped (not "some edges exist").
    if let Ok(gg) = graph_rw.try_read() {
        if gg.is_edge_build_complete() {
            sync_telemetry_if_graph_ready(state, root, &gg);
            return;
        }
    } else if telemetry.state() == code_graph::snooper::BackgroundEdgeBuildState::Complete {
        // No graph lock — trust telemetry only as weak signal; still try spawn path below.
    }

    let server_running = telemetry.thread_active.load(Ordering::Relaxed);

    if let Ok(mut gg) = graph_rw.try_write() {
        gg.reconcile_stale_running_state(server_running);
        // Clear false Complete when inventory still open.
        if gg.background_edge_build_complete && !gg.is_edge_build_complete() {
            gg.background_edge_build_complete = false;
            gg.background_edge_build_state =
                code_graph::snooper::BackgroundEdgeBuildState::Incomplete;
            vprintln!(
                "🔄 Edge inventory incomplete ({}/{}) — resuming full edge build for {}",
                gg.edge_inventory_progress().0,
                gg.edge_inventory_progress().1,
                root
            );
        }
        // Heal false Complete (0 edges, all files marked edged) from older over-forgiveness.
        if gg.heal_false_edge_complete() {
            vprintln!(
                "🔄 Edge inventory reopened for {} — will resuscitate background edge build",
                root
            );
        }
        // Honest tolerance only: path twins / missing / empty — never "exists ⇒ edged".
        // Empty `attempted` must not become a path to false Complete.
        if !gg.is_edge_build_complete() {
            let closed =
                gg.reconcile_edge_inventory_tolerance(Some(Path::new(root)), &[]);
            if closed > 0 {
                vprintln!(
                    "🧭 Edge inventory tolerance closed {} unedgeable slot(s) for {}",
                    closed, root
                );
            }
            // Only if *everything* left was truly unedgeable (missing/empty/twin).
            if gg.is_edge_build_complete() {
                vprintln!(
                    "🧭 Edge inventory fully mapped (unedgeable residuals only) for {}",
                    root
                );
                gg.mark_fully_complete(Some(&telemetry));
                sync_telemetry_if_graph_ready(state, root, &gg);
                return;
            }
        }
        gg.sync_bg_telemetry(Some(&telemetry));
        if gg.is_edge_build_complete() {
            sync_telemetry_if_graph_ready(state, root, &gg);
            return;
        }
    }

    // Single-flight **per root** for spawn. Peers are not cancelled (cancel-thrash was the
    // warehouse black hole). Heavy bodies serialize on `EDGE_BUILD_GLOBAL_SLOT` in code_graph
    // builder (queue, mem-aware batches) so dual warm roots don't dual-thrash RAM.
    if server_running {
        return;
    }

    let should_spawn = match graph_rw.try_read() {
        Ok(gg) => gg.needs_background_edge_resuscitation(),
        Err(_) => {
            telemetry.state() != code_graph::snooper::BackgroundEdgeBuildState::Complete
                && !(telemetry.percent() >= 100
                    && !telemetry.thread_active.load(Ordering::Relaxed)
                    && telemetry.state()
                        == code_graph::snooper::BackgroundEdgeBuildState::Complete)
        }
    };

    if !should_spawn {
        return;
    }

    let mut cancels = state.edge_build_cancels.blocking_write();
    // Re-check under cancel map lock: another request may have just spawned us.
    if telemetry.thread_active.load(Ordering::Relaxed) {
        return;
    }
    // Drop stale cancel flag for this root only (finished worker); never touch peers.
    cancels.remove(root);
    let cancel_tok = Arc::new(std::sync::atomic::AtomicBool::new(false));
    cancels.insert(root.to_string(), Arc::clone(&cancel_tok));

    // O(files) from inventory — never O(nodes) PathBuf HashSet (gecko 4.8M hang).
    let files_total = match graph_rw.try_read() {
        Ok(gg) => {
            if !gg.file_hashes.is_empty() {
                gg.file_hashes.len()
            } else {
                // Mid-scan fallback only; FullEdge overwrites from real inventory.
                gg.edgeable_file_inventory().len()
            }
        }
        Err(_) => telemetry.files_total.load(Ordering::Relaxed),
    };
    if telemetry.files_total.load(Ordering::Relaxed) == 0 {
        telemetry
            .files_total
            .store(files_total.max(1), Ordering::Relaxed);
    }
    if matches!(
        telemetry.state(),
        code_graph::snooper::BackgroundEdgeBuildState::NotStarted
            | code_graph::snooper::BackgroundEdgeBuildState::Cancelled
            | code_graph::snooper::BackgroundEdgeBuildState::Error
            | code_graph::snooper::BackgroundEdgeBuildState::Incomplete
    ) {
        // Incomplete resume: keep files_processed (honest progress); only reset fresh starts.
        if !matches!(
            telemetry.state(),
            code_graph::snooper::BackgroundEdgeBuildState::Incomplete
        ) {
            telemetry.files_processed.store(0, Ordering::Relaxed);
        }
    }

    let g_clone = Arc::clone(graph_rw);
    let skips_clone = state.settings.analysis.skip_directories.clone();
    let root_clone = root.to_string();
    let telemetry_spawn = Arc::clone(&telemetry);
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let edge_threads =
        (((cpus as f64) * state.settings.analysis.edge_build_thread_pct).round() as usize).max(1);

    // WarehousePolice owns the mutation lane (EvePolice lesson). FullEdge is exclusive;
    // surgical JIT can run between edge batches without a second spawn_blocking war.
    let resumes = telemetry_spawn
        .resume_count
        .fetch_add(1, Ordering::Relaxed);
    telemetry_spawn.mark_job_started();
    if resumes > 0 {
        vprintln!(
            "🔁 FullEdge resume #{} for {} (incomplete → police queue)",
            resumes, root
        );
    }
    code_graph::snooper::warehouse_police().submit_full_edge(
        g_clone,
        cancel_tok,
        PathBuf::from(root_clone),
        skips_clone,
        Some(telemetry_spawn),
        edge_threads,
    );
    vprintln!(
        "🚀 Background full edge build enqueued on WarehousePolice for {} (mem-aware batches, cancellable)",
        root
    );
}

/// Idle reaper: when util is free, finish Incomplete edge jobs instead of thumb-twiddling.
///
/// Scans **graphs in RAM** (not only telemetry keys) so boot-warm Incomplete roots get
/// FullEdge without waiting for a /context poke.
pub fn warehouse_idle_reaper_tick(state: &AppState) {
    use code_graph::snooper::BackgroundEdgeBuildState;
    use std::sync::atomic::Ordering;

    let mut roots: Vec<String> = {
        let Ok(gmap) = state.graphs.try_read() else {
            return;
        };
        gmap.keys().cloned().collect()
    };
    if let Ok(map) = state.edge_build_status.try_read() {
        for k in map.keys() {
            if !roots.iter().any(|r| r == k) {
                roots.push(k.clone());
            }
        }
    }

    for root in roots {
        let graph_rw = {
            let Ok(gmap) = state.graphs.try_read() else {
                continue;
            };
            match gmap.get(&root) {
                Some(g) => Arc::clone(g),
                None => continue,
            }
        };

        // Skip if inventory already complete under try_read.
        let needs = match graph_rw.try_read() {
            Ok(gg) => {
                if gg.is_edge_build_complete() {
                    continue;
                }
                gg.needs_background_edge_resuscitation()
            }
            Err(_) => true, // busy — still may need work; ensure_ will gate
        };

        let telemetry = get_or_create_telemetry(state, &root, 0);
        if telemetry.state() == BackgroundEdgeBuildState::Complete && !needs {
            continue;
        }
        // Clear silent zombies so Incomplete can resume.
        let _ = telemetry.clear_if_heartbeat_stale(90);
        if telemetry.thread_active.load(Ordering::Relaxed) {
            // Still claiming the lane — honest blocked/phase on /mcp/health.
            continue;
        }
        if !needs
            && !matches!(
                telemetry.state(),
                BackgroundEdgeBuildState::Incomplete
                    | BackgroundEdgeBuildState::Error
                    | BackgroundEdgeBuildState::Cancelled
                    | BackgroundEdgeBuildState::NotStarted
            )
        {
            continue;
        }

        vprintln!(
            "🧹 Warehouse idle reaper: resume FullEdge for {} (state={:?} pct={} phase={} blocked={:?})",
            root,
            telemetry.state(),
            telemetry.percent(),
            telemetry.phase_str(),
            telemetry.blocked_str()
        );
        ensure_background_edge_build(state, &root, &graph_rw);
    }
}

