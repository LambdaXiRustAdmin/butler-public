//! On-disk graph cache + hash-delta load/save (scanner package — **cache** leaf).
//!
//! # Ownership (see [`super`] package map)
//! **Owns:** `CACHE_SCHEMA_VERSION` / graph+edge schema consts, `CachedGraph`, `save_graph` /
//! `load_graph` (smart loader), hash-delta invalidation, `incremental_reparse` when dirty set
//! is small, empty-Complete refuse paths, `BUTLER_FORCE_RESCAN`.
//!
//! **Does not own:** Parallel filesystem walk / parse waves ([`super`] mod.rs); multi-bin
//! part files and stitch ([`super::shards`]); FullEdge batch collect ([`crate::snooper::builder`]).
//!
//! # Soft-freeze
//! Schema version numbers are **warehouse contracts**. Peels must not renumber without an
//! explicit product decision and migrate note. S1 is documentation only.
//!
//! Facade: public symbols re-exported from `scanner/mod.rs` so call sites stay
//! `snooper::scanner::load_graph` / `save_graph` / `GRAPH_SCHEMA_VERSION`.
//! Zero intentional behavior change for S1.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::snooper::{BackgroundEdgeBuildState, CodeGraph};

use super::{get_skip_patterns, scan_workspace};

/// After load: distinguish a fully-built cache from a partial/cancelled mid-build snapshot.
fn finalize_loaded_graph_state(graph: &mut CodeGraph, project_root: &Path) {
    let t0 = std::time::Instant::now();
    // Trusted Complete cache (edges + stamp from prior FullEdge+LTO): skip O(nodes) canonize
    // and full name_index audit — those hung leviathan warm loads for minutes after
    // "Loaded fresh multi-bin" while /context still showed Building Graph 40%.
    let trusted_complete = graph.background_edge_build_complete
        && matches!(
            graph.background_edge_build_state,
            crate::snooper::model::BackgroundEdgeBuildState::Complete
        )
        && !graph.edges.is_empty();

    if trusted_complete {
        println!(
            "📂 Skip path canonize on trusted Complete cache ({} nodes, {} edge sources)",
            graph.nodes.len(),
            graph.edges.len()
        );
        // Ensure O(1) inventory flag for Complete stamp / progress.
        graph.mark_edge_inventory_closed();
    } else {
        // Warehouse invariant: all paths repo-relative under project root + Id rekey.
        graph.normalize_paths_to_root(project_root);
    }

    // Stamp-only name index — not full audit_name_index (O(nodes)×string Id).
    graph.ensure_name_index();
    if graph.name_index_is_stale() {
        graph.rebuild_name_index();
        println!(
            "📇 name_index rebuilt (load finalize): nodes={} keys={}",
            graph.nodes.len(),
            graph.name_index.len()
        );
    } else {
        println!(
            "📇 name_index OK (load finalize stamp): nodes={} keys={} stamp={}",
            graph.nodes.len(),
            graph.name_index.len(),
            graph.name_index_nodes_len
        );
    }
    // File→ids for Arch file-local collect (not stored in name_index.bin).
    if !graph.file_node_index_is_warm() && !graph.nodes.is_empty() {
        let t_fi = std::time::Instant::now();
        graph.rebuild_file_node_index_only();
        println!(
            "📇 file_node_index built (load finalize): files={} in {:.1}ms",
            graph.file_node_index.len(),
            t_fi.elapsed().as_secs_f64() * 1000.0
        );
    }

    // Restore Complete / inventory (fixes serde-skip amnesia → false FullEdge every boot).
    // No-ops quickly when already Complete from load_shards.
    graph.restore_edge_build_state_after_load(true);

    // Never re-run whole-program PostPass on every boot of a Complete warehouse.
    // That re-paid torch FFI map (~186s) after a successful LTO save.
    if graph.is_edge_build_complete() && !graph.edges.is_empty() {
        if graph.nodes.len() <= 80_000 {
            graph.compute_hubs(0.05);
        }
        // Interconnect already applied in PostPass (or empty is final). Stamp so Trace
        // never re-injects on first butler_ask (multi-second agent-killer).
        graph.interconnect_session_ready = true;
    }

    // Lang void: re-assess on crumb inventories (old caches pre-field, or spring-boot shape).
    if graph.warehouse_lang_void.is_none() && graph.file_hashes.len() < 64 {
        let skips = get_skip_patterns(project_root, &[]);
        if let Some(void) =
            crate::snooper::warehouse_lang::refresh_lang_void(project_root, &skips, None)
        {
            println!(
                "⚠️  Load: warehouse lang void .{}×{} (scanned files={}) — product queries will refuse",
                void.dominant_ext,
                void.unsupported_files,
                void.supported_files
            );
            graph.warehouse_lang_void = Some(void);
        }
    }

    println!(
        "📂 Load finalize done in {:.2?} (trusted_complete={trusted_complete})",
        t0.elapsed()
    );
}

