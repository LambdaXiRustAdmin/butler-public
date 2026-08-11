// code_graph/src/snooper/watcher.rs
//! Live FS watcher: single-file dirty re-edge in the background.
//!
//! Product rules (don't trip on shoelaces):
//! - **Serve never waits** on dirty rebuild; rebuild never blocks `/context` birth.
//! - **Settle** absorbs editor event storms (one save → many notify events).
//! - **Busy coalesce** drains new events while a batch is mid-flight.
//! - **Per-file yield** releases the write lock between files so Trace can serve.
//! - **Post-batch cooldown** coalesces rapid save–save–save into the last state.
//! - **Batch cap** keeps huge monorepo storms from monopolizing the write lock.

use crate::snooper::scanner::{get_skip_patterns, should_scan_path_under, save_graph_async};
use crate::CodeGraph;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Quiet window after the last FS event before we start a re-edge batch.
const SETTLE_MS: u64 = 250;
/// Gap after a batch so rapid iteration lands on the last write, not every intermediate.
const COOLDOWN_MS: u64 = 400;
/// Max files re-edged per batch (single-pager-ish). Overflow stays pending for the next cycle.
const MAX_FILES_PER_BATCH: usize = 8;
/// Poll cancel flag while idle waiting for FS events (Hop B warehouse sleep).
const CANCEL_POLL_MS: u64 = 500;

/// Start live FS watcher (no cancel). Prefer [`start_watcher_cancellable`] for sleepable roots.
pub fn start_watcher(
    root: impl AsRef<Path>,
    graph: Arc<RwLock<CodeGraph>>,
    config_skip_directories: Vec<String>,
) {
    start_watcher_cancellable(root, graph, config_skip_directories, None);
}

/// Live FS watcher that exits when `cancel` is set (warehouse sleep / RSS budget).
///
/// Sleep **never** deletes on-disk Complete cache — only stops watching and drops the
/// live thread so the root can leave RAM without orphaned `butler-watcher-*` thrash.
pub fn start_watcher_cancellable(
    root: impl AsRef<Path>,
    graph: Arc<RwLock<CodeGraph>>,
    config_skip_directories: Vec<String>,
    cancel: Option<Arc<AtomicBool>>,
) {
    let root = root.as_ref().to_path_buf();
    std::thread::spawn(move || {
        let cancelled = || {
            cancel
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed))
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher: RecommendedWatcher = notify::recommended_watcher(tx).unwrap();
        watcher
            .configure(Config::default().with_poll_interval(Duration::from_millis(500)))
            .ok();

        let skip_patterns = get_skip_patterns(&root, &config_skip_directories);
        // Never Recursive-watch the whole leviathan root (pytorch: tens of k dirs).
        // Watch top-level children except skip/.butler — avoids multi-min setup + event storms
        // from .butler/cache writes.
        let mut watched = 0usize;
        if let Ok(rd) = std::fs::read_dir(&root) {
            for ent in rd.flatten() {
                let p = ent.path();
                let name = ent.file_name().to_string_lossy().to_string();
                if name == ".butler"
                    || name == ".git"
                    || name.starts_with('.')
                    || skip_patterns.iter().any(|s| {
                        let s = s.trim_matches('/');
                        s == name || name == s.trim_end_matches('/')
                    })
                {
                    continue;
                }
                if p.is_dir() {
                    if watcher.watch(&p, RecursiveMode::Recursive).is_ok() {
                        watched += 1;
                    }
                }
            }
        }
        if watched == 0 {
            // Flat package / empty listing fallback.
            let _ = watcher.watch(&root, RecursiveMode::Recursive);
            watched = 1;
        }

        println!(
            "👀 Watcher started — settle {}ms, cooldown {}ms, batch≤{} (busy coalesce); top-level watches={watched}",
            SETTLE_MS, COOLDOWN_MS, MAX_FILES_PER_BATCH
        );

        let mut pending: HashSet<PathBuf> = HashSet::new();

        loop {
            if cancelled() {
                println!(
                    "👀 Watcher stopped (warehouse sleep/cancel) for {}",
                    root.display()
                );
                return;
            }
            // Idle: wait for change, polling cancel so sleep can detach without orphan threads.
            if pending.is_empty() {
                match rx.recv_timeout(Duration::from_millis(CANCEL_POLL_MS)) {
                    Ok(Ok(event)) => {
                        absorb_event(&event, &root, &skip_patterns, &mut pending);
                    }
                    Ok(Err(_)) => continue,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return, // channel closed
                }
                if pending.is_empty() {
                    continue;
                }
                println!("🔔 [watcher] first change event(s): {:?}", pending);
            }

            // Settle: drain while events keep arriving within SETTLE_MS windows.
            if !settle_drain(&rx, &root, &skip_patterns, &mut pending, SETTLE_MS) {
                return;
            }
            if pending.is_empty() {
                continue;
            }

            // Cap batch; remainder stays dirty for the next cycle.
            let batch: Vec<PathBuf> = pending
                .iter()
                .take(MAX_FILES_PER_BATCH)
                .cloned()
                .collect();
            for p in &batch {
                pending.remove(p);
            }
            let leftover = pending.len();
            println!(
                "🔄 [watcher] Settled batch {} file(s){} — single-file re-edge",
                batch.len(),
                if leftover > 0 {
                    format!(" (+{} pending next)", leftover)
                } else {
                    String::new()
                }
            );

            // Multi-file co-update under one lock: insert all blocks first, then one
            // global name-map + edges (cross-file CALL within the settled batch).
            // Per-file yield still applies when batch is size-1; for multi-file we
            // prefer edge correctness over mid-batch Trace (cap is MAX_FILES_PER_BATCH).
            {
                let mut g = graph
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                g.update_files_batch(&batch, &root, true);
            }
            // Release write lock then drain — Trace can serve; new events coalesce.
            try_drain(&rx, &root, &skip_patterns, &mut pending);

            // Persist off the hot lock (read snap on a helper thread).
            save_graph_async(Arc::clone(&graph), root.clone());
            println!(
                "✅ [watcher] batch complete ({} file(s)); cooldown {}ms",
                batch.len(),
                COOLDOWN_MS
            );

            // Cooldown: absorb rapid iteration into pending; don't start next batch immediately.
            if !settle_drain(&rx, &root, &skip_patterns, &mut pending, COOLDOWN_MS) {
                return;
            }
            if !pending.is_empty() {
                println!(
                    "⏳ [watcher] {} dirty after cooldown — coalescing next batch",
                    pending.len()
                );
            }
        }
    });
}

