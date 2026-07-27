//! Multi-bin progressive cache: inventory + symbols + edges (+ slim graph.bin).
//!
//! # Ownership (scanner package — **shards** leaf)
//! **Owns:** `SHARD_FORMAT` / `CacheManifest`, writing and reading progressive part files,
//! parallel decode of symbol/edge parts, stitch into a resident [`CodeGraph`].
//!
//! **Does not own:** Tree-sitter parse waves ([`super`] mod.rs); schema version policy and
//! hash-delta single-file load path ([`super::cache`]); edge *collection* semantics (builder).
//!
//! Avoids one mega photocopy of all sources. Assembly stitches small records.
//! Soft-freeze: do not change `SHARD_FORMAT` or part layout without a migrate plan.
//! S1 = documentation only; zero intentional behavior change.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rayon::prelude::*;

use crate::snooper::interconnect::BridgeKind;
use crate::snooper::model::{CodeGraph, Id};
use crate::snooper::project_paths::ProjectPaths;

use super::cache::{EDGE_SEMANTICS_VERSION, GRAPH_SCHEMA_VERSION};

pub const SHARD_FORMAT: u32 = 1;

/// Symbols per part file. Monolith `symbols.bin` (~1–2 GiB) forces serial bincode of one
/// blob and dominates the first 5–10 s of hydrate; parts decode in parallel.
///
/// Kept modest so mid-size repos (vite ~60k nodes) still fan out to several parts
/// instead of one 20 MiB serial decode job.
const SYMBOLS_PER_PART: usize = 12_000;
/// CALL edge pairs per part (edges.bin is smaller but still benefits on leviathans).
const EDGES_PER_PART: usize = 80_000;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct CacheManifest {
    pub format: u32,
    pub graph_schema: u32,
    pub edge_semantics: u32,
    /// L0 inventory written
    pub has_inventory: bool,
    /// L1 symbols written (may be partial)
    pub has_symbols: bool,
    /// L2 edges written (may be partial)
    pub has_edges: bool,
    /// Name → locations index written
    #[serde(default)]
    pub has_name_index: bool,
    /// Edge inventory + Complete stamp written (`edge_status.bin`)
    #[serde(default)]
    pub has_edge_status: bool,
    /// Persisted Complete (mirrors graph stamp; belt-and-suspenders for amnesia heal)
    #[serde(default)]
    pub edge_build_complete: bool,
    pub node_count: usize,
    pub edge_count: usize,
    pub file_count: usize,
}

/// Cheap sources fingerprint (stat only) — **separate file** so existing `manifest.bin`
/// (pre-fingerprint) still bincode-loads.  Missing file ⇒ force content-hash once, then stamp.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SourcesFingerprint {
    pub max_mtime_ns: u64,
    pub total_bytes: u64,
    pub path_count: u64,
}