/// Serialization / field layout of [`CodeGraph`] + [`BlockInfo`].
/// Bump only when on-disk shape changes (new required fields, renames, etc.).
/// Forces a full rescan when mismatched.
/// Bumped: C blocks tagged `lang=c` (was cpp); separate C/C++ indexers.
/// Bump when inventory membership / path policy changes (e.g. new vendor prune).
/// Bumped (v10): skip patterns match relative to project root (not absolute path prefix).
/// Bumped (v11): TS variable_declarator blocks (const Form = … export aliases).
/// Bumped (v12): dual-stack packages under examples/ stay in inventory (L2.1).
/// Bumped (v13): dual-stack under examples/ includes native+TS/JS (Tauri IPC apps, L2.3).
/// Bumped (v14): pybind binding roots keep `tests/` (m.def Export fixtures, A′.5).
/// Bumped 15: product warehouse definition-tier inventory only (drop statement /
/// expression AST kinds from permanent nodes). Old Complete caches full-rescan.
pub const GRAPH_SCHEMA_VERSION: u32 = 15;

/// Edge-building semantics (call/usage rules, linker passes).
///
/// # Bump policy (Complete cache reliability)
///
/// **Bump when** stored CALL/bridge edges would be **wrong or incomplete** under new rules
/// (who-connects-to-whom changes). Examples: link precision, bridge kinds, same-lang maps,
/// import/barrel resolution, AC default that changes edge population.
///
/// **Do not bump** for pure refactors, log-only changes, or Trace/pack presentation.
/// Layout / `BlockInfo` serde changes → [`GRAPH_SCHEMA_VERSION`] (full rescan), not edge_sem.
///
/// **Mismatch policy:** exact version match, or **drop edges** (keep skeleton). No migrators.
/// See `EDGE_SEM_POLICY.md` in this package.
///
/// Bump when edge logic changes but the serde layout is unchanged.
/// Mismatch keeps nodes + file/module hashes and forces an edge rebuild (no full rescan).
/// Bumped: CALLS resolve to function-like only (no type/struct callees) + edge persist.
/// Bumped (v10): pyfunction FFI export bridges + AC noise path skip.
/// Bumped (v11): pybind11 `m.def` export table (c_family drawer) + py link.
/// Bumped (v12): precise Python FFI link (no substring contains / full-file body).
/// Bumped (v13): polyglot AC off by default (structural FFI only; `BUTLER_POLYGLOT_AC=1` opt-in).
/// Bumped (v14): call-edge global name map is per-language (no py→rs via CALL fallback).
/// Bumped (v15): precise pyfunction attr attach + py fn body call-only FFI link.
/// Bumped (v16): shard load respects edge_sem (drop stale edges.bin).
/// Bumped (v17): TS QueryOnly calls + path prefs + relative import edges.
/// Bumped (v18): generalize py name prefs; JSX HTML intrinsics not call targets.
/// Bumped (v19): typed interconnect bridges (Export/Ipc/Twin) separate from CALL adjacency.
/// Bumped (v20): TS/JS import edges resolve `@/` tsconfig path aliases + multi-line imports.
/// Bumped (v21): TS const aliases (`const Form = …`) exportable for import edges.
/// Bumped (v22): L2.2 monorepo Export — co-located `*_py` twin + reject method-call
/// false bridges (`re.search(`) + skip guide/glossary Python noise paths.
/// Bumped (v23): L2.3 Tauri IPC rule (examples/src-tauri) + dual-stack frontend inventory.
/// Bumped (v24): pybind Export — module attr call `m.export(` for long structural names.
/// Bumped (v25): Python import-bound attribute CALL (import map + honest silence on dynamic).
/// Bumped (v26): import-bound attr require path_affinity > 0 (stdlib/pip silence).
/// Bumped (v27): TS/JS barrel re-export walk (export table + depth-8 named>star).
/// Bumped (v28): TS/JS import-bound call/JSX → re-export terminus (Cut 3).
/// Bumped (v29): Rust CALL edges QueryOnly (no Aggressive body-scan); kills
/// comment/string false callers (normalize_goal→handle_orchestrate) and relies
/// on Tree-sitter call_expression + per-lang global map for real cross-file CALL.
/// v30: IPC full-file re-read line-span filter + invoker rank (tauri log() not *_default).
/// v31: pybind Export never targets TEST_SUBMODULE / junk macro hosts.
pub const EDGE_SEMANTICS_VERSION: u32 = 31;

