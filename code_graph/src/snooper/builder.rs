//! Heavy edge-building orchestration for the Snooper graph (Strangler Fig extraction).
//!
//! Contains:
//! - `CodeGraph::update_single_file` / `update_files_batch` (watcher incremental + cross-file re-edge)
//! - `CodeGraph::ensure_dependency_versions`
//! - `CodeGraph::ensure_call_graph` (on-demand / JIT surgical)
//! - `CodeGraph::add_dependency_edges`
//! - `run_background_full_edge_build` (the cancellable rayon batch coordinator)
//!
//! These were previously mixed into the God module `mod.rs`. The pure data model
//! (Id / BlockInfo / CodeGraph + basic methods) lives in `model.rs`.
//!
//! **B1:** mem tier / batch budget / locality → [`super::edge_mem`].
//! **B2:** CALL name maps → [`super::call_name_maps`].
//! **B3:** edge collect dispatch + path I/O → [`super::edge_collect`].
//! The facade in `mod.rs` re-exports everything so call sites and submodules are unaffected.

use super::collector;
use super::lang;
use super::parser;
use super::scanner;
use super::utils::normalize_path;
use crate::snooper::model::*;
use super::edge_mem::{
    edge_batch_budget, edge_batch_budget_ceiling, edge_mem_tier_bytes, edge_pool_threads,
    get_bounded_edge_pool, mem_budget_bytes, process_rss_bytes, sort_files_for_edge_locality,
    take_edge_batch, GIB, MIB,
};
use super::call_name_maps::call_name_maps_snapshot;
use super::edge_collect::{abs_source_path, collect_edges_for_lang, is_edge_buildable_ext, rel_source_path};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;

use camino::Utf8PathBuf;
#[allow(unused_imports)]
use cargo_metadata::{Metadata, MetadataCommand, Package};
use walkdir::WalkDir;

// Full-edge exclusivity: WarehousePolice job queue (not a bare Mutex). See warehouse_police.rs.

/// Max seconds FullEdge will spin on `try_write` before aborting to Incomplete (reaper retries).
/// Prevents police-lane deadlock when a long reader holds the graph (Trace) forever.
const FULL_EDGE_WRITE_WAIT_SECS: u64 = 90;

fn full_edge_phase(telemetry: &Option<Arc<BgBuildProgress>>, root: &Path, phase: &str) {
    if let Some(t) = telemetry {
        t.set_phase(phase);
        t.beat();
    }
    println!(
        "🚦 FullEdge phase={} root={}",
        phase,
        root.display()
    );
}