fn absorb_event(
    event: &notify::Event,
    root: &Path,
    skip_patterns: &[String],
    pending: &mut HashSet<PathBuf>,
) {
    // Modify/Create: reparse. Remove: update_single_file drops blocks when read fails.
    // Ignoring Remove left stale nodes after file delete (incremental truth hole).
    if !matches!(
        event.kind,
        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
    ) {
        return;
    }
    for p in &event.paths {
        let s = p.to_string_lossy();
        // Never re-edge from cache/noise writes (.butler/cache graph.bin storms).
        if s.contains("/.butler/") || s.contains("\\.butler\\") {
            continue;
        }
        if should_scan_path_under(p, skip_patterns, Some(root)) {
            pending.insert(p.clone());
        }
    }
}

/// Drain events for up to `window_ms` of quiet. Extends `pending`.
/// Returns false if the notify channel disconnected (watcher should exit).
fn settle_drain(
    rx: &Receiver<Result<notify::Event, notify::Error>>,
    root: &Path,
    skip_patterns: &[String],
    pending: &mut HashSet<PathBuf>,
    window_ms: u64,
) -> bool {
    let window = Duration::from_millis(window_ms);
    loop {
        match rx.recv_timeout(window) {
            Ok(Ok(event)) => {
                absorb_event(&event, root, skip_patterns, pending);
                // Keep settling — any activity resets the quiet window.
            }
            Ok(Err(_)) => {} // notify send error
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// Non-blocking drain of whatever is already queued (busy coalesce).
fn try_drain(
    rx: &Receiver<Result<notify::Event, notify::Error>>,
    root: &Path,
    skip_patterns: &[String],
    pending: &mut HashSet<PathBuf>,
) {
    loop {
        match rx.try_recv() {
            Ok(Ok(event)) => absorb_event(&event, root, skip_patterns, pending),
            Ok(Err(_)) => {}
            Err(std::sync::mpsc::TryRecvError::Empty) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
    }
}