/// Back-compat alias: primary on-disk schema field is [`GRAPH_SCHEMA_VERSION`].
pub const CACHE_SCHEMA_VERSION: u32 = GRAPH_SCHEMA_VERSION;

fn default_edge_semantics_legacy() -> u32 {
    // Caches written before edge_semantics existed — treat as older than current.
    0
}

/// Fast O(1) check — graph.bin and/or progressive shards.
pub fn graph_cache_exists(root: impl AsRef<Path>) -> bool {
    let d = root.as_ref().join(".butler/cache");
    d.join("graph.bin").is_file() || super::shards::shards_exist(root)
}

/// Internal wrapper for the on-disk cache format.
/// `version` = graph schema; `edge_semantics` = edge-building logic (independent bump).
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedGraph {
    version: u32,
    #[serde(default = "default_edge_semantics_legacy")]
    edge_semantics: u32,
    graph: CodeGraph,
}

pub fn save_graph(graph: &CodeGraph, root: impl AsRef<Path>) -> std::io::Result<()> {
    // Slim once: never photocopy source text into the warehouse.
    save_graph_owned(graph.slim_for_cache(), root)
}

/// Persist an already-slim owned graph (no second slim walk, no extra structure clone).
/// Used by [`save_graph_async`] after a single snapshot under the read lock.
pub fn save_graph_owned(slim: CodeGraph, root: impl AsRef<Path>) -> std::io::Result<()> {
    let root = root.as_ref();
    // Refuse litter under src/examples/tests; probe real FS writability (root-owned Docker trap).
    let cache_dir = crate::snooper::ensure_project_butler_cache_dir(root)?;
    let cache_file = cache_dir.join("graph.bin");
    // Progressive multi-bin first (borrows slim), then consume into graph.bin.
    // Must not swallow errors — partial shard write + failed graph.bin poisons trust.
    super::shards::save_shards(&slim, root).map_err(|e| {
        eprintln!(
            "⚠️  save_shards failed for {} — warehouse not updated: {e}",
            root.display()
        );
        e
    })?;
    let n = slim.nodes.len();
    let cached = CachedGraph {
        version: GRAPH_SCHEMA_VERSION,
        edge_semantics: EDGE_SEMANTICS_VERSION,
        graph: slim,
    };
    let bytes = bincode::serialize(&cached).map_err(std::io::Error::other)?;
    std::fs::write(&cache_file, bytes).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            eprintln!(
                "⚠️  graph.bin write denied at {} — Complete edges stay in RAM only; \
                 next open may thrash FullEdge until cache is writable (chown .butler).",
                cache_file.display()
            );
        }
        e
    })?;
    println!(
        "💾 Graph cached to {} (schema v{}, edge_sem v{}, slim sources, {} blocks)",
        cache_file.display(),
        GRAPH_SCHEMA_VERSION,
        EDGE_SEMANTICS_VERSION,
        n
    );
    Ok(())
}