/// Acquire graph write without silent infinite block. Beats + logs while waiting; aborts on budget.
fn full_edge_write<'a>(
    graph: &'a std::sync::RwLock<CodeGraph>,
    telemetry: &Option<Arc<BgBuildProgress>>,
    root: &Path,
    phase: &str,
    budget_secs: u64,
) -> Result<std::sync::RwLockWriteGuard<'a, CodeGraph>, ()> {
    let t0 = Instant::now();
    let mut next_log = 0.0_f64;
    loop {
        match graph.try_write() {
            Ok(g) => {
                if let Some(t) = telemetry {
                    t.clear_blocked();
                    t.set_phase(phase);
                    t.beat();
                }
                let waited = t0.elapsed().as_secs_f64();
                if waited >= 0.5 {
                    println!(
                        "✅ FullEdge write acquired after {:.1}s phase={} root={}",
                        waited,
                        phase,
                        root.display()
                    );
                }
                return Ok(g);
            }
            Err(_) => {
                let waited = t0.elapsed().as_secs_f64();
                if waited > budget_secs as f64 {
                    if let Some(t) = telemetry {
                        t.set_blocked(format!(
                            "write_wait_timeout {:.0}s phase={phase}",
                            waited
                        ));
                        t.set_phase(format!("blocked:{phase}"));
                        t.beat();
                    }
                    eprintln!(
                        "🚧 FullEdge ABORT write_wait >{budget_secs}s phase={phase} root={} — Incomplete; idle reaper will retry",
                        root.display()
                    );
                    return Err(());
                }
                if waited >= next_log {
                    if let Some(t) = telemetry {
                        t.set_blocked(format!("write_wait {:.0}s phase={phase}", waited));
                        t.set_phase(format!("write_wait:{phase}"));
                        t.beat();
                    }
                    eprintln!(
                        "🚧 FullEdge BLOCKED write_wait {:.0}s phase={phase} root={}",
                        waited,
                        root.display()
                    );
                    next_log = waited + 5.0;
                } else if let Some(t) = telemetry {
                    // Keep heartbeat fresh so health shows live+blocked, not zombie silence.
                    t.beat();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

fn full_edge_abort_incomplete(
    graph: &std::sync::RwLock<CodeGraph>,
    telemetry: &Option<Arc<BgBuildProgress>>,
    reason: &str,
) {
    if let Some(t) = telemetry {
        t.mark_incomplete_idle(reason);
    }
    // Never block forever on abort (we may already be in write_wait timeout).
    if let Ok(mut g) = graph.try_write() {
        g.background_edge_build_active = false;
        g.background_edge_build_complete = false;
        g.background_edge_build_state = BackgroundEdgeBuildState::Incomplete;
        if let Some(t) = telemetry {
            g.sync_bg_telemetry(Some(t.as_ref()));
        }
    }
}
/// Log files whose edge parse+collect exceeds this (diagnose ATen mega-headers).
const SLOW_EDGE_FILE_SECS: f64 = 5.0;

impl CodeGraph {
    /// Files that currently CALL into any of `ids` (excluding `self_files`).
    /// Captured **before** stripping so reverse-dependent re-edge can restore A→B
    /// after B is reparsed (watcher would otherwise leave A with a dangling hole).
    fn reverse_dependent_files(
        &self,
        ids: &[Id],
        self_files: &std::collections::HashSet<PathBuf>,
    ) -> std::collections::HashSet<PathBuf> {
        let mut out = std::collections::HashSet::new();
        for id in ids {
            if let Some(sources) = self.reverse.get(id) {
                for src in sources {
                    if let Some(b) = self.nodes.get(src) {
                        if !self_files.contains(&b.file) {
                            out.insert(b.file.clone());
                        }
                    }
                }
            }
        }
        out
    }

    /// Evict all blocks for `norm_path` and every edge that touches them (both directions).
    fn evict_file_blocks_and_edges(&mut self, norm_path: &Path) -> Vec<Id> {
        let ids_to_remove: Vec<Id> = self
            .nodes
            .iter()
            .filter(|(_, b)| b.file == norm_path)
            .map(|(id, _)| id.clone())
            .collect();
        self.nodes.retain(|_, block| block.file != norm_path);
        for id in &ids_to_remove {
            self.edges.remove(id);
            self.reverse.remove(id);
        }
        // O(ids) set for retain on large adjacency lists.
        let drop: std::collections::HashSet<&Id> = ids_to_remove.iter().collect();
        for targets in self.edges.values_mut() {
            targets.retain(|id| !drop.contains(id));
        }
        for sources in self.reverse.values_mut() {
            sources.retain(|id| !drop.contains(id));
        }
        ids_to_remove
    }

    /// Drop only outbound CALL/usage edges from blocks in `norm_path` (blocks stay).
    /// Used when re-linking reverse dependents after a callee file changes.
    fn clear_outbound_edges_for_file(&mut self, norm_path: &Path) {
        let ids: Vec<Id> = self
            .nodes
            .iter()
            .filter(|(_, b)| b.file == norm_path)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            if let Some(targets) = self.edges.remove(id) {
                for t in targets {
                    if let Some(srcs) = self.reverse.get_mut(&t) {
                        srcs.retain(|s| s != id);
                    }
                }
            }
        }
    }

    /// Re-collect call/usage edges for one already-resident file using a global name map.
    fn recollect_edges_for_resident_file(
        &mut self,
        norm_path: &Path,
        root: &Path,
        global: Option<&std::collections::HashMap<String, Id>>,
        go_all: Option<&std::collections::HashMap<String, Vec<Id>>>,
    ) {
        let ext = norm_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !is_edge_buildable_ext(ext) {
            return;
        }
        let abs = abs_source_path(root, norm_path);
        let Ok(source) = std::fs::read_to_string(&abs) else {
            return;
        };
        let Ok(parsed) = parser::parse_file(norm_path, &source) else {
            return;
        };
        let Some(ref tree) = parsed.tree else {
            return;
        };
        self.clear_outbound_edges_for_file(norm_path);
        let edges = collect_edges_for_lang(
            norm_path,
            ext,
            &parsed.blocks,
            &source,
            tree,
            global,
            go_all,
        );
        for (from, to) in edges {
            self.add_edge(from, to);
        }
        self.files_with_edges.insert(norm_path.to_path_buf());
    }

    /// Insert parsed blocks + hash; does **not** build edges (caller does with global map).
    fn insert_parsed_file_blocks(&mut self, norm_path: &Path, parsed: parser::ParsedFile) {
        let pstr = norm_path.to_string_lossy().to_string();
        self.file_hashes
            .insert(pstr, CodeGraph::content_hash(&parsed.source));
        for block in parsed.blocks {
            self.nodes.insert(block.id.clone(), block);
        }
    }

    /// Incrementally updates a single file in the graph.
    ///
    /// Evicts all blocks (and their edges) belonging to the file, reparses only that file,
    /// rebuilds its call/usage edges **with the same-lang global name map** (cross-file CALL),
    /// re-edges reverse dependents that lost A→F when F was stripped, then finalizes.
    ///
    /// This is the key method used by the live file watcher to keep the graph fresh
    /// without doing a full `scan_workspace` on every edit.
    pub fn update_single_file(&mut self, path: &PathBuf, root: &Path, verbose: bool) {
        self.update_files_batch(std::slice::from_ref(path), root, verbose);
    }

    /// Multi-file incremental update (watcher batch).
    ///
    /// **Critical for cross-file CALL truth:** all batch files get blocks inserted first,
    /// then one global name-map build, then edges for every batch file + reverse dependents.
    /// Processing A then B one-at-a-time with no global map left parent.callees empty while
    /// Trace loc-fallback still listed parent under callee.callers (soft M2 asymmetry).
    pub fn update_files_batch(&mut self, paths: &[PathBuf], root: &Path, verbose: bool) {
        if paths.is_empty() {
            return;
        }
        if verbose {
            if paths.len() == 1 {
                println!("🔄 Watcher triggering incremental update for: {:?}", paths[0]);
            } else {
                println!(
                    "🔄 Watcher batch incremental update: {} file(s)",
                    paths.len()
                );
            }
        }

        let pp = super::project_paths::ProjectPaths::new(root);
        let mut norm_paths: Vec<PathBuf> = Vec::with_capacity(paths.len());
        let mut seen = std::collections::HashSet::new();
        for path in paths {
            let norm = PathBuf::from(normalize_path(&pp.to_rel(path).to_string_lossy()));
            if seen.insert(norm.clone()) {
                norm_paths.push(norm);
            }
        }
        let self_files: std::collections::HashSet<PathBuf> =
            norm_paths.iter().cloned().collect();

        // Reverse dependents of *old* symbols (before strip) — restore A→B after B reparse.
        let mut reedge_extra: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::new();
        for norm in &norm_paths {
            let ids: Vec<Id> = self
                .nodes
                .iter()
                .filter(|(_, b)| b.file == *norm)
                .map(|(id, _)| id.clone())
                .collect();
            reedge_extra.extend(self.reverse_dependent_files(&ids, &self_files));
        }

        // Evict all batch files, then reparse + insert blocks (edges later with shared map).
        // IDs are content-addressed (file:kind:hash) so a second parse for edge collect matches.
        let mut parsed_ok: Vec<PathBuf> = Vec::new();
        for norm in &norm_paths {
            self.evict_file_blocks_and_edges(norm);
            let abs = pp.to_abs(norm);
            match std::fs::read_to_string(&abs)
                .ok()
                .and_then(|source| parser::parse_file(norm, &source).ok())
            {
                Some(parsed) => {
                    self.insert_parsed_file_blocks(norm, parsed);
                    // Mark edges pending until we recollect below.
                    self.files_with_edges.remove(norm);
                    parsed_ok.push(norm.clone());
                }
                None => {
                    // File deleted / unreadable — drop hash; reverse deps still re-edged.
                    let pstr = norm.to_string_lossy().to_string();
                    self.file_hashes.remove(&pstr);
                    self.files_with_edges.remove(norm);
                }
            }
        }

        self.invalidate_call_name_maps();
        // Global maps include newly inserted batch blocks → cross-file CALL within batch.
        let global_maps = call_name_maps_snapshot(self);

        for norm in &parsed_ok {
            let ext = norm.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !is_edge_buildable_ext(ext) {
                continue;
            }
            let go_all = if ext == "go" {
                Some(&global_maps.go_all)
            } else {
                None
            };
            self.recollect_edges_for_resident_file(
                norm,
                root,
                Some(global_maps.for_ext(ext)),
                go_all,
            );
        }

        // Restore A→F for files outside the batch that called into F before the strip.
        for dep in &reedge_extra {
            let ext = dep.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !is_edge_buildable_ext(ext) {
                continue;
            }
            let go_all = if ext == "go" {
                Some(&global_maps.go_all)
            } else {
                None
            };
            self.recollect_edges_for_resident_file(
                dep,
                root,
                Some(global_maps.for_ext(ext)),
                go_all,
            );
        }

        if !parsed_ok.is_empty() {
            super::linker::run_post_edge_passes(self, None, Some(root));
        }

        self.finalize_build();
        self.rebuild_module_hashes();
        self.version += 1;
        self.invalidate_trace_epoch();
    }

    /// Lazily loads crate name → version mapping using `cargo metadata`.
    ///
    /// Safe to call multiple times — it is a no-op if the data is already present.
    /// This implements Phase 2 lazy loading from the skeleton-first plan: heavy
    /// `cargo metadata` work is no longer done during the initial scan.
    pub fn ensure_dependency_versions(&mut self, root: &std::path::Path) {
        if self.dependency_versions.is_empty() {
            self.dependency_versions = lang::rust::load_dependency_versions(root);
        }
    }

    /// Builds call/usage edges on demand (Phase 3 lazy edge building) or surgically for JIT.
    ///
    /// If `target_files` is Some, only builds edges for those files (surgical JIT for Trace/FindImpl
    /// while background is running). Otherwise full workspace (for legacy or when bg complete).
    /// Always marks processed files in `files_with_edges` and updates `background_edge_build_complete`.
    ///
    /// Background full build uses batching + brief locks outside this (see server.rs bg loop).
    /// This method is the core used by both JIT and final ensure.
    pub fn ensure_call_graph(
        &mut self,
        root: &std::path::Path,
        config_skip_directories: &[String],
        target_files: Option<&[std::path::PathBuf]>,
    ) {
        // Caller (Butler server) must run `ensure_background_edge_build` when
        // `needs_background_edge_resuscitation()` is true so cancelled graphs respawn
        // their background builder before JIT/surgical edge work proceeds.
        let _ = self.heal_false_edge_complete();

        // Finding 3.2 fix + guard (prevent progress bar resets):
        // Only reset the edges_built_count (which drives the "Building Graph (XX%)" / progress telemetry)
        // when we are starting a true initial/empty build.
        // Subsequent surgical/JIT calls to ensure_call_graph (while a background build is running
        // at/near 100%, or on a warm graph) must NOT wipe the counter from 100% back to 0%.
        if self.edges.is_empty() {
            self.edges_built_count.store(0, Ordering::Relaxed);
        }
        let edges_built_count = self.edges_built_count.clone();

        let _skip_patterns = scanner::get_skip_patterns(root, config_skip_directories);

        // Per-language global maps (HIT 5) — never polyglot for CALL resolution.
        // Cached on the graph: rebuild only when nodes change (not every Trace JIT).
        let global_maps = call_name_maps_snapshot(self);

        let files_to_process: Vec<std::path::PathBuf> = if let Some(specific) = target_files {
            specific
                .iter()
                .filter(|p| {
                    // Path-form tolerant (src/foo.c vs ./src/foo.c) — contains alone re-JITed 150+ files.
                    !self.file_has_edges(p)
                        && p.extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(is_edge_buildable_ext)
                })
                .cloned()
                .collect()
        } else {
            // HIT 8: Use in-memory file_hashes (populated by skeleton scan) instead of O(n) WalkDir every JIT.
            // No FS stat/walk for cache hits; only the known skeleton files that need edges.
            self.file_hashes
                .keys()
                .filter_map(|pstr| {
                    let p = PathBuf::from(normalize_path(pstr));
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if is_edge_buildable_ext(ext) && !self.file_has_edges(&p) {
                        Some(p)
                    } else {
                        None
                    }
                })
                .collect()
        };

        if files_to_process.is_empty() {
            // Surgical JIT with nothing left: do NOT re-run polyglot AC every Trace (1–2s serial).
            // Full ensure (target_files=None) still finishes polyglot once then marks complete.
            if target_files.is_none() {
                super::linker::run_post_edge_passes(self, None, Some(root));
                self.mark_background_edge_build_complete();
            }
            return;
        }

        // Surgical = targeted list. Cap so a buggy caller cannot re-FullEdge 4k files
        // under the write lock (torch Arch once JIT'd 4132 files → +650k edges, multi-min core).
        const MAX_SURGICAL_FILES: usize = 64;
        let surgical = target_files.is_some();
        let files_to_process: Vec<PathBuf> = if surgical && files_to_process.len() > MAX_SURGICAL_FILES
        {
            println!(
                "🔗 JIT surgical cap: {} → {} file(s) (refuse mini-FullEdge on hot path)",
                files_to_process.len(),
                MAX_SURGICAL_FILES
            );
            files_to_process.into_iter().take(MAX_SURGICAL_FILES).collect()
        } else {
            files_to_process
        };

        if surgical {
            println!(
                "🔗 JIT surgical edge build for {} file(s) (targeted for symbol)",
                files_to_process.len()
            );
        } else {
            println!("🔗 Lazy/full call graph building triggered for {:?}", root);
        }

        // Wrap heavy collection (the multi-lang parse + collect_call/usage) in the bounded pool (Fix 2 extension to ensure_call_graph).
        // par_iter + install as specified. (Note: to fully satisfy "write never held across install" the callers in server
        // should snapshot then brief-merge; the bg path already did this correctly.)
        let new_edges: Vec<(Id, Id)> = get_bounded_edge_pool(0).install(|| {
            let edges_built_count = edges_built_count.clone();
            files_to_process
                .par_iter()
                .filter_map(|path| {
                    edges_built_count.fetch_add(1, Ordering::Relaxed);
                    let abs = abs_source_path(root, path);
                    let source = std::fs::read_to_string(&abs).ok()?;
                    // Repo-relative for parse/Id — matches skeleton scan (ProjectPaths).
                    let rel_path = rel_source_path(root, path);
                    let parsed = parser::parse_file(&rel_path, &source).ok()?;
                    let tree = parsed.tree.as_ref()?;
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    let go_all = if ext == "go" {
                        Some(&global_maps.go_all)
                    } else {
                        None
                    };
                    let edges = collect_edges_for_lang(
                        &rel_path,
                        ext,
                        &parsed.blocks,
                        &source,
                        tree,
                        Some(global_maps.for_ext(ext)),
                        go_all,
                    );
                    Some(edges)
                })
                .flatten()
                .collect()
        });

        let new_edge_count = new_edges.len();
        for (from, to) in new_edges {
            self.add_edge(from, to);
        }

        for p in &files_to_process {
            self.files_with_edges.insert(p.clone());
        }

        // Full-graph post passes (C decl↔def over all nodes, FFI export tables, hubs)
        // are multi-second single-core on monsters and hold the warehouse write lock —
        // parking the background edge rayon pool. Surgical JIT: same-lang call edges only;
        // bg full-complete + polyglot pass still runs post-ready.
        if !surgical {
            super::linker::run_post_edge_passes(self, None, Some(root));
            self.mark_background_edge_build_complete();
            self.compute_hubs(0.05);
        }

        let total_edges = self.total_edges();
        println!(
            "✅ Edge build complete for batch: {} new edge instances (total edges now {})",
            new_edge_count, total_edges
        );

        // Do not clone+bincode the warehouse on surgical JIT (hot Trace path).
        // Background full-complete already save_graph_async. Surgical edges stay in RAM;
        // file_has_edges path tolerance avoids re-JIT next request.
        if !surgical {
            println!(
                "💾 Full edge ensure done under {} (total={}) — persist via bg/async path",
                root.display(),
                total_edges
            );
        } else {
            println!(
                "⚡ Surgical edges in RAM under {} (+{} this batch, total={}; skipped full-graph post-pass)",
                root.display(),
                new_edge_count,
                total_edges
            );
        }
    }

    /// Parse scoped frontend/backend files missing from the skeleton (e.g. stale cache built before
    /// TS/Svelte support). Returns paths that were newly ingested so callers can JIT edges.
    ///
    /// **Hard caps + O(1) membership** — never WalkDir the whole monorepo and never scan
    /// `nodes` per candidate (that treated every repo-relative warehouse file as "missing"
    /// vs absolute walk paths → 4k-file "surgical" JIT + 650k edges on torch Arch).
    pub fn ensure_scoped_files_parsed(
        &mut self,
        root: &Path,
        scope_paths: &Option<Vec<String>>,
        ignore_paths: &Option<Vec<String>>,
        config_skip_directories: &[String],
    ) -> Vec<PathBuf> {
        /// Max new files to parse in one request (surgical, not a second FullEdge).
        const MAX_SCOPED_INGEST: usize = 48;

        let (scopes, _ignores) = collector::resolved_scope_paths(scope_paths, ignore_paths);
        // No explicit scope → nothing to fill; never walk the whole root.
        if scopes.is_empty() {
            return vec![];
        }

        let skip_patterns = scanner::get_skip_patterns(root, config_skip_directories);
        let pp = super::project_paths::ProjectPaths::new(root);

        // O(files) membership — warehouse keys are repo-relative.
        let mut known: std::collections::HashSet<String> =
            self.file_hashes.keys().cloned().collect();
        for b in self.nodes.values() {
            known.insert(normalize_path(&b.file.to_string_lossy()));
        }

        let mut added = Vec::new();
        let mut candidates_seen = 0usize;

        // Walk each scope prefix only (not the entire monorepo).
        for scope in &scopes {
            if added.len() >= MAX_SCOPED_INGEST {
                break;
            }
            let scope_trim = scope.trim_start_matches("./").trim_end_matches('/');
            let start = if scope_trim.is_empty() || scope_trim == "." {
                root.to_path_buf()
            } else {
                root.join(scope_trim)
            };
            if !start.exists() {
                continue;
            }

            for path in WalkDir::new(&start)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .map(|e| e.path().to_path_buf())
                .filter(|p| scanner::should_scan_path_under(p, &skip_patterns, Some(root)))
            {
                if added.len() >= MAX_SCOPED_INGEST {
                    break;
                }
                candidates_seen += 1;

                let rel = pp.to_rel(&path);
                let rel_str = normalize_path(&rel.to_string_lossy());
                if rel_str.is_empty() || known.contains(&rel_str) {
                    continue;
                }
                // Scope check on repo-relative form (matches warehouse + scope_paths).
                if !collector::file_matches_scope(&rel_str, scope_paths, ignore_paths) {
                    continue;
                }

                if let Ok(source) = std::fs::read_to_string(&path) {
                    self.file_hashes
                        .insert(rel_str.clone(), CodeGraph::content_hash(&source));
                    known.insert(rel_str.clone());
                    // Parse with repo-relative path so Ids match the warehouse.
                    if let Ok(parsed) = parser::parse_file(&rel, &source) {
                        for block in parsed.blocks {
                            self.add_block(block);
                        }
                        added.push(rel);
                    }
                }
            }
        }

        if !added.is_empty() {
            println!(
                "📥 Scoped skeleton ingest: {} file(s) missing from cache (cap={}; candidates_scanned={})",
                added.len(),
                MAX_SCOPED_INGEST,
                candidates_seen
            );
        }
        added
    }

    /// Adds crate-level dependency edges based on `cargo metadata`.
    ///
    /// Analyzes local packages in the workspace and adds edges between crates
    /// that depend on each other. This operates at the crate level, not block level.
    pub fn add_dependency_edges(
        &mut self,
        workspace_root: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let metadata = MetadataCommand::new()
            .manifest_path(workspace_root.join("Cargo.toml"))
            .exec()?;

        let workspace_root_utf8 = Utf8PathBuf::from_path_buf(workspace_root.to_path_buf())
            .map_err(|p| format!("Non-UTF8 workspace path: {:?}", p))?;

        let local_packages: HashMap<String, Package> = metadata
            .packages
            .into_iter()
            .filter(|pkg| pkg.manifest_path.starts_with(&workspace_root_utf8))
            .map(|pkg| (pkg.name.to_string(), pkg))
            .collect();

        for (name, pkg) in &local_packages {
            for dep in &pkg.dependencies {
                if let Some(_dep_pkg) = local_packages.get(&dep.name) {
                    let from_id = self.get_or_create_crate_id(name);
                    let to_id = self.get_or_create_crate_id(&dep.name);
                    self.edges
                        .entry(from_id.clone())
                        .or_default()
                        .push(to_id.clone());
                    self.reverse.entry(to_id).or_default().push(from_id);
                }
            }
        }

        Ok(())
    }

    /// Gets or creates a crate-level node identifier.
    fn get_or_create_crate_id(&mut self, crate_name: &str) -> Id {
        let id_str = format!("crate:{}", crate_name);
        let id = Id::new("", "crate", &id_str);
        if !self.nodes.contains_key(&id) {
            self.nodes
                .insert(id.clone(), BlockInfo::new_crate(crate_name));
        }
        id
    }
}

/// Background full edge build (tests / direct call). Prefer [`run_background_full_edge_build_policed`]
/// via WarehousePolice so FullEdge jobs serialize and JIT can run between batches.
pub fn run_background_full_edge_build(
    graph: Arc<std::sync::RwLock<CodeGraph>>,
    cancel: Arc<AtomicBool>,
    root: std::path::PathBuf,
    config_skip_directories: Vec<String>,
    telemetry: Option<Arc<BgBuildProgress>>,
    edge_threads: usize,
) {
    let need_post = run_background_full_edge_build_policed(
        Arc::clone(&graph),
        cancel,
        root.clone(),
        config_skip_directories,
        telemetry.clone(),
        edge_threads,
        None,
    );
    if need_post {
        run_deferred_warehouse_post_pass_with_telemetry(graph, root, telemetry, None);
    } else {
        // No PostPass: stamp Complete now (thin link left Running when inv complete).
        let mut g = graph
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let files_done = g.files_with_edges.len().max(1);
        g.mark_fully_complete_after_lto(telemetry.as_deref(), files_done);
        g.interconnect_session_ready = true;
    }
}

/// Full edge build body for WarehousePolice. `between_batches` runs after each stream-merge
/// (drain interactive JIT without waiting for whole-repo Complete).
///
/// Returns `true` if inventory completed and a **deferred PostPass** should run (compiler LTO).
pub fn run_background_full_edge_build_policed(
    graph: Arc<std::sync::RwLock<CodeGraph>>,
    cancel: Arc<AtomicBool>,
    root: std::path::PathBuf,
    config_skip_directories: Vec<String>,
    telemetry: Option<Arc<BgBuildProgress>>,
    edge_threads: usize,
    mut between_batches: Option<Box<dyn FnMut() + Send>>,
) -> bool {
    let root_for_recovery = root.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_background_full_edge_build_inner(
            Arc::clone(&graph),
            Arc::clone(&cancel),
            root,
            config_skip_directories,
            telemetry.clone(),
            edge_threads,
            between_batches
                .as_mut()
                .map(|f| f.as_mut() as &mut dyn FnMut()),
        )
    }));
    match result {
        Ok(need_post) => need_post,
        Err(_) => {
            eprintln!(
                "❌ Background full edge build panicked for {} — marking Error",
                root_for_recovery.display()
            );
            let files_done = {
                let g = graph
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                g.files_with_edges.len()
            };
            if let Ok(mut g) = graph.write() {
                g.mark_background_edge_build_failed();
                g.sync_bg_telemetry(telemetry.as_deref());
                let _ = scanner::save_graph(&g, &root_for_recovery);
            }
            if let Some(t) = telemetry {
                t.mark_failed(files_done);
            }
            false
        }
    }
}

