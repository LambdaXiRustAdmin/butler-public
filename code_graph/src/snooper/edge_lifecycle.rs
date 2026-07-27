//! Edge inventory / Complete stamp / background build lifecycle (P2b peel from model.rs).
//!
//! Warehouse lifecycle truth — not CALL resolution. Zero intentional behavior change.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::model::{BackgroundEdgeBuildState, BgBuildProgress, CodeGraph};
use super::normalize_path;

/// Stable key for edge inventory membership (slash dialect, no leading `./`).
fn edge_path_key(path: &Path) -> String {
    let s = normalize_path(&path.to_string_lossy());
    s.trim_start_matches("./").to_string()
}


impl CodeGraph {
    /// Extensions that participate in call/usage edge building.
    pub fn is_edge_buildable_path(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                matches!(
                    ext,
                    "rs" | "py"
                        | "go"
                        | "c"
                        | "h"
                        | "cpp"
                        | "hpp"
                        | "cc"
                        | "cxx"
                        | "ts"
                        | "tsx"
                        | "js"
                        | "jsx"
                        | "svelte"
                )
            })
    }

    /// Edgeable source files the edge builder walks (progress / tolerance inventory).
    ///
    /// **Prefer `file_hashes` (O(files))** — gecko ~32k vs 4.8M nodes. The old
    /// nodes-first walk under the warehouse write lock hung FullEdge for minutes
    /// (`sync_bg_telemetry` right after mark_running) and starved Trace `try_read`.
    /// Nodes fallback only when inventory is empty (mid-scan / legacy).
    /// Stamped [`is_edge_build_complete`] still returns O(1) without rewalk.
    pub fn edgeable_file_inventory(&self) -> HashSet<std::path::PathBuf> {
        if !self.file_hashes.is_empty() {
            let mut files = HashSet::with_capacity(self.file_hashes.len());
            for pstr in self.file_hashes.keys() {
                let p = PathBuf::from(normalize_path(pstr));
                if Self::is_edge_buildable_path(&p) {
                    files.insert(p);
                }
            }
            return files;
        }
        // Fallback: mid-scan / graphs without inventory.bin.
        self.nodes
            .values()
            .map(|b| b.file.clone())
            .filter(|p| Self::is_edge_buildable_path(p))
            .collect()
    }

    /// Path-normalized membership (repo-relative `/` dialect; strips `./`).
    ///
    /// O(1) HashSet lookups for common path dialects — **never** linear-scan
    /// `files_with_edges` (that was O(inventory) per probe on monster graphs).
    pub fn file_has_edges(&self, path: &Path) -> bool {
        if self.files_with_edges.contains(path) {
            return true;
        }
        let key = edge_path_key(path);
        if key.is_empty() {
            return false;
        }
        let as_key = PathBuf::from(&key);
        if self.files_with_edges.contains(&as_key) {
            return true;
        }
        // Dialect twin: `./src/a.rs` vs `src/a.rs`
        if !key.starts_with("./") {
            let dotted = PathBuf::from(format!("./{key}"));
            if self.files_with_edges.contains(&dotted) {
                return true;
            }
        } else if let Some(stripped) = key.strip_prefix("./") {
            let bare = PathBuf::from(stripped);
            if self.files_with_edges.contains(&bare) {
                return true;
            }
        }
        false
    }

    /// Drop edge-done mark for a path (all path dialects). Used when re-JITing after empty warehouse.
    pub fn clear_file_edge_mark(&mut self, path: &Path) {
        let want = edge_path_key(path);
        self.files_with_edges
            .retain(|f| edge_path_key(f) != want);
    }

    /// `(edged inventory files, inventory size)` — path-tolerant.
    pub fn edge_inventory_progress(&self) -> (usize, usize) {
        // O(1) after FullEdge closed every edgeable slot (see `edge_inventory_closed`).
        if self.edge_inventory_closed {
            let n = self.files_with_edges.len().max(1);
            return (n, n);
        }
        let inv = self.edgeable_file_inventory();
        let total = inv.len();
        if total == 0 {
            return (0, 0);
        }
        // Build O(1) key set once — do not call file_has_edges in a loop that
        // re-walks files_with_edges (was O(files²) during grind).
        let edged: HashSet<String> = self
            .files_with_edges
            .iter()
            .map(|p| edge_path_key(p))
            .filter(|s| !s.is_empty())
            .collect();
        let done = inv
            .iter()
            .filter(|f| {
                let k = edge_path_key(f);
                !k.is_empty() && edged.contains(&k)
            })
            .count();
        (done, total)
    }

    /// Mark finite edge inventory fully closed (FullEdge stream finished all files).
    #[inline]
    pub fn mark_edge_inventory_closed(&mut self) {
        self.edge_inventory_closed = true;
    }

    /// Inventory files still open (not yet closed as edged / forgiven).
    pub fn open_edge_inventory_files(&self) -> Vec<PathBuf> {
        if self.edge_inventory_closed {
            return Vec::new();
        }
        let edged: HashSet<String> = self
            .files_with_edges
            .iter()
            .map(|p| edge_path_key(p))
            .filter(|s| !s.is_empty())
            .collect();
        self.edgeable_file_inventory()
            .into_iter()
            .filter(|f| {
                let k = edge_path_key(f);
                k.is_empty() || !edged.contains(&k)
            })
            .collect()
    }

    /// True when every known edgeable file has been edge-built (finite inventory map).
    /// Does **not** mean PostPass/LTO finished — use [`is_edge_build_complete`] for that.
    pub fn is_edge_inventory_complete(&self) -> bool {
        if self.edge_inventory_closed {
            return true;
        }
        let (done, total) = self.edge_inventory_progress();
        if total == 0 {
            return self.background_edge_build_complete || self.nodes.is_empty();
        }
        done >= total
    }

    /// True when the warehouse is fully ready: edge inventory mapped **and** deferred
    /// PostPass (LTO) finished (or never running). Inventory-only 100% must not lie
    /// while `butler-warehous` still maps FFI.
    pub fn is_edge_build_complete(&self) -> bool {
        // ── Fast path (monster /context lobby) ──────────────────────────────
        // Stamped Complete after FullEdge+LTO: trust the stamp. Re-walking 1M
        // nodes + O(files²) inventory on every request was ~5s of pure disrespect
        // even on Query-cache / Trace-memo HIT (pytorch multi-hit band).
        // Mutations / heal_false_edge_complete clear the stamp.
        if self.background_edge_build_complete
            && !self.background_edge_build_active
            && self.background_edge_build_state == BackgroundEdgeBuildState::Complete
        {
            return true;
        }

        if !self.is_edge_inventory_complete() {
            return false;
        }
        // LTO / PostPass in flight: inventory may be 100% but not Complete.
        if self.background_edge_build_active
            && self.background_edge_build_state == BackgroundEdgeBuildState::Running
        {
            return false;
        }
        // Explicit stamp after LTO, or legacy/cache paths that never set active.
        self.background_edge_build_complete
            || matches!(
                self.background_edge_build_state,
                BackgroundEdgeBuildState::Complete
                    | BackgroundEdgeBuildState::NotStarted
                    | BackgroundEdgeBuildState::Incomplete
            )
    }

    /// Error tolerance: close inventory slots that will **never** yield useful edges.
    ///
    /// After a real edge pass, call with `attempted` so mislabels / missing files / path
    /// dialect twins / empty sources don't leave the map stuck at N−1 forever.
    ///
    /// **Does not** mark existing source files as edged just because they exist on disk.
    /// That was a boolean landmine: defer edges → tolerance with empty `attempted` →
    /// every residual "forgiven" → false Complete → serve-gate skip JIT → 0 callers.
    ///
    /// Returns how many open slots were forgiven/closed.
    pub fn reconcile_edge_inventory_tolerance(
        &mut self,
        project_root: Option<&Path>,
        attempted: &[PathBuf],
    ) -> usize {
        // Always credit files we just attempted (success or hard fail).
        for p in attempted {
            self.files_with_edges.insert(p.clone());
            if let Some(root) = project_root {
                let pp = crate::snooper::project_paths::ProjectPaths::new(root);
                let rel = pp.to_rel(p);
                if !rel.as_os_str().is_empty() {
                    self.files_with_edges.insert(rel);
                }
            }
        }

        let open = self.open_edge_inventory_files();
        if open.is_empty() {
            return 0;
        }

        let pp = project_root.map(crate::snooper::project_paths::ProjectPaths::new);
        let edged_norm: HashSet<String> = self
            .files_with_edges
            .iter()
            .map(|p| {
                if let Some(ref paths) = pp {
                    edge_path_key(&paths.to_rel(p))
                } else {
                    edge_path_key(p)
                }
            })
            .filter(|s| !s.is_empty())
            .collect();

        let mut closed = 0usize;
        let mut reasons: Vec<String> = Vec::new();
        for f in open {
            let rel_norm = if let Some(ref paths) = pp {
                edge_path_key(&paths.to_rel(&f))
            } else {
                edge_path_key(&f)
            };

            // Twin already edged under another path form.
            if !rel_norm.is_empty() && edged_norm.contains(&rel_norm) {
                self.files_with_edges.insert(f.clone());
                closed += 1;
                if reasons.len() < 5 {
                    reasons.push(format!("{} (path twin)", f.display()));
                }
                continue;
            }

            let abs = if let Some(ref paths) = pp {
                paths.to_abs(&f)
            } else {
                f.clone()
            };

            // Only unedgeable residuals — never "file exists ⇒ edged".
            let forgive = match std::fs::metadata(&abs) {
                Err(_) => true,                            // missing / unreadable
                Ok(m) if !m.is_file() => true,             // not a regular file
                Ok(m) if m.len() == 0 => true,             // empty source
                Ok(_) => false,                            // real source still needs an edge pass
            };
            if forgive {
                self.files_with_edges.insert(f.clone());
                closed += 1;
                if reasons.len() < 5 {
                    reasons.push(format!("{} (forgiven)", f.display()));
                }
            }
        }
        if closed > 0 {
            println!(
                "🧭 Edge inventory tolerance: closed {} residual slot(s){}",
                closed,
                if reasons.is_empty() {
                    String::new()
                } else {
                    format!(" e.g. {}", reasons.join(", "))
                }
            );
        }
        closed
    }

    /// Heal false Complete: entire inventory marked edged but warehouse has **zero** CALL edges.
    ///
    /// Happens after deferred scan + over-forgiveness. Does not fire on tiny legit graphs
    /// (single file, no calls) — only when many files claim edged with empty edge map.
    pub fn heal_false_edge_complete(&mut self) -> bool {
        // Cheap first: real edges mean this is not a false Complete. Avoids O(nodes)
        // inventory rewalk on the post-LTO stamp path (torch 1.2M hang).
        if self.total_edges() > 0 {
            return false;
        }
        let (done, total) = self.edge_inventory_progress();
        // total > 8: avoid reopening a one-file stub that truly produced 0 CALL edges.
        if total <= 8 || done < total {
            return false;
        }
        println!(
            "🔄 Healing false edge Complete: {}/{} files marked edged but 0 warehouse edges — reopening inventory",
            done, total
        );
        self.files_with_edges.clear();
        self.edge_inventory_closed = false;
        self.background_edge_build_complete = false;
        self.background_edge_build_state = BackgroundEdgeBuildState::Incomplete;
        self.background_edge_build_active = false;
        self.invalidate_trace_epoch();
        true
    }

    /// True when a background full edge build should be (re)started.
    /// Keeps going until the finite edge inventory is fully mapped — not just "some edges exist".
    pub fn needs_background_edge_resuscitation(&self) -> bool {
        if self.is_edge_build_complete() {
            return false;
        }
        if self.background_edge_build_active
            || self.background_edge_build_state == BackgroundEdgeBuildState::Running
        {
            return false;
        }
        true
    }

    /// After load from disk: restore Complete stamp / inventory so we do **not** false-resuscitate FullEdge.
    ///
    /// **Amnesia bug:** `files_with_edges` + complete stamp used to be `serde(skip)`. Edges loaded
    /// but inventory looked open → every boot re-ran FullEdge on a clean tree.
    ///
    /// `edge_sem_ok`: current EDGE_SEMANTICS matches the cache (caller already dropped edges if not).
    pub fn restore_edge_build_state_after_load(&mut self, edge_sem_ok: bool) {
        // Never resume a dead "Running" worker from disk.
        self.background_edge_build_active = false;
        if self.background_edge_build_state == BackgroundEdgeBuildState::Running {
            self.background_edge_build_state = BackgroundEdgeBuildState::Incomplete;
            self.background_edge_build_complete = false;
        }

        if !edge_sem_ok {
            return;
        }

        let _ = self.heal_false_edge_complete();

        // Trusted persisted Complete (new caches).
        if self.background_edge_build_complete
            && !self.edges.is_empty()
            && self.background_edge_build_state == BackgroundEdgeBuildState::Complete
        {
            // O(1) inventory closed — never rewalk nodes to re-derive file set on load.
            self.mark_edge_inventory_closed();
            let files_done = self.files_with_edges.len().max(1);
            self.stamp_complete_fast(None, files_done, "cache");
            println!(
                "📂 Edge Complete restored from cache ({} edges, files_with_edges={})",
                self.total_edges(),
                files_done
            );
            return;
        }

        // Reconstruct inventory from endpoints when set was empty (old cache / skip serde).
        if self.files_with_edges.is_empty() && !self.edges.is_empty() {
            self.reconstruct_files_with_edges_from_adjacency();
        }

        // Legacy / amnesia: edges on disk but stamp missing (serde skip history).
        // Close residual open slots (0-CALL edged files never re-listed) and stamp Complete.
        // Prefer this over FullEdge thrash on every boot of a finished warehouse.
        if !self.edges.is_empty() {
            let (done, total) = self.edge_inventory_progress();
            if total > 0 && done < total {
                let open = self.open_edge_inventory_files();
                let n = open.len();
                for f in open {
                    self.files_with_edges.insert(f);
                }
                if n > 0 {
                    println!(
                        "📂 Edge inventory amnesia heal: closed {} residual file slot(s) (edges present, stamp missing)",
                        n
                    );
                }
            }
            // After closing residuals, inventory should be full; stamp Complete.
            if self.is_edge_inventory_complete() {
                self.mark_edge_inventory_closed();
                let files_done = self.files_with_edges.len().max(1);
                self.stamp_complete_fast(None, files_done, "load-heal");
                println!(
                    "📂 Edge Complete after load amnesia heal ({} edges, files={})",
                    self.total_edges(),
                    files_done
                );
                return;
            }
            // Still open after heal — genuine partial grind snapshot.
            self.background_edge_build_complete = false;
            self.background_edge_build_state = BackgroundEdgeBuildState::Incomplete;
            let (done, total) = self.edge_inventory_progress();
            println!(
                "📂 Loaded partial edge map ({} blocks, edge files {}/{}) — will resuscitate background build",
                self.nodes.len(),
                done,
                total
            );
            return;
        }

        // No edges on disk.
        self.background_edge_build_state = BackgroundEdgeBuildState::Incomplete;
        if !self.files_with_edges.is_empty() {
            self.background_edge_build_complete = false;
            let (done, total) = self.edge_inventory_progress();
            println!(
                "📂 Loaded edge inventory without edges ({}/{}) — will resuscitate background build",
                done, total
            );
        }
    }

    /// Mark files that appear as edge endpoints as edged (load-time approximate inventory).
    pub fn reconstruct_files_with_edges_from_adjacency(&mut self) {
        for b in self.nodes.values() {
            if self.edges.contains_key(&b.id) || self.reverse.contains_key(&b.id) {
                self.files_with_edges.insert(b.file.clone());
            }
        }
    }

    /// Mark background build thread as running (server spawns).
    pub fn mark_background_edge_build_running(&mut self) {
        self.background_edge_build_active = true;
        self.background_edge_build_state = BackgroundEdgeBuildState::Running;
    }

    /// Mark background build cancelled (workspace switch).
    pub fn mark_background_edge_build_cancelled(&mut self) {
        self.background_edge_build_active = false;
        if !self.background_edge_build_complete {
            self.background_edge_build_state = BackgroundEdgeBuildState::Cancelled;
        }
    }

    /// Mark background build failed (panic / abnormal thread exit).
    pub fn mark_background_edge_build_failed(&mut self) {
        self.background_edge_build_active = false;
        if !self.background_edge_build_complete {
            self.background_edge_build_state = BackgroundEdgeBuildState::Error;
        }
    }

    /// Mark background build finished successfully.
    pub fn mark_background_edge_build_complete(&mut self) {
        self.background_edge_build_active = false;
        self.background_edge_build_complete = true;
        self.background_edge_build_state = BackgroundEdgeBuildState::Complete;
        self.invalidate_trace_epoch();
    }

    /// Skeleton/warehouse loaded — Arch / orientation may serve without full edges.
    ///
    /// **Not** "all edges mapped". Partial JIT edges must not flip this into edge-complete.
    pub fn is_ready_to_serve(&self) -> bool {
        !self.nodes.is_empty()
            || self.background_edge_build_complete
            || self.background_edge_build_state == BackgroundEdgeBuildState::Complete
    }

    /// Final post-build transition: **only** when inventory is fully mapped.
    /// If still open, stamps Incomplete + progress so bg can resuscitate.
    ///
    /// Prefer [`mark_fully_complete_after_lto`] after FullEdge+PostPass — that path is O(1)
    /// and must not rewalk 1M+ nodes under the write lock.
    pub fn mark_fully_complete(&mut self, telemetry: Option<&BgBuildProgress>) {
        let _ = self.heal_false_edge_complete();
        let (done, total) = self.edge_inventory_progress();
        if total > 0 && done < total {
            self.background_edge_build_active = false;
            self.background_edge_build_complete = false;
            self.background_edge_build_state = BackgroundEdgeBuildState::Incomplete;
            if let Some(t) = telemetry {
                use std::sync::atomic::Ordering;
                t.files_total.store(total, Ordering::Relaxed);
                t.files_processed.store(done, Ordering::Relaxed);
                t.set_state(BackgroundEdgeBuildState::Incomplete);
                t.thread_active.store(false, Ordering::Relaxed);
            }
            println!(
                "📂 Edge map partial — {} blocks, {} edges (edge files {}/{}) — will keep mapping",
                self.nodes.len(),
                self.total_edges(),
                done,
                total
            );
            self.invalidate_trace_epoch();
            return;
        }
        let files_done = done.max(total).max(1);
        self.stamp_complete_fast(telemetry, files_done, "inventory");
    }

    /// O(1) Complete after FullEdge closed inventory + PostPass LTO + adjacency squish.
    ///
    /// **Does not** rewalk `edgeable_file_inventory` (O(nodes) PathBuf clones) or run a full
    /// `audit_name_index` (O(nodes) string Id compares). Those hung torch at 99% for minutes
    /// after a successful squish. Builder invariant: inventory was closed during stream collect.
    pub fn mark_fully_complete_after_lto(
        &mut self,
        telemetry: Option<&BgBuildProgress>,
        edge_files_done: usize,
    ) {
        let t0 = std::time::Instant::now();
        // Zero-edge warehouse with many "edged" files → false complete (rare after LTO).
        if self.total_edges() == 0 && self.files_with_edges.len() > 8 {
            let _ = self.heal_false_edge_complete();
            if !self.background_edge_build_complete && self.total_edges() == 0 {
                // heal reopened inventory — fall back to slow path only in this edge case
                self.mark_fully_complete(telemetry);
                return;
            }
        }
        let files_done = edge_files_done
            .max(self.files_with_edges.len())
            .max(1);
        self.stamp_complete_fast(telemetry, files_done, "LTO");
        println!(
            "⚡ Complete stamp (LTO, no inventory rewalk) in {:.2?}",
            t0.elapsed()
        );
    }

    /// Shared Complete stamp: ensure name index stamp, telemetry, log. No inventory rewalk.
    fn stamp_complete_fast(
        &mut self,
        telemetry: Option<&BgBuildProgress>,
        files_done: usize,
        kind: &str,
    ) {
        let was_complete = self.background_edge_build_complete;
        self.mark_background_edge_build_complete();
        // O(1) if stamp matches; rebuild only when stale (not full content audit).
        self.ensure_name_index();
        // Full audit is O(nodes)×string Id — log stamp only (audit on demand / tests).
        if self.name_index_is_stale() {
            self.rebuild_name_index();
        }
        println!(
            "📇 name_index stamp OK (Complete/{kind}): nodes={} keys={} stamp={}",
            self.nodes.len(),
            self.name_index.len(),
            self.name_index_nodes_len
        );
        if let Some(t) = telemetry {
            t.mark_fully_complete(files_done);
        }
        if !was_complete {
            let n_edges = self.total_edges();
            println!(
                "✅ Graph fully ready — {} blocks, {} edges (edge files {}, stamp={kind})",
                self.nodes.len(),
                n_edges,
                files_done
            );
        }
    }

    /// Recover zombie `Running` state when server knows no thread is alive.
    pub fn reconcile_stale_running_state(&mut self, server_thread_alive: bool) {
        if self.background_edge_build_state == BackgroundEdgeBuildState::Running
            && !server_thread_alive
            && !self.background_edge_build_complete
        {
            self.background_edge_build_active = false;
            self.background_edge_build_state = if self.edges.is_empty() {
                BackgroundEdgeBuildState::Cancelled
            } else {
                BackgroundEdgeBuildState::Incomplete
            };
        }
    }

    /// True only when clients should see live "Building Graph (X%)" progress.
    pub fn is_background_edge_build_in_progress(&self) -> bool {
        self.background_edge_build_active
            && self.background_edge_build_state == BackgroundEdgeBuildState::Running
            && !self.background_edge_build_complete
    }

    /// Mirror graph bg state into lock-free telemetry for HTTP fast-fail.
    /// Only stamps Complete when the finite edge inventory is fully mapped.
    pub fn sync_bg_telemetry(&self, telemetry: Option<&BgBuildProgress>) {
        let Some(t) = telemetry else { return };
        use std::sync::atomic::Ordering;
        if self.is_edge_build_complete() {
            let (done, total) = self.edge_inventory_progress();
            t.mark_fully_complete(done.max(total).max(1));
            return;
        }
        let (done, total) = self.edge_inventory_progress();
        if total > 0 {
            t.files_total.store(total, Ordering::Relaxed);
            t.files_processed.store(done, Ordering::Relaxed);
        }
        t.set_state(self.background_edge_build_state);
        t.thread_active
            .store(self.background_edge_build_active, Ordering::Relaxed);
    }

}