/// Snapshot under a **read** lock (slim once), then write in background.
/// Does **not** block the caller — cold Phase-1 ready path uses this so
/// `Async graph ready` is not delayed by multi‑GB bincode + disk.
///
/// **Single-flight process-wide:** leviathan slim clones (gecko multi‑GiB) must not
/// overlap — dual FullEdge thin-link + PostPass saves held the read lock twice and
/// pinned Phase‑4 `write()` / Trace lobby `try_read` for minutes.
pub fn save_graph_async(graph: Arc<std::sync::RwLock<CodeGraph>>, root: PathBuf) {
    let _ = std::thread::Builder::new()
        .name("butler-graph-save".into())
        .spawn(move || {
            // Serialize multi‑GB slim+disk so post-build cleanup cannot stack.
            static SAVE_SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _slot = SAVE_SLOT
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let t0 = Instant::now();
            let snap = {
                let g = graph
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                g.slim_for_cache()
            };
            // Release graph lock before multi‑GB bincode + disk (snap is owned).
            let n = snap.nodes.len();
            match save_graph_owned(snap, &root) {
                Ok(()) => {
                    println!(
                        "💾 Background graph save finished for {} ({} blocks) in {:.1?}",
                        root.display(),
                        n,
                        t0.elapsed()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "⚠️  background graph save failed for {}: {e}",
                        root.display()
                    );
                    if e.kind() == std::io::ErrorKind::PermissionDenied {
                        eprintln!(
                            "   → Cache not writable by this process. After FullEdge, disk still has \
                             old edge_sem → next warm drops edges and rebuilds forever. \
                             Fix ownership then re-warm: chown -R \"$(whoami)\" \"{}/.butler\"",
                            root.display()
                        );
                    }
                }
            }
        });
}