fn run_background_full_edge_build_inner(
    graph: Arc<std::sync::RwLock<CodeGraph>>,
    cancel: Arc<AtomicBool>,
    root: std::path::PathBuf,
    config_skip_directories: Vec<String>,
    telemetry: Option<Arc<BgBuildProgress>>,
    edge_threads: usize,
    mut between_batches: Option<&mut dyn FnMut()>,
) -> bool {
    if cancel.load(Ordering::Relaxed) {
        println!(
            "🛑 Background edge build cancelled before start for {}",
            root.display()
        );
        let mut g = graph
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        g.mark_background_edge_build_cancelled();
        g.sync_bg_telemetry(telemetry.as_deref());
        if let Some(t) = &telemetry {
            t.thread_active.store(false, Ordering::Relaxed);
        }
        return false;
    }

    // Fresh compile: drop prior edge object files for this root.
    full_edge_phase(&telemetry, &root, "clear_objects");
    let _ = scanner::shards::clear_edge_batch_objects(&root);

    let rayon_threads = edge_pool_threads(edge_threads);
    let mut batch_budget = edge_batch_budget(rayon_threads);
    println!(
        "🚀 Edge build (WarehousePolice lane): {} threads={} (mem-capped) batch max_files={} max_bytes≈{:.1} MiB MemAvailable≈{:.1} GiB tier≈{:.1} GiB rss≈{:.1} GiB",
        root.display(),
        rayon_threads,
        batch_budget.max_files,
        batch_budget.max_bytes as f64 / MIB as f64,
        mem_budget_bytes() as f64 / GIB as f64,
        edge_mem_tier_bytes() as f64 / GIB as f64,
        process_rss_bytes().unwrap_or(0) as f64 / GIB as f64
    );

    let processed = telemetry
        .as_ref()
        .map(|t| Arc::clone(&t.files_processed))
        .unwrap_or_else(|| Arc::new(AtomicUsize::new(0)));

    full_edge_phase(&telemetry, &root, "mark_running");
    {
        let mut g = match full_edge_write(
            &graph,
            &telemetry,
            &root,
            "mark_running",
            FULL_EDGE_WRITE_WAIT_SECS,
        ) {
            Ok(g) => g,
            Err(()) => {
                full_edge_abort_incomplete(&graph, &telemetry, "write_wait_timeout mark_running");
                return false;
            }
        };
        g.mark_background_edge_build_running();
        g.sync_bg_telemetry(telemetry.as_deref());
    }
    if let Some(t) = &telemetry {
        t.thread_active.store(true, Ordering::Relaxed);
        t.beat();
    }

    // Reset the progress counter ONLY ONCE at the very beginning of the bg build (not per chunk / not later).
    // This fixes the 0% reset bug during long builds.
    let edges_built_count: Arc<AtomicUsize> = {
        let g = graph
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        g.edges_built_count.store(0, Ordering::Relaxed);
        g.edges_built_count.clone()
    };

    processed.store(0, std::sync::atomic::Ordering::Relaxed);

    let _skip_patterns = scanner::get_skip_patterns(&root, &config_skip_directories);

    // Snapshot skeleton + per-lang global maps.
    // File list under **read** from `file_hashes` (O(files) ≪ O(nodes) on gecko 4.8M→32k).
    // Walking every node PathBuf was single-core multi-minute "weak utilization" pre-batch.
    full_edge_phase(&telemetry, &root, "inventory");
    let all_files = {
        let g = graph
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut files: Vec<std::path::PathBuf> = if !g.file_hashes.is_empty() {
            g.file_hashes.keys().map(std::path::PathBuf::from).collect()
        } else {
            // Fallback only when inventory empty (tiny / mid-scan graphs).
            let mut v: Vec<std::path::PathBuf> =
                g.nodes.values().map(|b| b.file.clone()).collect();
            v.sort();
            v.dedup();
            v
        };
        files.sort();
        files.dedup();
        println!(
            "📋 FullEdge file inventory: {} path(s) (from {})",
            files.len(),
            if g.file_hashes.is_empty() {
                "nodes"
            } else {
                "file_hashes"
            }
        );
        files
    };
    if let Some(t) = &telemetry {
        t.beat();
    }
    full_edge_phase(&telemetry, &root, "call_name_maps");
    let global_maps = {
        let mut g = match full_edge_write(
            &graph,
            &telemetry,
            &root,
            "call_name_maps",
            FULL_EDGE_WRITE_WAIT_SECS,
        ) {
            Ok(g) => g,
            Err(()) => {
                full_edge_abort_incomplete(&graph, &telemetry, "write_wait_timeout call_name_maps");
                return false;
            }
        };
        // Arc clone — do not deep-copy multi‑M HashMaps into the edge pool.
        call_name_maps_snapshot(&mut g)
    };
    if let Some(t) = &telemetry {
        t.beat();
    }

    // Pool only after global slot — avoids dual 15-thread edge pools fighting for RAM.
    full_edge_phase(&telemetry, &root, "stream_setup");
    let pool = get_bounded_edge_pool(edge_threads);

    // Snapshot once — avoid per-file graph.read() (was serial + lock thrash in btop).
    let already_edged: std::collections::HashSet<PathBuf> = {
        let g = graph
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        g.files_with_edges.iter().cloned().collect()
    };
    let mut to_do: Vec<PathBuf> = all_files
        .into_iter()
        .filter(|p| !already_edged.contains(p))
        .collect();
    // Path-island locality (dom/base before layout/…) before byte-budget batching.
    sort_files_for_edge_locality(&mut to_do);
    let total = to_do.len();
    processed.store(0, std::sync::atomic::Ordering::Relaxed);
    if let Some(t) = &telemetry {
        t.files_total
            .store(total.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    // Stream-merge: parallel parse of a RAM-budgeted batch, then brief write-lock merge.
    full_edge_phase(&telemetry, &root, "streaming");
    let collect_start = Instant::now();
    println!(
        "🚀 Streaming edge build: {} files, mem-aware batches (≤{} files / ≤{:.1} MiB), {} butler-edge threads",
        total,
        batch_budget.max_files,
        batch_budget.max_bytes as f64 / MIB as f64,
        rayon_threads
    );
    let read_fail = AtomicUsize::new(0);
    let parse_fail = AtomicUsize::new(0);
    let skipped_ext = AtomicUsize::new(0);
    let ok_files = AtomicUsize::new(0);
    let mut total_edge_pairs = 0usize;
    let mut batches_done = 0usize;
    let mut cursor = 0usize;
    while cursor < total {
        if cancel.load(Ordering::Relaxed) {
            println!(
                "🛑 Background edge build cancelled (workspace switch or request) for {}",
                root.display()
            );
            let mut g = graph
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            g.mark_background_edge_build_cancelled();
            g.sync_bg_telemetry(telemetry.as_deref());
            if let Some(t) = &telemetry {
                t.thread_active.store(false, Ordering::Relaxed);
            }
            let _ = scanner::save_graph(&g, &root);
            return false;
        }

        // Host black pressure: brief yield so co-tenants can reclaim — not a long stall.
        // (Earlier 2 min pause loops were too tight for co-tenant leviathans.)
        {
            let p = crate::sys_pressure::snapshot();
            if p.tier >= crate::sys_pressure::PressureTier::Black {
                println!(
                    "⏸️  FullEdge brief yield (black pressure) for {} — {} before batch {}",
                    root.display(),
                    p.summary_line(),
                    batches_done + 1
                );
                std::thread::sleep(std::time::Duration::from_millis(750));
            }
        }

        let batch_started = Instant::now();
        let (next, chunk_paths) = take_edge_batch(&to_do, cursor, &root, batch_budget);
        cursor = next;
        if chunk_paths.is_empty() {
            break;
        }

        let batch_id = batches_done + 1;
        let first_disp = chunk_paths
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let done_before = processed.load(Ordering::Relaxed);
        let pct_before = if total > 0 {
            ((done_before as u64 * 100) / total as u64).min(99)
        } else {
            0
        };
        println!(
            "⚡ Edge batch {} start: {} files (~{}/{} = {}%) first={} threads={}",
            batch_id,
            chunk_paths.len(),
            done_before,
            total,
            pct_before,
            first_disp,
            rayon_threads
        );

        let batch_edges: Vec<(Id, Id)> = pool.install(|| {
            let edges_built_count = edges_built_count.clone();
            let root = root.clone();
            let global_maps = &global_maps;
            chunk_paths
                .par_iter()
                .filter_map(|p: &PathBuf| {
                    if cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    let file_t0 = Instant::now();
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if !is_edge_buildable_ext(ext) {
                        skipped_ext.fetch_add(1, Ordering::Relaxed);
                        processed.fetch_add(1, Ordering::Relaxed);
                        return None;
                    }
                    let abs = abs_source_path(&root, p);
                    let rel_for_ids = rel_source_path(&root, p);
                    let out = match std::fs::read_to_string(&abs) {
                        Ok(source) => match parser::parse_file(&rel_for_ids, &source) {
                            Ok(parsed) => {
                                if let Some(ref tree) = parsed.tree {
                                    let go_all = if ext == "go" {
                                        Some(&global_maps.go_all)
                                    } else {
                                        None
                                    };
                                    let edges = collect_edges_for_lang(
                                        &rel_for_ids,
                                        ext,
                                        &parsed.blocks,
                                        &source,
                                        tree,
                                        Some(global_maps.for_ext(ext)),
                                        go_all,
                                    );
                                    ok_files.fetch_add(1, Ordering::Relaxed);
                                    Some(edges)
                                } else {
                                    parse_fail.fetch_add(1, Ordering::Relaxed);
                                    None
                                }
                            }
                            Err(_) => {
                                parse_fail.fetch_add(1, Ordering::Relaxed);
                                None
                            }
                        },
                        Err(_) => {
                            read_fail.fetch_add(1, Ordering::Relaxed);
                            None
                        }
                    };
                    let secs = file_t0.elapsed().as_secs_f64();
                    if secs >= SLOW_EDGE_FILE_SECS {
                        println!(
                            "🐢 slow edge file {:.1}s  {}",
                            secs,
                            p.display()
                        );
                    }
                    let n_done = processed.fetch_add(1, Ordering::Relaxed) + 1;
                    edges_built_count.fetch_add(1, Ordering::Relaxed);
                    // Heartbeat + sparse mid-batch progress (batch 1 hangs were silent).
                    if n_done == 1 || n_done.is_multiple_of(25) {
                        if let Some(t) = &telemetry {
                            t.beat();
                        }
                    }
                    if n_done.is_multiple_of(40) || n_done <= 3 {
                        println!(
                            "   … edge batch {} file {}/{} (+pairs pending merge)",
                            batch_id,
                            n_done.min(chunk_paths.len()),
                            chunk_paths.len()
                        );
                    }
                    out
                })
                .flatten()
                .collect()
        });

        if let Some(t) = &telemetry {
            t.beat();
        }
        let parse_secs = batch_started.elapsed().as_secs_f64();
        let batch_n = batch_edges.len();
        total_edge_pairs += batch_n;
        // Persist .o off the merge path only when small — deep-clone of multi‑M edge
        // pairs after a leviathan batch is another OOM/swap trap (gecko batch 1).
        if !batch_edges.is_empty() && batch_edges.len() < 50_000 {
            let root_obj = root.clone();
            let edges_obj = batch_edges.clone();
            let _ = std::thread::Builder::new()
                .name("butler-edge-obj".into())
                .spawn(move || {
                    if let Err(e) =
                        scanner::shards::write_edge_batch_object(&root_obj, batch_id, &edges_obj)
                    {
                        eprintln!(
                            "⚠️  edge batch object write failed (batch {batch_id}): {e}"
                        );
                    }
                });
        } else if batch_edges.len() >= 50_000 {
            println!(
                "⏭️  Edge batch {} skip async .o clone ({} pairs — merge-only)",
                batch_id, batch_n
            );
        }
        // Heartbeat ticker while merge holds write (can be minutes on huge batches).
        let merge_alive = Arc::new(AtomicBool::new(true));
        if let Some(t) = telemetry.clone() {
            let alive = Arc::clone(&merge_alive);
            let _ = std::thread::Builder::new()
                .name("butler-edge-hb".into())
                .spawn(move || {
                    while alive.load(Ordering::Relaxed) {
                        t.beat();
                        std::thread::sleep(std::time::Duration::from_secs(20));
                    }
                    t.beat();
                });
        }
        println!(
            "⚡ Edge batch {} merging {} pairs (write lock) …",
            batch_id, batch_n
        );
        let merge_t0 = Instant::now();
        {
            let mut g = match full_edge_write(
                &graph,
                &telemetry,
                &root,
                &format!("merge_batch_{batch_id}"),
                FULL_EDGE_WRITE_WAIT_SECS,
            ) {
                Ok(g) => g,
                Err(()) => {
                    full_edge_abort_incomplete(
                        &graph,
                        &telemetry,
                        &format!("write_wait_timeout merge_batch_{batch_id}"),
                    );
                    return false;
                }
            };
            // Thin link: apply delta only (no whole-program analysis here).
            g.add_edges_batch_vec(batch_edges);
            for p in &chunk_paths {
                g.files_with_edges.insert(p.clone());
            }
            // Bump version every 4 batches (not every merge) — fewer cache invalidations.
            if batch_id == 1 || batch_id % 4 == 0 || cursor >= total {
                g.version = g.version.saturating_add(1);
            }
            g.sync_bg_telemetry(telemetry.as_deref());
        }
        merge_alive.store(false, Ordering::Relaxed);
        let merge_secs = merge_t0.elapsed().as_secs_f64();
        if let Some(t) = &telemetry {
            t.beat();
        }
        batches_done += 1;

        let batch_secs = batch_started.elapsed().as_secs_f64();
        let n_files = chunk_paths.len().max(1) as f64;
        let ceiling = edge_batch_budget_ceiling(rayon_threads);
        // Adaptive shrink: thrash-y batches get smaller next time.
        if batch_secs > 30.0 && batch_secs / n_files > 2.0 {
            batch_budget.max_files = (batch_budget.max_files / 2).max(8);
            batch_budget.max_bytes = (batch_budget.max_bytes / 2).max(512 * 1024);
            println!(
                "📉 Edge batch slow ({:.1}s / {} files) — shrink budget → max_files={} max_bytes≈{:.1} MiB",
                batch_secs,
                chunk_paths.len(),
                batch_budget.max_files,
                batch_budget.max_bytes as f64 / MIB as f64
            );
        } else if batch_secs < 8.0
            && chunk_paths.len() >= batch_budget.max_files.saturating_sub(2)
            && batch_budget.max_files < ceiling.max_files
        {
            // Adaptive grow: keep workers fed when merges are cheap.
            batch_budget.max_files = (batch_budget.max_files
                + (batch_budget.max_files / 4).max(8))
            .min(ceiling.max_files);
            batch_budget.max_bytes = (batch_budget.max_bytes
                + (batch_budget.max_bytes / 4).max(MIB))
            .min(ceiling.max_bytes);
            println!(
                "📈 Edge batch fast ({:.1}s / {} files) — grow budget → max_files={} max_bytes≈{:.1} MiB",
                batch_secs,
                chunk_paths.len(),
                batch_budget.max_files,
                batch_budget.max_bytes as f64 / MIB as f64
            );
        }

        // Always log batch end (torch stalls were invisible when only every 8th printed).
        {
            let done = processed.load(Ordering::Relaxed);
            let pct = if total > 0 {
                ((done as u64 * 100) / total as u64).min(99)
            } else {
                0
            };
            // Avoid O(all edges) total_edges() every batch on leviathans — sample every 8.
            let edges_note = if batches_done == 1 || batches_done % 8 == 0 || cursor >= total {
                let edges_now = {
                    let g = graph
                        .read()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    g.total_edges()
                };
                format!(" warehouse_edges={edges_now}")
            } else {
                String::new()
            };
            println!(
                "⚡ Edge batch {} done: +{} pairs files {}/{} ({}%) parse={:.1}s merge={:.1}s cum={:.1?}{}",
                batches_done,
                batch_n,
                done,
                total,
                pct,
                parse_secs,
                merge_secs,
                collect_start.elapsed(),
                edges_note
            );
        }

        // WarehousePolice: drain interactive JIT between batches (EvePolice-style yield).
        if let Some(hook) = between_batches.as_mut() {
            hook();
        }
    }

    println!(
        "⚡ Stream edge collect done: {} edge pairs in {:.2?} (ok_files={} read_fail={} parse_fail={} skip_ext={})",
        total_edge_pairs,
        collect_start.elapsed(),
        ok_files.load(Ordering::Relaxed),
        read_fail.load(Ordering::Relaxed),
        parse_fail.load(Ordering::Relaxed),
        skipped_ext.load(Ordering::Relaxed),
    );
    if read_fail.load(Ordering::Relaxed) > 0 && ok_files.load(Ordering::Relaxed) == 0 {
        eprintln!(
            "⚠️  All edge source reads failed — check path join (root={}, sample={:?})",
            root.display(),
            to_do.first()
        );
    }

    processed.store(total.max(1), std::sync::atomic::Ordering::Relaxed);
    std::thread::yield_now();

    // ── Thin link (compiler final link): inventory complete, no whole-program LTO ──
    full_edge_phase(&telemetry, &root, "thin_link");
    let link_start = Instant::now();
    let inventory_complete = {
        let mut g = match full_edge_write(
            &graph,
            &telemetry,
            &root,
            "thin_link",
            FULL_EDGE_WRITE_WAIT_SECS,
        ) {
            Ok(g) => g,
            Err(()) => {
                full_edge_abort_incomplete(&graph, &telemetry, "write_wait_timeout thin_link");
                return false;
            }
        };
        // Credit every file the stream walked (path dialects included).
        for p in &to_do {
            g.files_with_edges.insert(p.clone());
        }
        // O(1) inventory closed — FullEdge enumerated all unique node files.
        // Do **not** rewalk edgeable_file_inventory (1.2M PathBuf clones on torch).
        g.mark_edge_inventory_closed();
        // Light hubs only (degree on existing edges) — cheap enough for Arch.
        // Cap work on leviathans (same gate as PostPass).
        if g.nodes.len() <= 80_000 {
            g.compute_hubs(0.05);
        }
        // Inventory mapped; keep health **Running** until deferred PostPass finishes
        // (do not stamp Complete yet — that was the "done while LTO still burns" lie).
        g.background_edge_build_active = true;
        g.background_edge_build_complete = false;
        g.background_edge_build_state = BackgroundEdgeBuildState::Running;
        if let Some(t) = telemetry.as_deref() {
            use std::sync::atomic::Ordering;
            let n = g.files_with_edges.len().max(total).max(1);
            t.files_total.store(n, Ordering::Relaxed);
            // Cap reported progress under 100 until LTO Complete (honest health).
            t.files_processed.store(n.saturating_sub(1).max(1), Ordering::Relaxed);
            t.set_state(BackgroundEdgeBuildState::Running);
            t.thread_active.store(true, Ordering::Relaxed);
            t.clear_blocked();
            t.set_phase("thin_link_done");
        }
        true
    };
    println!(
        "⚡ Thin link (inventory complete, hubs) in {:.2?} — PostPass deferred",
        link_start.elapsed()
    );

    // Do **not** `save_graph_async` here. PostPass always follows on success and saves
    // once at LTO end. A second multi‑GB slim under the graph read lock (gecko ~3 GiB
    // clone) raced Trace Phase‑4 writers and left the warehouse try_read_busy for minutes.
    // Edge batch objects on disk already cover crash mid‑PostPass.
    println!(
        "✅ Edge collect COMPLETE for {} ({} edges) — whole-program PostPass queued separately (persist after LTO)",
        root.display(),
        {
            let g = graph
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            g.total_edges()
        }
    );
    inventory_complete
}

/// Deferred whole-program post-pass (LTO-ish): decl↔def, FFI, optional AC, IPC.
///
/// **Map-reduce:** each chunk calculates edges under **read** (rayon-friendly), then
/// appends under a **brief write**. Yields between chunks so WarehousePolice can JIT.
/// Stamps `mark_fully_complete` only when LTO finishes (honest health).
pub fn run_deferred_warehouse_post_pass(
    graph: Arc<std::sync::RwLock<CodeGraph>>,
    root: std::path::PathBuf,
    between_chunks: Option<Box<dyn FnMut() + Send>>,
) {
    run_deferred_warehouse_post_pass_with_telemetry(graph, root, None, between_chunks);
}

/// Same as [`run_deferred_warehouse_post_pass`] with lock-free progress telemetry.
pub fn run_deferred_warehouse_post_pass_with_telemetry(
    graph: Arc<std::sync::RwLock<CodeGraph>>,
    root: std::path::PathBuf,
    telemetry: Option<Arc<BgBuildProgress>>,
    mut between_chunks: Option<Box<dyn FnMut() + Send>>,
) {
    println!(
        "🔗 Deferred PostPass start (whole-program LTO, map-reduce) for {}",
        root.display()
    );
    let t0 = Instant::now();
    let mut yield_lane = || {
        if let Some(ref mut h) = between_chunks {
            h();
        }
    };

    // ── Chunk 1: C decl↔def (map under read, reduce under write) ──
    let c_edges = {
        let g = graph
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let t_map = Instant::now();
        let edges = super::linker::map_c_decl_def_edges(&g);
        if !edges.is_empty() || super::linker::graph_has_c_family(&g) {
            println!(
                "🔗 PostPass map C decl↔def: {} edges in {:.2?}",
                edges.len(),
                t_map.elapsed()
            );
        }
        edges
    };
    if !c_edges.is_empty() {
        let n = c_edges.len();
        let t_red = Instant::now();
        let mut g = graph
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Blind-append reduce (no HashSet merge) — sort-and-squish at LTO end.
        g.add_edges_batch(c_edges);
        println!(
            "⚡ C decl↔def: {} implements edges (blind reduce in {:.2?})",
            n,
            t_red.elapsed()
        );
    }
    yield_lane();

    // ── Chunk 2: FFI + TS (lang-gated) + optional AC + IPC ──
    let rest = {
        let g = graph
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let t_map = Instant::now();
        let maps = super::interconnect::map_without_decl_def(&g, None, Some(&root));
        println!(
            "🔗 PostPass interconnect map: {} call + {} bridge in {:.2?} (ts_js={})",
            maps.call.len(),
            maps.bridge.len(),
            t_map.elapsed(),
            super::interconnect::graph_has_ts_js(&g)
        );
        maps
    };
    {
        let mut g = graph
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if rest.total_len() > 0 {
            let n_call = rest.call.len();
            let n_br = rest.bridge.len();
            let t_red = Instant::now();
            super::interconnect::apply_post_edge_maps(&mut g, rest);
            println!(
                "⚡ PostPass interconnect reduce: {} CALL + {} typed bridge in {:.2?}",
                n_call,
                n_br,
                t_red.elapsed()
            );
        }
        // Contiguous topology for Trace / GNN / flat maps (not embedding RAG).
        g.normalize_adjacency();
        if g.nodes.len() <= 80_000 {
            g.compute_hubs(0.05);
        }
        // O(1) Complete — do not rewalk 1M+ nodes (inventory/audit hung torch at 99%).
        let files_done = g.files_with_edges.len().max(1);
        g.mark_fully_complete_after_lto(telemetry.as_deref(), files_done);
        // Bridges applied above (or empty). Hot-path Trace must not re-run interconnect.
        g.interconnect_session_ready = true;
    }
    yield_lane();

    scanner::save_graph_async(Arc::clone(&graph), root.clone());
    println!(
        "🔗 Deferred PostPass done for {} in {:.2?}",
        root.display(),
        t0.elapsed()
    );
}

#[cfg(test)]
mod edge_budget_tests {
    use super::*;

    #[test]
    fn incremental_batch_resolves_cross_file_call() {
        // Watcher soft-M2 repro: parent→callee across two new files must yield
        // both forward and reverse CALL after one batch co-update.
        let dir = std::env::temp_dir().join(format!(
            "butler_inc_cross_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let path_b = src.join("mf_b.rs");
        let path_a = src.join("mf_a.rs");
        let callee = "butler_mf_callee_test";
        let parent = "butler_mf_parent_test";
        std::fs::write(
            &path_b,
            format!("#[allow(dead_code)]\npub fn {callee}() {{}}\n"),
        )
        .unwrap();
        std::fs::write(
            &path_a,
            format!(
                "#[allow(dead_code)]\npub fn {parent}() {{\n    {callee}();\n}}\n"
            ),
        )
        .unwrap();

        let mut g = CodeGraph::new();
        // Process in A-then-B order (worst-case HashSet order) as one batch.
        g.update_files_batch(&[path_a.clone(), path_b.clone()], &dir, false);

        let parent_id = g
            .nodes
            .values()
            .find(|b| b.name == parent && b.kind.contains("function"))
            .map(|b| b.id.clone())
            .expect("parent block");
        let callee_id = g
            .nodes
            .values()
            .find(|b| b.name == callee && b.kind.contains("function"))
            .map(|b| b.id.clone())
            .expect("callee block");
        assert!(
            g.children(&parent_id).iter().any(|id| id == &callee_id),
            "parent must CALL callee (fwd)"
        );
        assert!(
            g.callers(&callee_id).iter().any(|id| id == &parent_id),
            "callee must list parent as caller (rev)"
        );

        // Reverse-dependent re-edge: rewrite callee file; parent→callee must survive.
        std::fs::write(
            &path_b,
            format!("#[allow(dead_code)]\npub fn {callee}() {{ /* touch */ }}\n"),
        )
        .unwrap();
        g.update_single_file(&path_b, &dir, false);
        let parent_id = g
            .nodes
            .values()
            .find(|b| b.name == parent && b.kind.contains("function"))
            .map(|b| b.id.clone())
            .expect("parent after B update");
        let callee_id = g
            .nodes
            .values()
            .find(|b| b.name == callee && b.kind.contains("function"))
            .map(|b| b.id.clone())
            .expect("callee after B update");
        assert!(
            g.children(&parent_id).iter().any(|id| id == &callee_id),
            "after callee-only update, reverse-dep re-edge must restore parent→callee"
        );
        assert!(
            g.callers(&callee_id).iter().any(|id| id == &parent_id),
            "after callee-only update, reverse must still list parent"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