#[cfg(test)]
mod edge_inventory_tests {
    use super::*;
    use crate::snooper::model::{BlockInfo, CodeGraph, Id};
    use std::path::PathBuf;

    fn insert_node(g: &mut CodeGraph, file: &str, name: &str) {
        let hash = format!("{name:0<16}");
        let b = BlockInfo {
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
            external_crates: HashSet::new(),
            is_highly_connected: false,
        };
        g.nodes.insert(b.id.clone(), b);
        g.file_hashes.insert(file.into(), 1);
    }

    #[test]
    fn partial_edges_are_not_complete() {
        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        insert_node(&mut g, "src/b.rs", "bb");
        g.files_with_edges.insert(PathBuf::from("src/a.rs"));
        g.edges.insert(
            Id::new("src/a.rs", "function_item", "aaaaaaaa"),
            vec![],
        );
        assert!(!g.is_edge_build_complete());
        assert!(g.needs_background_edge_resuscitation());
        g.files_with_edges.insert(PathBuf::from("src/b.rs"));
        assert!(g.is_edge_build_complete());
        assert!(!g.needs_background_edge_resuscitation());
    }

    #[test]
    fn trace_epoch_caches_and_invalidates_on_edge_batch() {
        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        g.file_hashes.insert("src/a.rs".into(), 1);
        let e1 = g.current_trace_epoch();
        let e1b = g.current_trace_epoch();
        assert_eq!(e1, e1b, "warm epoch must be O(1) stable");
        let from = Id::new("src/a.rs", "function_item", "aaaaaaaa");
        let to = Id::new("src/a.rs", "function_item", "bbbbbbbb");
        g.add_edges_batch_vec(vec![(from, to)]);
        let e2 = g.current_trace_epoch();
        assert_ne!(e1, e2, "edge batch must bump epoch");
    }