pub fn load_graph(
    root: impl AsRef<Path>,
    current_file: Option<Arc<std::sync::Mutex<Option<String>>>>,
    config_skip_directories: &[String],
) -> CodeGraph {
    let root = root.as_ref();
    let cache_dir = root.join(".butler/cache");
    let cache_file = cache_dir.join("graph.bin");

    // FORCE FULL RE-SCAN if env var is set (remove this line once injection is stable)
    if std::env::var("BUTLER_FORCE_RESCAN").is_ok() {
        println!("🔄 BUTLER_FORCE_RESCAN=true → forcing full rebuild (ignoring cache)");
        let graph = scan_workspace(root, current_file.clone(), config_skip_directories);
        let _ = save_graph(&graph, root);
        return graph;
    }

    // Prefer multi-bin shards when present (slim, no sources). Validate freshness.
    if let Ok(Some(mut g)) = super::shards::load_shards(root) {
        let skip_patterns = get_skip_patterns(root, config_skip_directories);
        let force_hash = std::env::var("BUTLER_FORCE_HASH_CHECK").is_ok();
        let fp = super::shards::load_sources_fingerprint(root);
        // Trusted Complete + stat fingerprint match → skip full-text rehash (leviathan open).
        //
        // Stamp at save uses inventory keys (`sources_stat_fingerprint_from_inventory`).
        // Match **the same way** first — walk-based match often misses (skip-set / extra
        // extensions) and forced content-hash + dirty thrash / FullEdge re-entry (~10–15 s
        // on vite-class reopen). Walk match remains a fallback for older stamps.
        let trust_stat = !force_hash
            && g.is_edge_build_complete()
            && !g.edges.is_empty()
            && !g.nodes.is_empty()
            && fp.as_ref().is_some_and(|f| {
                if f.path_count == 0 || f.max_mtime_ns == 0 {
                    return false;
                }
                if !g.file_hashes.is_empty() {
                    let (m, b, c) = super::sources_stat_fingerprint_from_inventory(
                        root,
                        &g.file_hashes,
                    );
                    if m == f.max_mtime_ns && b == f.total_bytes && c == f.path_count {
                        return true;
                    }
                }
                super::sources_stat_matches_manifest(
                    root,
                    &skip_patterns,
                    f.max_mtime_ns,
                    f.total_bytes,
                    f.path_count,
                )
            });
        if trust_stat {
            println!(
                "📂 Hydrate trusted Complete (stat fingerprint OK) — skip content-hash ({} blocks, {} files)",
                g.nodes.len(),
                g.file_hashes.len()
            );
            finalize_loaded_graph_state(&mut g, root);
            return g;
        }
        if !force_hash {
            if fp.is_none() {
                println!(
                    "📂 No sources_fp.bin yet — content-hash verify once, then stamp"
                );
            } else if g.is_edge_build_complete() {
                if let Some(f) = fp.as_ref() {
                    let (wm, wb, wc) =
                        super::sources_stat_fingerprint(root, &skip_patterns);
                    let (im, ib, ic) = if g.file_hashes.is_empty() {
                        (0, 0, 0)
                    } else {
                        super::sources_stat_fingerprint_from_inventory(root, &g.file_hashes)
                    };
                    println!(
                        "📂 Stat fingerprint miss — content-hash verify \
                         (stamp m/b/c={}/{}/{} inv={}/{}/{} walk={}/{}/{})",
                        f.max_mtime_ns,
                        f.total_bytes,
                        f.path_count,
                        im,
                        ib,
                        ic,
                        wm,
                        wb,
                        wc
                    );
                } else {
                    println!(
                        "📂 Stat fingerprint miss — content-hash verify (tree may have changed)"
                    );
                }
            }
        }
        let hash_t0 = Instant::now();
        let current_hashes = super::collect_source_file_hashes(root, &skip_patterns);
        println!(
            "📂 Content-hash verify: {} files in {:.1?}",
            current_hashes.len(),
            hash_t0.elapsed()
        );
        let mut dirty_paths: Vec<PathBuf> = Vec::new();
        for (pstr, &cur_h) in &current_hashes {
            if g.file_hashes.get(pstr).is_none_or(|&old| old != cur_h) {
                dirty_paths.push(PathBuf::from(pstr));
            }
        }
        for pstr in g.file_hashes.keys() {
            if !current_hashes.contains_key(pstr) {
                dirty_paths.push(PathBuf::from(pstr));
            }
        }
        dirty_paths.sort();
        dirty_paths.dedup();
        let dirty = dirty_paths.len();
        if dirty == 0 {
            // Poisoned empty Complete: skip rules once wiped inventory but stamped Complete.
            // After path-policy fixes, refuse to trust 0-node caches when sources exist.
            if g.nodes.is_empty() && !current_hashes.is_empty() {
                println!(
                    "🔄 Empty Complete cache but {} sources on disk — forcing rescan",
                    current_hashes.len()
                );
            } else {
                println!(
                    "📂 Loaded fresh multi-bin shards ({} blocks, {} files)",
                    g.nodes.len(),
                    g.file_hashes.len()
                );
                // Backfill fingerprint using **inventory keys** (same as save path) so the
                // next open hits trust_stat without walk/skip drift.
                let (m, b, c) = if !g.file_hashes.is_empty() {
                    super::sources_stat_fingerprint_from_inventory(root, &g.file_hashes)
                } else {
                    super::sources_stat_fingerprint(root, &skip_patterns)
                };
                let _ = super::shards::stamp_sources_fingerprint(root, m, b, c);
                finalize_loaded_graph_state(&mut g, root);
                return g;
            }
        } else {
            // Hop A: small dirty set on a warehouse that already has edges → dirty-cone
            // ensure only (update_files_batch + reverse-dep). Full rescan/ensure only when
            // dirty is huge, edges empty, force-full, or force-rescan env.
            let force_full = std::env::var("BUTLER_FORCE_FULL_EDGE").is_ok();
            let threshold = (g.file_hashes.len() / 3).max(50);
            let can_cone = !force_full
                && dirty <= threshold
                && !g.nodes.is_empty()
                && (!g.edges.is_empty() || g.is_edge_build_complete());
            if can_cone {
                println!(
                    "🔄 Hop A dirty cone: {} file(s) (threshold {}) — incremental edge ensure, keep warehouse",
                    dirty, threshold
                );
                let inc_start = Instant::now();
                if let Ok(mut updated) =
                    incremental_reparse(g, &dirty_paths, root, config_skip_directories)
                {
                    println!(
                        "⚡ Dirty-cone ensure done in {} ms ({} file(s))",
                        inc_start.elapsed().as_millis(),
                        dirty
                    );
                    updated.rebuild_module_hashes();
                    finalize_loaded_graph_state(&mut updated, root);
                    let _ = save_graph(&updated, root);
                    return updated;
                }
                println!(
                    "⚠️  Dirty-cone ensure failed — falling through to graph.bin / rescan"
                );
            } else {
                println!(
                    "🔄 Shard cache stale ({} dirty file hashes{}; threshold {}) — falling through to graph.bin / rescan",
                    dirty,
                    if force_full { ", FORCE_FULL_EDGE" } else { "" },
                    threshold
                );
            }
        }
    }

    if let Ok(bytes) = std::fs::read(&cache_file) {
        // Try new schema-wrapped format first
        match bincode::deserialize::<CachedGraph>(&bytes) {
            Ok(cached) => {
                if cached.version == GRAPH_SCHEMA_VERSION {
                    let mut graph = cached.graph;
                    let edge_sem_stale = cached.edge_semantics != EDGE_SEMANTICS_VERSION;
                    if edge_sem_stale {
                        // Keep AST skeleton + file/module hashes; drop edges so bg rebuild runs.
                        println!(
                            "🔄 Edge semantics version mismatch (on-disk v{} vs current v{}) — keeping nodes, forcing edge rebuild",
                            cached.edge_semantics, EDGE_SEMANTICS_VERSION
                        );
                        graph.edges.clear();
                        graph.reverse.clear();
                        graph.clear_bridges();
                        graph.files_with_edges.clear();
                        graph.background_edge_build_complete = false;
                        graph.background_edge_build_active = false;
                        graph.background_edge_build_state = BackgroundEdgeBuildState::Incomplete;
                        graph.highly_connected_nodes.clear();
                        for n in graph.nodes.values_mut() {
                            n.is_highly_connected = false;
                            n.has_cycle = false;
                            n.usages.clear();
                        }
                    }

                    // === File-level content hash delta (deterministic invalidation) ===
                    // Walk (respecting skips), hash all source files on disk, diff vs stored graph.file_hashes.
                    let hash_start = Instant::now();
                    let skip_patterns = get_skip_patterns(root, config_skip_directories);
                    let current_hashes = super::collect_source_file_hashes(root, &skip_patterns);
                    let file_count = current_hashes.len();
                    let hash_ms = hash_start.elapsed().as_millis();

                    let mut dirty: Vec<PathBuf> = vec![];
                    let mut mod_add = 0usize;
                    let mut dels = 0usize;
                    for (pstr, &cur_h) in &current_hashes {
                        if graph.file_hashes.get(pstr).is_none_or(|&old| old != cur_h) {
                            dirty.push(PathBuf::from(pstr));
                            mod_add += 1;
                        }
                    }
                    for pstr in graph.file_hashes.keys() {
                        if !current_hashes.contains_key(pstr) {
                            let p = Path::new(pstr);
                            if !p.exists() {
                                dirty.push(PathBuf::from(pstr));
                                dels += 1;
                            }
                        }
                    }
                    dirty.sort();
                    dirty.dedup();

                    println!(
                        "Hash scan of {} files took {} ms. Detected {} dirty files ({} modified/added, {} deleted).",
                        file_count, hash_ms, dirty.len(), mod_add, dels
                    );

                    if dirty.is_empty() {
                        if graph.nodes.is_empty() && !current_hashes.is_empty() {
                            println!(
                                "🔄 Empty Complete graph.bin but {} sources on disk — forcing rescan",
                                current_hashes.len()
                            );
                        } else {
                            println!(
                                "📂 Loaded fresh graph from cache ({} blocks, schema v{}, edge_sem v{})",
                                graph.nodes.len(),
                                cached.version,
                                cached.edge_semantics
                            );
                            graph.rebuild_module_hashes();
                            // Drop any legacy photocopied sources (hydrate on compose).
                            graph.strip_all_sources();
                            let (m, b, c) =
                                super::sources_stat_fingerprint(root, &skip_patterns);
                            let _ = super::shards::stamp_sources_fingerprint(root, m, b, c);
                            finalize_loaded_graph_state(&mut graph, root);
                            return graph;
                        }
                    }

                    // Practical incremental path using hash delta (modified/added/deleted)
                    if dirty.len() < (graph.nodes.len() / 3).max(50) {
                        println!("Running incremental reparse on dirty files...");
                        let inc_start = Instant::now();
                        if let Ok(mut updated) =
                            incremental_reparse(graph, &dirty, root, config_skip_directories)
                        {
                            println!(
                                "Incremental reparse completed in {} ms.",
                                inc_start.elapsed().as_millis()
                            );
                            updated.rebuild_module_hashes();
                            finalize_loaded_graph_state(&mut updated, root);
                            let _ = save_graph(&updated, root);
                            return updated;
                        }
                    }

                    println!("🔄 Cache stale — doing full rebuild...");
                } else {
                    println!(
                        "🔄 Graph cache schema version mismatch (on-disk v{} vs current v{}) — forcing full rescan",
                        cached.version, GRAPH_SCHEMA_VERSION
                    );
                    let _ = std::fs::remove_file(&cache_file);
                }
            }
            Err(_) => {
                // Old format (pre-schema-versioning) or corrupt file — treat as miss
                println!(
                    "🔄 Graph cache format unrecognized or from old schema (expected v{}) — forcing full rescan",
                    GRAPH_SCHEMA_VERSION
                );
                let _ = std::fs::remove_file(&cache_file);
            }
        }
    }
    let mut graph = scan_workspace(root, current_file, config_skip_directories);
    graph.rebuild_module_hashes();
    let _ = save_graph(&graph, root);
    // If we loaded a cached graph that already contains edges, mark it complete.
    // This prevents the fast-fail from triggering on perfectly valid disk cache loads.
    finalize_loaded_graph_state(&mut graph, root);
    graph
}