/// Durable edge inventory + Complete (survives `serde(skip)` history on CodeGraph fields).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct EdgeStatusShard {
    pub format: u32,
    pub edge_semantics: u32,
    pub complete: bool,
    /// Repo-relative path strings
    pub files_with_edges: Vec<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct InventoryShard {
    pub format: u32,
    /// path string → content hash
    pub file_hashes: HashMap<String, u64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolRecord {
    pub id: Id,
    pub name: String,
    pub file: PathBuf,
    pub kind: String,
    pub lang: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub content_hash: String,
    pub sig_hash: String,
    pub score: f64,
    pub is_highly_connected: bool,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolsShard {
    pub format: u32,
    pub symbols: Vec<SymbolRecord>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct EdgesShard {
    pub format: u32,
    pub edges: Vec<(Id, Id)>,
}

/// Typed interconnect bridges (separate from CALL `edges.bin`).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct BridgesShard {
    pub format: u32,
    pub bridges: Vec<(Id, Id, BridgeKind)>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct NameIndexShard {
    pub format: u32,
    /// exact symbol name → all locations (no source)
    pub by_name: HashMap<String, Vec<crate::snooper::model::NameLocation>>,
}


fn cache_dir(root: &Path) -> PathBuf {
    root.join(".butler/cache")
}

/// Create cache dir only when policy allows (no `.butler` under src/examples/tests).
fn ensure_cache_dir(root: &Path) -> std::io::Result<PathBuf> {
    crate::snooper::ensure_project_butler_cache_dir(root)
}

fn edge_batches_dir(root: &Path) -> PathBuf {
    cache_dir(root).join("edge_batches")
}

/// Clear prior edge object files at the start of a full grind (fresh compile).
pub fn clear_edge_batch_objects(root: &Path) -> std::io::Result<()> {
    ensure_cache_dir(root)?;
    let dir = edge_batches_dir(root);
    if dir.is_dir() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    std::fs::create_dir_all(&dir)
}

/// Compiler-style **.o file**: immutable edge delta for one stream batch.
/// Thin-link already applied these into the live graph; objects are crash/resume gold.
pub fn write_edge_batch_object(
    root: &Path,
    batch_idx: usize,
    edges: &[(Id, Id)],
) -> std::io::Result<()> {
    if edges.is_empty() {
        return Ok(());
    }
    ensure_cache_dir(root)?;
    let dir = edge_batches_dir(root);
    std::fs::create_dir_all(&dir)?;
    let ed = EdgesShard {
        format: SHARD_FORMAT,
        edges: edges.to_vec(),
    };
    let path = dir.join(format!("batch_{batch_idx:05}.bin"));
    std::fs::write(
        &path,
        bincode::serialize(&ed).map_err(std::io::Error::other)?,
    )?;
    Ok(())
}

/// Write inventory + symbols + edges + manifest (sources never stored).
pub fn save_shards(graph: &CodeGraph, root: &Path) -> std::io::Result<()> {
    let dir = ensure_cache_dir(root)?;

    let inv = InventoryShard {
        format: SHARD_FORMAT,
        file_hashes: graph.file_hashes.clone(),
    };
    std::fs::write(
        dir.join("inventory.bin"),
        bincode::serialize(&inv).map_err(std::io::Error::other)?,
    )?;

    let symbols: Vec<SymbolRecord> = graph
        .nodes
        .values()
        .map(|b| SymbolRecord {
            id: b.id.clone(),
            name: b.name.clone(),
            file: b.file.clone(),
            kind: b.kind.clone(),
            lang: b.lang.clone(),
            start_line: b.start_line,
            end_line: b.end_line,
            start_byte: b.start_byte,
            end_byte: b.end_byte,
            content_hash: b.content_hash.clone(),
            sig_hash: b.sig_hash.clone(),
            score: b.score,
            is_highly_connected: b.is_highly_connected,
        })
        .collect();
    write_symbol_parts(&dir, symbols)?;

    let mut edge_list = Vec::new();
    for (from, tos) in &graph.edges {
        for to in tos {
            edge_list.push((from.clone(), to.clone()));
        }
    }
    let edge_count = edge_list.len();
    write_edge_parts(&dir, edge_list)?;

    // Typed bridges (separate file so old edges.bin still loads).
    let mut bridge_list: Vec<(Id, Id, BridgeKind)> = Vec::new();
    for (from, tos) in &graph.bridge_fwd {
        for (to, kind) in tos {
            bridge_list.push((from.clone(), to.clone(), *kind));
        }
    }
    if !bridge_list.is_empty() {
        let br = BridgesShard {
            format: SHARD_FORMAT,
            bridges: bridge_list,
        };
        std::fs::write(
            dir.join("bridges.bin"),
            bincode::serialize(&br).map_err(std::io::Error::other)?,
        )?;
    } else {
        let _ = std::fs::remove_file(dir.join("bridges.bin"));
    }

    // Name index (rg-shaped exact lookup). Rebuild from nodes if empty (no full graph clone).
    let by_name = if !graph.name_index.is_empty() {
        graph.name_index.clone()
    } else {
        let mut idx: HashMap<String, Vec<crate::snooper::model::NameLocation>> = HashMap::new();
        for b in graph.nodes.values() {
            if b.name.is_empty() {
                continue;
            }
            idx.entry(b.name.clone()).or_default().push(
                crate::snooper::model::NameLocation {
                    id: b.id.clone(),
                    name: b.name.clone(),
                    file: b.file.clone(),
                    start_line: b.start_line,
                    end_line: b.end_line,
                    kind: b.kind.clone(),
                    lang: b.lang.clone(),
                },
            );
        }
        idx
    };
    let ni = NameIndexShard {
        format: SHARD_FORMAT,
        by_name: by_name.clone(),
    };
    std::fs::write(
        dir.join("name_index.bin"),
        bincode::serialize(&ni).map_err(std::io::Error::other)?,
    )?;

    // Edge status: inventory + Complete stamp (prevents false FullEdge on every boot).
    let edge_status = EdgeStatusShard {
        format: SHARD_FORMAT,
        edge_semantics: EDGE_SEMANTICS_VERSION,
        complete: graph.is_edge_build_complete(),
        files_with_edges: graph
            .files_with_edges
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect(),
    };
    std::fs::write(
        dir.join("edge_status.bin"),
        bincode::serialize(&edge_status).map_err(std::io::Error::other)?,
    )?;

    let man = CacheManifest {
        format: SHARD_FORMAT,
        graph_schema: GRAPH_SCHEMA_VERSION,
        edge_semantics: EDGE_SEMANTICS_VERSION,
        has_inventory: true,
        has_symbols: true,
        has_edges: !graph.edges.is_empty(),
        has_name_index: !by_name.is_empty(),
        has_edge_status: true,
        edge_build_complete: edge_status.complete,
        node_count: graph.nodes.len(),
        edge_count,
        file_count: graph.file_hashes.len(),
    };
    std::fs::write(
        dir.join("manifest.bin"),
        bincode::serialize(&man).map_err(std::io::Error::other)?,
    )?;

    // Stat fingerprint sidecar (does not alter manifest.bin layout).
    let (src_mtime, src_bytes, src_count) =
        super::sources_stat_fingerprint_from_inventory(root, &graph.file_hashes);
    let _ = stamp_sources_fingerprint(root, src_mtime, src_bytes, src_count);

    println!(
        "💾 Shards saved under {} (nodes={} edges={} files={} complete={} fp_paths={})",
        dir.display(),
        man.node_count,
        man.edge_count,
        man.file_count,
        man.edge_build_complete,
        src_count
    );
    Ok(())
}

/// True if progressive multi-bin cache is present.
pub fn shards_exist(root: impl AsRef<Path>) -> bool {
    let d = cache_dir(root.as_ref());
    d.join("manifest.bin").is_file() && d.join("symbols.bin").is_file()
}

/// After a clean content-hash verify, write sources fingerprint sidecar so the next
/// open can skip full-text rehash. No graph / manifest rewrite.
pub fn stamp_sources_fingerprint(
    root: impl AsRef<Path>,
    max_mtime_ns: u64,
    total_bytes: u64,
    path_count: u64,
) -> std::io::Result<()> {
    let root = root.as_ref();
    let dir = ensure_cache_dir(root)?;
    let fp = SourcesFingerprint {
        max_mtime_ns,
        total_bytes,
        path_count,
    };
    std::fs::write(
        dir.join("sources_fp.bin"),
        bincode::serialize(&fp).map_err(std::io::Error::other)?,
    )?;
    println!(
        "📂 Stamped sources fingerprint (paths={path_count}, bytes={total_bytes})"
    );
    Ok(())
}

/// Load sidecar fingerprint if present.
pub fn load_sources_fingerprint(root: impl AsRef<Path>) -> Option<SourcesFingerprint> {
    let path = cache_dir(root.as_ref()).join("sources_fp.bin");
    if !path.is_file() {
        return None;
    }
    bincode::deserialize(&std::fs::read(path).ok()?).ok()
}

/// Read cache manifest if present and schema-compatible (for hydrate trust checks).
pub fn load_manifest(root: impl AsRef<Path>) -> Option<CacheManifest> {
    let dir = cache_dir(root.as_ref());
    let man_path = dir.join("manifest.bin");
    if !man_path.is_file() {
        return None;
    }
    let man: CacheManifest = bincode::deserialize(&std::fs::read(man_path).ok()?).ok()?;
    if man.graph_schema != GRAPH_SCHEMA_VERSION {
        return None;
    }
    Some(man)
}

/// Write symbols as parallel-friendly part files; drop legacy monolith.
fn write_symbol_parts(dir: &Path, symbols: Vec<SymbolRecord>) -> std::io::Result<()> {
    clear_named_parts(dir, "symbols_part_")?;
    let _ = std::fs::remove_file(dir.join("symbols.bin"));
    if symbols.is_empty() {
        // Keep empty monolith so older tools still see a symbols path.
        let sym = SymbolsShard {
            format: SHARD_FORMAT,
            symbols: vec![],
        };
        std::fs::write(
            dir.join("symbols.bin"),
            bincode::serialize(&sym).map_err(std::io::Error::other)?,
        )?;
        return Ok(());
    }
    let n_parts = symbols.len().div_ceil(SYMBOLS_PER_PART);
    println!(
        "💾 Writing {} symbol part(s) ({} symbols, {} / part)",
        n_parts,
        symbols.len(),
        SYMBOLS_PER_PART
    );
    symbols
        .chunks(SYMBOLS_PER_PART)
        .enumerate()
        .try_for_each(|(i, chunk)| {
            let sym = SymbolsShard {
                format: SHARD_FORMAT,
                symbols: chunk.to_vec(),
            };
            std::fs::write(
                dir.join(format!("symbols_part_{i:05}.bin")),
                bincode::serialize(&sym).map_err(std::io::Error::other)?,
            )
        })?;
    Ok(())
}

fn write_edge_parts(dir: &Path, edges: Vec<(Id, Id)>) -> std::io::Result<()> {
    clear_named_parts(dir, "edges_part_")?;
    let _ = std::fs::remove_file(dir.join("edges.bin"));
    if edges.is_empty() {
        let ed = EdgesShard {
            format: SHARD_FORMAT,
            edges: vec![],
        };
        std::fs::write(
            dir.join("edges.bin"),
            bincode::serialize(&ed).map_err(std::io::Error::other)?,
        )?;
        return Ok(());
    }
    let n_parts = edges.len().div_ceil(EDGES_PER_PART);
    if n_parts == 1 {
        // Single part — keep classic name for tiny graphs.
        let ed = EdgesShard {
            format: SHARD_FORMAT,
            edges,
        };
        std::fs::write(
            dir.join("edges.bin"),
            bincode::serialize(&ed).map_err(std::io::Error::other)?,
        )?;
        return Ok(());
    }
    println!(
        "💾 Writing {n_parts} edge part(s) ({} edges, {} / part)",
        edges.len(),
        EDGES_PER_PART
    );
    edges
        .chunks(EDGES_PER_PART)
        .enumerate()
        .try_for_each(|(i, chunk)| {
            let ed = EdgesShard {
                format: SHARD_FORMAT,
                edges: chunk.to_vec(),
            };
            std::fs::write(
                dir.join(format!("edges_part_{i:05}.bin")),
                bincode::serialize(&ed).map_err(std::io::Error::other)?,
            )
        })?;
    Ok(())
}

fn clear_named_parts(dir: &Path, prefix: &str) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for ent in std::fs::read_dir(dir)? {
        let ent = ent?;
        let name = ent.file_name();
        let s = name.to_string_lossy();
        if s.starts_with(prefix) && s.ends_with(".bin") {
            let _ = std::fs::remove_file(ent.path());
        }
    }
    Ok(())
}

fn list_parts(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let name = ent.file_name();
        let s = name.to_string_lossy();
        if s.starts_with(prefix) && s.ends_with(".bin") {
            out.push(ent.path());
        }
    }
    out.sort();
    out
}

/// Rebuild monolith files from parts (compat for servers that only read `symbols.bin`).
pub fn merge_parts_to_monolith(root: impl AsRef<Path>) -> std::io::Result<(usize, usize)> {
    let dir = cache_dir(root.as_ref());
    let mut n_sym = 0usize;
    let mut n_edge = 0usize;
    let sym_parts = list_parts(&dir, "symbols_part_");
    if !sym_parts.is_empty() {
        let mut symbols = Vec::new();
        for p in &sym_parts {
            let sym: SymbolsShard = bincode::deserialize(&std::fs::read(p)?)
                .map_err(std::io::Error::other)?;
            symbols.extend(sym.symbols);
        }
        n_sym = symbols.len();
        let shard = SymbolsShard {
            format: SHARD_FORMAT,
            symbols,
        };
        std::fs::write(
            dir.join("symbols.bin"),
            bincode::serialize(&shard).map_err(std::io::Error::other)?,
        )?;
        println!("📂 Merged {n_sym} symbols → symbols.bin (compat monolith)");
    }
    let edge_parts = list_parts(&dir, "edges_part_");
    if !edge_parts.is_empty() {
        let mut edges = Vec::new();
        for p in &edge_parts {
            let ed: EdgesShard = bincode::deserialize(&std::fs::read(p)?)
                .map_err(std::io::Error::other)?;
            edges.extend(ed.edges);
        }
        n_edge = edges.len();
        let shard = EdgesShard {
            format: SHARD_FORMAT,
            edges,
        };
        std::fs::write(
            dir.join("edges.bin"),
            bincode::serialize(&shard).map_err(std::io::Error::other)?,
        )?;
        println!("📂 Merged {n_edge} edges → edges.bin (compat monolith)");
    }
    Ok((n_sym, n_edge))
}

/// One-shot: split legacy monolith `symbols.bin` / `edges.bin` into parts for parallel hydrate.
/// Safe to call on an already-parted cache (no-op).
pub fn rechunk_monolith_shards(root: impl AsRef<Path>) -> std::io::Result<(usize, usize)> {
    let root = root.as_ref();
    let dir = cache_dir(root);
    let mut n_sym = 0usize;
    let mut n_edge = 0usize;

    let sym_parts = list_parts(&dir, "symbols_part_");
    let sym_mono = dir.join("symbols.bin");
    if sym_parts.is_empty() && sym_mono.is_file() {
        let t0 = Instant::now();
        let bytes = std::fs::read(&sym_mono)?;
        let sym: SymbolsShard =
            bincode::deserialize(&bytes).map_err(std::io::Error::other)?;
        n_sym = sym.symbols.len();
        if n_sym > SYMBOLS_PER_PART {
            println!(
                "📂 Rechunk symbols monolith → parts ({} symbols, {:.1?})…",
                n_sym,
                t0.elapsed()
            );
            write_symbol_parts(&dir, sym.symbols)?;
        }
    }

    let edge_parts = list_parts(&dir, "edges_part_");
    let edge_mono = dir.join("edges.bin");
    if edge_parts.is_empty() && edge_mono.is_file() {
        let t0 = Instant::now();
        let bytes = std::fs::read(&edge_mono)?;
        let ed: EdgesShard = bincode::deserialize(&bytes).map_err(std::io::Error::other)?;
        n_edge = ed.edges.len();
        if n_edge > EDGES_PER_PART {
            println!(
                "📂 Rechunk edges monolith → parts ({} edges, {:.1?})…",
                n_edge,
                t0.elapsed()
            );
            write_edge_parts(&dir, ed.edges)?;
        }
    }

    Ok((n_sym, n_edge))
}

/// Load graph from shards (symbols + edges + inventory). Sources empty.
///
/// Part files (`symbols_part_*`, `edges_part_*`) + other bins are **read + decoded in
/// parallel**. Legacy monolith `symbols.bin` / `edges.bin` still load (serial on that blob).
pub fn load_shards(root: impl AsRef<Path>) -> std::io::Result<Option<CodeGraph>> {
    let root = root.as_ref();
    let dir = cache_dir(root);
    let man_path = dir.join("manifest.bin");
    if !man_path.is_file() {
        return Ok(None);
    }
    let t0 = Instant::now();
    let man: CacheManifest = bincode::deserialize(&std::fs::read(&man_path)?)
        .map_err(std::io::Error::other)?;
    if man.graph_schema != GRAPH_SCHEMA_VERSION {
        println!(
            "📂 Shard schema mismatch (disk v{} vs {}), ignoring shards",
            man.graph_schema, GRAPH_SCHEMA_VERSION
        );
        return Ok(None);
    }
    // Edge logic version is independent of node schema. Stale edges must not load as
    // "already edged" or bg/JIT will skip recollect and keep polluted polyglot CALL links.
    let edge_sem_stale = man.edge_semantics != EDGE_SEMANTICS_VERSION;
    if edge_sem_stale {
        println!(
            "🔄 Shard edge_sem mismatch (disk v{} vs current v{}) — loading nodes, dropping edges",
            man.edge_semantics, EDGE_SEMANTICS_VERSION
        );
    }

    // Build parallel job list: fixed bins + symbol/edge parts (or monolith fallback).
    let mut jobs: Vec<(String, PathBuf)> = Vec::new();
    for name in [
        "inventory.bin",
        "bridges.bin",
        "edge_status.bin",
        "name_index.bin",
    ] {
        let p = dir.join(name);
        if p.is_file() {
            jobs.push((name.to_string(), p));
        }
    }
    let sym_parts = list_parts(&dir, "symbols_part_");
    if !sym_parts.is_empty() {
        for p in sym_parts {
            jobs.push((
                format!("symbols_part:{}", p.file_name().unwrap_or_default().to_string_lossy()),
                p,
            ));
        }
    } else {
        let p = dir.join("symbols.bin");
        if p.is_file() {
            jobs.push(("symbols.bin".into(), p));
        }
    }
    let edge_parts = list_parts(&dir, "edges_part_");
    if !edge_parts.is_empty() {
        for p in edge_parts {
            jobs.push((
                format!("edges_part:{}", p.file_name().unwrap_or_default().to_string_lossy()),
                p,
            ));
        }
    } else {
        let p = dir.join("edges.bin");
        if p.is_file() {
            jobs.push(("edges.bin".into(), p));
        }
    }

    let n_jobs = jobs.len();
    let decoded: Vec<(String, DecodedShard)> = jobs
        .into_par_iter()
        .filter_map(|(label, path)| {
            let bytes = std::fs::read(&path).ok()?;
            let dec = if label == "inventory.bin" {
                bincode::deserialize::<InventoryShard>(&bytes)
                    .ok()
                    .map(DecodedShard::Inventory)
            } else if label == "symbols.bin" || label.starts_with("symbols_part:") {
                bincode::deserialize::<SymbolsShard>(&bytes)
                    .ok()
                    .map(DecodedShard::Symbols)
            } else if label == "edges.bin" || label.starts_with("edges_part:") {
                bincode::deserialize::<EdgesShard>(&bytes)
                    .ok()
                    .map(DecodedShard::Edges)
            } else if label == "bridges.bin" {
                bincode::deserialize::<BridgesShard>(&bytes)
                    .ok()
                    .map(DecodedShard::Bridges)
            } else if label == "edge_status.bin" {
                bincode::deserialize::<EdgeStatusShard>(&bytes)
                    .ok()
                    .map(DecodedShard::EdgeStatus)
            } else if label == "name_index.bin" {
                bincode::deserialize::<NameIndexShard>(&bytes)
                    .ok()
                    .map(DecodedShard::NameIndex)
            } else {
                None
            }?;
            Some((label, dec))
        })
        .collect();

    let mut graph = CodeGraph::new();
    let pp = ProjectPaths::new(root);
    let t_io = t0.elapsed();

    let mut symbol_shards: Vec<SymbolsShard> = Vec::new();
    let mut edge_shards: Vec<EdgesShard> = Vec::new();
    let mut inventory: Option<InventoryShard> = None;
    let mut bridges: Option<BridgesShard> = None;
    let mut edge_status: Option<EdgeStatusShard> = None;
    let mut name_index: Option<NameIndexShard> = None;

    for (label, dec) in decoded {
        match dec {
            DecodedShard::Inventory(inv) => inventory = Some(inv),
            DecodedShard::Symbols(sym) => symbol_shards.push(sym),
            DecodedShard::Edges(ed) => edge_shards.push(ed),
            DecodedShard::Bridges(br) => bridges = Some(br),
            DecodedShard::EdgeStatus(st) => edge_status = Some(st),
            DecodedShard::NameIndex(ni) => name_index = Some(ni),
        }
        let _ = label;
    }

    if let Some(inv) = inventory {
        let mut fh = std::collections::HashMap::with_capacity(inv.file_hashes.len());
        for (k, v) in inv.file_hashes {
            fh.insert(pp.key(k), v);
        }
        graph.file_hashes = fh;
    }

    // Parallel record→BlockInfo, then sequential insert (HashMap not concurrent).
    let t_sym = Instant::now();
    let all_records: Vec<SymbolRecord> = symbol_shards
        .into_iter()
        .flat_map(|s| s.symbols)
        .collect();
    let total_sym = all_records.len();
    graph.nodes.reserve(total_sym);
    let blocks: Vec<crate::snooper::model::BlockInfo> = all_records
        .into_par_iter()
        .map(|s| symbol_record_to_block(&pp, s))
        .collect();
    for b in blocks {
        if b.is_highly_connected {
            graph.highly_connected_nodes.insert(b.id.clone());
        }
        graph.nodes.insert(b.id.clone(), b);
    }
    let t_sym_done = t_sym.elapsed();

    if !edge_sem_stale {
        let mut all_edges: Vec<(Id, Id)> = Vec::new();
        for ed in edge_shards {
            all_edges.extend(ed.edges);
        }
        if !all_edges.is_empty() {
            graph.add_edges_batch_vec(all_edges);
        }
        if let Some(br) = bridges {
            graph.add_bridge_edges_batch(br.bridges);
        }
        // Prefer durable inventory + Complete stamp over endpoint approximation.
        if let Some(st) = edge_status {
            if st.edge_semantics == EDGE_SEMANTICS_VERSION {
                graph.files_with_edges = st
                    .files_with_edges
                    .into_iter()
                    .map(PathBuf::from)
                    .collect();
                if st.complete {
                    graph.background_edge_build_complete = true;
                    graph.background_edge_build_state =
                        crate::snooper::model::BackgroundEdgeBuildState::Complete;
                }
            }
        } else if man.has_edge_status && man.edge_build_complete {
            // Manifest-only Complete (status blob missing) — still prefer stamp over resuscitate.
            graph.background_edge_build_complete = true;
            graph.background_edge_build_state =
                crate::snooper::model::BackgroundEdgeBuildState::Complete;
        } else if man.has_edges && !graph.edges.is_empty() && graph.files_with_edges.is_empty() {
            // Legacy: approximate from adjacency only when no status shard.
            graph.reconstruct_files_with_edges_from_adjacency();
        }
    }

    graph.rebuild_module_hashes();
    if let Some(ni) = name_index {
        graph.name_index = ni.by_name;
    }
    if graph.name_index.is_empty() {
        graph.rebuild_name_index();
    } else {
        // Stamp load so O(1) stale check works; full audit on finalize_loaded_graph_state.
        graph.stamp_name_index_after_load();
    }

    if edge_sem_stale {
        // Force full edge recollect (call + structural FFI) under current semantics.
        graph.edges.clear();
        graph.reverse.clear();
        graph.clear_bridges();
        graph.files_with_edges.clear();
        graph.background_edge_build_complete = false;
        graph.background_edge_build_active = false;
        graph.background_edge_build_state =
            crate::snooper::model::BackgroundEdgeBuildState::Incomplete;
    } else {
        graph.restore_edge_build_state_after_load(true);
    }

    println!(
        "📂 Loaded shards: {} nodes, {} edge-sources, {} files, {} name keys, complete={} ({} jobs; parallel io+decode {:.1?}; nodes assemble {:.1?}; total {:.1?})",
        graph.nodes.len(),
        graph.edges.len(),
        graph.file_hashes.len(),
        graph.name_index.len(),
        graph.is_edge_build_complete(),
        n_jobs,
        t_io,
        t_sym_done,
        t0.elapsed()
    );
    Ok(Some(graph))
}

fn symbol_record_to_block(
    pp: &ProjectPaths,
    s: SymbolRecord,
) -> crate::snooper::model::BlockInfo {
    let file = pp.to_rel(&s.file);
    crate::snooper::model::BlockInfo {
        id: s.id,
        name: s.name,
        file,
        kind: s.kind,
        lang: s.lang,
        start_line: s.start_line,
        end_line: s.end_line,
        start_byte: s.start_byte,
        end_byte: s.end_byte,
        parent_id: None,
        children: Vec::new(),
        content_hash: s.content_hash,
        sig_hash: s.sig_hash,
        git_blame_recency: None,
        git_author: None,
        score: s.score,
        has_cycle: false,
        is_macro_expanded: false,
        source: String::new(),
        usages: vec![],
        external_crates: Default::default(),
        is_highly_connected: s.is_highly_connected,
    }
}

/// Parallel-decoded shard payload (owned so rayon workers transfer cleanly).
enum DecodedShard {
    Inventory(InventoryShard),
    Symbols(SymbolsShard),
    Edges(EdgesShard),
    Bridges(BridgesShard),
    EdgeStatus(EdgeStatusShard),
    NameIndex(NameIndexShard),
}