    #[test]
    fn restore_after_load_heals_amnesia_without_stamp() {
        // Simulate old cache: edges present, files_with_edges empty, complete=false.
        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        insert_node(&mut g, "src/b.rs", "bb");
        let a = Id::new("src/a.rs", "function_item", "aaaaaaaa");
        let b = Id::new("src/b.rs", "function_item", "bbbbbbbb");
        g.add_edge(a, b);
        assert!(g.files_with_edges.is_empty());
        assert!(!g.background_edge_build_complete);
        g.restore_edge_build_state_after_load(true);
        assert!(
            g.is_edge_build_complete(),
            "must not false-resuscitate when edges already on disk"
        );
        assert!(!g.needs_background_edge_resuscitation());
        assert!(g.files_with_edges.len() >= 2);
    }

    #[test]
    fn restore_after_load_skips_heal_when_edge_sem_stale() {
        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        insert_node(&mut g, "src/b.rs", "bb");
        let a = Id::new("src/a.rs", "function_item", "aaaaaaaa");
        let b = Id::new("src/b.rs", "function_item", "bbbbbbbb");
        g.add_edge(a, b);
        // Caller would clear edges on sem mismatch; we only check the flag path.
        g.edges.clear();
        g.reverse.clear();
        g.restore_edge_build_state_after_load(false);
        assert!(!g.is_edge_build_complete() || g.edges.is_empty());
        // With no edges and edge_sem_ok=false, do not stamp complete.
        assert!(!g.background_edge_build_complete);
    }