/// Hop A dirty-cone ensure: re-parse + re-edge only `dirty_files` (+ reverse dependents
/// for CALL soundness) via [`CodeGraph::update_files_batch`].
///
/// **Does not** full-tree ensure. Whole-warehouse PostPass is skipped when the warehouse
/// was Complete and the cone is tiny (see `update_files_batch`).
///
/// **Path dialect:** `dirty_files` keys are repo-relative (from `file_hashes`).
/// `update_files_batch` resolves via `ProjectPaths` under `root`.
fn incremental_reparse(
    mut graph: CodeGraph,
    dirty_files: &[std::path::PathBuf],
    root: &std::path::Path,
    _config_skip_directories: &[String],
) -> Result<CodeGraph, String> {
    if dirty_files.is_empty() {
        return Ok(graph);
    }
    println!(
        "🔄 Incremental dirty-cone reparse via update_files_batch ({} file(s))...",
        dirty_files.len()
    );
    let t0 = Instant::now();
    // update_files_batch: evict → reparse → global name maps → edges for dirty + reverse-deps.
    graph.update_files_batch(dirty_files, root, true);
    println!(
        "🔄 Incremental dirty-cone done in {} ms (dirty={})",
        t0.elapsed().as_millis(),
        dirty_files.len()
    );
    Ok(graph)
}
