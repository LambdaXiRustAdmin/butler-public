//! Surgical JIT + interconnect session (P3 peel).
//! Zero intentional behavior change.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use code_graph::snooper::normalize_path;
use code_graph::BlockInfo;

use crate::server::dto::*;
use crate::vprintln;

/// Surgical JIT without parking FullEdge for minutes.
///
/// - Prefer **try_write** on the request thread (collect keeps running; only merge contends).
/// - If FullEdge is live and write is busy: **enqueue** for between-batch drain and wait
///   briefly (≤500ms) so first Trace can get symbol-file edges without serializing collect.
/// - If idle: short police wait as fallback (PostPass-safe).
pub(super) fn run_surgical_jit_nonblocking(
    graph_rw: &Arc<std::sync::RwLock<code_graph::CodeGraph>>,
    root: &str,
    skips: &[String],
    files: &[PathBuf],
    full_edge_live: bool,
) -> bool {
    if files.is_empty() {
        return true;
    }
    // Prefer non-blocking write: never sit 120s on the police FullEdge queue.
    if let Ok(mut g) = graph_rw.try_write() {
        let _ = g.heal_false_edge_complete();
        if g.total_edges() == 0 {
            for p in files {
                g.clear_file_edge_mark(p);
            }
        }
        g.ensure_call_graph(Path::new(root), skips, Some(files));
        return true;
    }
    if full_edge_live {
        // Merge holds write — enqueue for next between-batch yield (police drains JIT).
        // Wait up to ~1.2s for between-batch drain (batches are often 3–5s; short
        // enough not to feel hung, long enough for one yield on many hosts).
        vprintln!(
            "⚡ JIT yield-wait (FullEdge merge busy) for {} file(s) under {} — ≤1.2s",
            files.len(),
            root
        );
        let ok = code_graph::snooper::warehouse_police().jit_files_yield_wait(
            Arc::clone(graph_rw),
            PathBuf::from(root),
            skips.to_vec(),
            files.to_vec(),
            std::time::Duration::from_millis(1200),
        );
        if !ok {
            vprintln!(
                "⚡ JIT still pending after yield-wait (will land on next batch) under {}",
                root
            );
        }
        return ok;
    }
    // Idle warehouse: brief police lane (PostPass-safe).
    code_graph::snooper::warehouse_police().jit_files_blocking(
        Arc::clone(graph_rw),
        PathBuf::from(root),
        skips.to_vec(),
        files.to_vec(),
        std::time::Duration::from_secs(15),
    )
}

/// Run Export/Ipc/Twin interconnect **once per warehouse session**.
/// Re-running on every Trace held the write lock on one core for the whole battery.
pub(super) fn ensure_interconnect_session(
    g: &mut code_graph::CodeGraph,
    ipc_rules: &[code_graph::snooper::ipc_engine::IpcRule],
    root: &Path,
) {
    if g.interconnect_session_ready {
        return;
    }
    // Already have bridges from disk PostPass — stamp ready, skip re-map thrash.
    if !g.bridge_fwd.is_empty() {
        g.interconnect_session_ready = true;
        vprintln!(
            "⚡ Phase-4 interconnect skip ({} bridge sources already loaded) under {}",
            g.bridge_fwd.len(),
            root.display()
        );
        return;
    }
    let t0 = std::time::Instant::now();
    code_graph::snooper::interconnect::run_without_decl_def(g, Some(ipc_rules), Some(root));
    g.interconnect_session_ready = true;
    vprintln!(
        "⚡ Phase-4 interconnect inject once under {} in {:.1?} (bridges={})",
        root.display(),
        t0.elapsed(),
        g.bridge_fwd.len()
    );
}

pub(super) fn handle_surgical_mode(req: &ContextRequest, graph: &code_graph::CodeGraph) -> Vec<BlockInfo> {
    let target_file = req.target_file.as_deref().unwrap_or("");
    let target_line = req.target_line.unwrap_or(0);

    let mut surgical_blocks: Vec<BlockInfo> = graph
        .nodes
        .values()
        .filter(|b| {
            let file = normalize_path(&b.file.to_string_lossy());
            let target = normalize_path(target_file);
            file.ends_with(&target) && b.start_line <= target_line && b.end_line >= target_line
        })
        .cloned()
        .collect();

    for b in &mut surgical_blocks {
        b.score += 100.0;
    }
    surgical_blocks
}