    #[test]
    fn typed_bridge_not_on_call_adjacency() {
        use crate::snooper::interconnect::BridgeKind;
        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        insert_node(&mut g, "src/b.py", "bb");
        let a = Id::new("src/a.rs", "function_item", "aaaaaaaa");
        let b = Id::new("src/b.py", "function_definition", "bbbbbbbb");
        g.add_bridge_edge(b.clone(), a.clone(), BridgeKind::Export);
        assert!(g.children(&b).is_empty(), "bridge must not be CALL");
        assert!(g.callers(&a).is_empty(), "bridge must not reverse CALL");
        assert_eq!(
            g.bridge_kind_between(&b, &a),
            Some(BridgeKind::Export)
        );
        assert_eq!(g.total_bridge_edges(), 1);
        g.normalize_adjacency();
        assert_eq!(g.bridge_kind_between(&b, &a), Some(BridgeKind::Export));
    }

    #[test]
    fn stamped_complete_skips_inventory_rewalk() {
        // Even with zero inventory bookkeeping, stamped Complete is trusted.
        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        // Deliberately leave files_with_edges empty — slow path would say incomplete.
        g.mark_background_edge_build_complete();
        assert!(
            g.is_edge_build_complete(),
            "stamped Complete must be O(1) true without inventory rewalk"
        );
    }

    #[test]
    fn file_has_edges_dialect_twin_without_scan() {
        let mut g = CodeGraph::new();
        g.files_with_edges.insert(PathBuf::from("./src/a.rs"));
        assert!(g.file_has_edges(Path::new("src/a.rs")));
        assert!(g.file_has_edges(Path::new("./src/a.rs")));
        assert!(!g.file_has_edges(Path::new("src/b.rs")));
    }

    #[test]
    fn mark_fully_complete_refuses_partial_inventory() {
        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        insert_node(&mut g, "src/b.rs", "bb");
        g.files_with_edges.insert(PathBuf::from("src/a.rs"));
        g.mark_fully_complete(None);
        assert!(!g.background_edge_build_complete);
        assert_eq!(
            g.background_edge_build_state,
            BackgroundEdgeBuildState::Incomplete
        );
        g.files_with_edges.insert(PathBuf::from("src/b.rs"));
        g.mark_fully_complete(None);
        assert!(g.background_edge_build_complete);
    }

    #[test]
    fn mark_fully_complete_after_lto_is_o1_without_inventory_rewalk() {
        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        insert_node(&mut g, "src/b.rs", "bb");
        // Simulate stream closed without re-deriving inventory from nodes.
        g.files_with_edges.insert(PathBuf::from("src/a.rs"));
        g.files_with_edges.insert(PathBuf::from("src/b.rs"));
        g.mark_edge_inventory_closed();
        g.edges
            .entry(g.nodes.keys().next().unwrap().clone())
            .or_default()
            .push(g.nodes.keys().nth(1).unwrap().clone());
        g.rebuild_name_index();
        g.mark_fully_complete_after_lto(None, 2);
        assert!(g.background_edge_build_complete);
        assert!(g.is_edge_build_complete());
        assert!(g.edge_inventory_closed);
        assert_eq!(g.edge_inventory_progress(), (2, 2));
    }

    #[test]
    fn orphan_file_hash_does_not_block_complete() {
        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        // Inventory is driven by file_hashes when non-empty; only a.rs is edged.
        g.file_hashes.insert("src/a.rs".into(), 1);
        g.file_hashes.insert("src/orphan.rs".into(), 99); // no nodes — still edgeable inventory
        g.files_with_edges.insert(PathBuf::from("src/a.rs"));
        // Orphan keeps inventory open until forgiven/edged (not false Complete).
        assert!(!g.is_edge_build_complete());
        // Node-only inventory (no orphan hash): complete when that file is edged.
        g.file_hashes.remove("src/orphan.rs");
        assert!(g.is_edge_build_complete());
    }

    #[test]
    fn tolerance_closes_path_twin_and_missing() {
        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        insert_node(&mut g, "src/ghost.rs", "gg");
        // Edged under different path form
        g.files_with_edges.insert(PathBuf::from("./src/a.rs"));
        assert!(!g.is_edge_build_complete());
        let closed = g.reconcile_edge_inventory_tolerance(None, &[]);
        assert!(closed >= 1);
        // ghost has no disk file → forgiven
        assert!(g.is_edge_build_complete(), "progress={:?}", g.edge_inventory_progress());
    }

    #[test]
    fn tolerance_does_not_forgive_existing_source_on_disk() {
        let dir = std::env::temp_dir().join(format!(
            "butler_tol_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let src = dir.join("src/a.rs");
        std::fs::write(&src, "fn a() {}").unwrap();

        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        assert!(!g.is_edge_build_complete());
        // Empty attempted + file exists → must stay open (no landmine).
        let closed = g.reconcile_edge_inventory_tolerance(Some(&dir), &[]);
        assert_eq!(closed, 0, "must not forgive live sources");
        assert!(!g.is_edge_build_complete());
        // After a real attempt, inventory can close.
        let closed2 = g.reconcile_edge_inventory_tolerance(Some(&dir), &[PathBuf::from("src/a.rs")]);
        assert!(closed2 >= 1 || g.is_edge_build_complete());
        assert!(g.is_edge_build_complete());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn heal_false_edge_complete_reopens_mass_lie() {
        let mut g = CodeGraph::new();
        for i in 0..12 {
            insert_node(&mut g, &format!("src/f{i}.rs"), &format!("n{i:02}xx"));
            g.files_with_edges
                .insert(PathBuf::from(format!("src/f{i}.rs")));
        }
        assert!(g.is_edge_build_complete());
        assert_eq!(g.total_edges(), 0);
        assert!(g.heal_false_edge_complete());
        assert!(!g.is_edge_build_complete());
        assert!(g.files_with_edges.is_empty());
    }

    #[test]
    fn heal_skips_tiny_zero_edge_graphs() {
        let mut g = CodeGraph::new();
        insert_node(&mut g, "src/a.rs", "aa");
        g.files_with_edges.insert(PathBuf::from("src/a.rs"));
        assert!(!g.heal_false_edge_complete());
        assert!(g.is_edge_build_complete());
    }
}

