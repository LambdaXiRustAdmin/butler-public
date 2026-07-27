//! FullEdge memory tier + batch budgeting (Thesis B1 peel).
//!
//! Graph-aware RAM tiers, rayon pool sizing, path-island locality, take_edge_batch.
//! Tuned constants: see `plans/edge_mem_tiers.md` — **do not retune under peels**.
//! Zero intentional behavior change.

use std::path::{Path, PathBuf};

use rayon::ThreadPoolBuilder;

pub(crate) const MIB: u64 = 1024 * 1024;
pub(crate) const GIB: u64 = 1024 * MIB;

/// Rough peak per edge worker (source + Tree-sitter AST + query temps).
const EDGE_RAM_PER_WORKER: u64 = 64 * MIB;

/// Linux MemAvailable; non-Linux / failure → `None` (callers assume 8 GiB design point).
fn mem_available_bytes() -> Option<u64> {
    let ok = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in ok.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

fn mem_total_bytes() -> Option<u64> {
    let ok = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in ok.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

/// Current process resident set (Linux `/proc/self/status` VmRSS).
pub(crate) fn process_rss_bytes() -> Option<u64> {
    let ok = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in ok.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

/// Design point when /proc is missing: consumer 8 GiB class.
pub(crate) fn mem_budget_bytes() -> u64 {
    mem_available_bytes().unwrap_or(8 * GIB)
}

/// **Graph-aware** RAM tier for FullEdge thread/batch sizing.
///
/// After a leviathan skeleton install (pytorch ~5 GiB RSS), `MemAvailable` collapses to a
/// "tiny laptop" number even on 32–64 GiB hosts. That incorrectly caps workers at 2–4.
/// Already-resident process RSS is treated as **sunk graph cost** (not missing free RAM);
/// concurrent parse peaks still scale from raw `MemAvailable` via [`edge_pool_threads`].
///
/// `effective = min(avail + rss, total×0.9)`, never below raw avail.
///
/// Tuned constants / tier table: see `plans/edge_mem_tiers.md` (do not random-edit).
pub(crate) fn edge_mem_tier_bytes() -> u64 {
    let avail = mem_budget_bytes();
    let rss = process_rss_bytes().unwrap_or(0);
    let total = mem_total_bytes().unwrap_or(avail.saturating_add(rss));
    let graph_aware = avail.saturating_add(rss);
    let cap = total.saturating_mul(9) / 10;
    graph_aware.min(cap).max(avail)
}

/// Tier caps from a byte budget (shared by threads + batch sizing).
pub(crate) fn edge_tier_caps(
    tier_bytes: u64,
) -> (usize /* thread_cap */, usize /* max_files */, u64 /* max_bytes */) {
    if tier_bytes < 2 * GIB {
        (2, 16, 2 * MIB)
    } else if tier_bytes < 4 * GIB {
        (4, 40, 6 * MIB)
    } else if tier_bytes < 8 * GIB {
        (6, 96, 16 * MIB)
    } else if tier_bytes < 16 * GIB {
        (10, 160, 28 * MIB)
    } else {
        (usize::MAX, 256, 48 * MIB)
    }
}

fn edge_tier_ceiling_caps(tier_bytes: u64) -> (usize, u64) {
    if tier_bytes < 2 * GIB {
        (24, 3 * MIB)
    } else if tier_bytes < 4 * GIB {
        (64, 10 * MIB)
    } else if tier_bytes < 8 * GIB {
        (128, 24 * MIB)
    } else if tier_bytes < 16 * GIB {
        (192, 40 * MIB)
    } else {
        (320, 64 * MIB)
    }
}

/// Rayon pool size. `edge_threads` 0 = all logical CPUs.
///
/// - **Worker peak** (by_ram): raw `MemAvailable` — do not OOM on parse spikes.
/// - **Tier cap**: graph-aware budget so post-install free-RAM collapse does not
///   force a 4-thread crawl on a machine that already holds the graph.
pub(crate) fn edge_pool_threads(edge_threads: usize) -> usize {
    let requested = if edge_threads > 0 {
        edge_threads
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(1)
    };
    // Optional hard override for known big-RAM hosts (ops escape hatch).
    if let Ok(v) = std::env::var("BUTLER_EDGE_THREADS") {
        if let Ok(n) = v.trim().parse::<usize>() {
            if n >= 1 {
                return n;
            }
        }
    }
    let avail = mem_budget_bytes();
    let tier = edge_mem_tier_bytes();
    let rss = process_rss_bytes().unwrap_or(0);
    // Spend at most ~1/4 of *free-ish* RAM on concurrent parse peaks.
    let by_ram = ((avail / 4) / EDGE_RAM_PER_WORKER).max(1) as usize;
    let (tier_cap, _, _) = edge_tier_caps(tier);
    // After a large graph install, free RAM alone under-counts the host. Raise a
    // modest floor when we already hold the warehouse and free is still ≥1 GiB.
    let floor = if rss >= GIB && avail >= 2 * GIB {
        requested.min(6)
    } else if rss >= GIB && avail >= GIB {
        requested.min(4)
    } else {
        1
    };
    requested
        .min(tier_cap)
        .min(by_ram.max(floor))
        .max(1)
}

/// File-count + source-byte caps for one stream-merge batch.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EdgeBatchBudget {
    pub max_files: usize,
    pub max_bytes: u64,
}

pub(crate) fn edge_batch_budget(threads: usize) -> EdgeBatchBudget {
    let tier = edge_mem_tier_bytes();
    let (_, max_files, max_bytes) = edge_tier_caps(tier);
    // Feed the pool: at least ~2× threads files so workers stay busy between merges.
    let max_files = max_files.max(threads.saturating_mul(2).min(64)).max(1);
    EdgeBatchBudget {
        max_files,
        max_bytes: max_bytes.max(512 * 1024),
    }
}

/// Soft ceiling for adaptive batch grow (never above this regardless of speed).
pub(crate) fn edge_batch_budget_ceiling(threads: usize) -> EdgeBatchBudget {
    let tier = edge_mem_tier_bytes();
    let (max_files, max_bytes) = edge_tier_ceiling_caps(tier);
    EdgeBatchBudget {
        max_files: max_files.max(threads.saturating_mul(2)).max(1),
        max_bytes: max_bytes.max(MIB),
    }
}

/// First two path components → island key (`dom/base`, `torch/csrc`). Locality for batches
/// without package-marker I/O (marker islands are P1+/P2).
pub(crate) fn edge_island_key(p: &Path) -> String {
    let mut parts = p.components().filter_map(|c| match c {
        std::path::Component::Normal(s) => Some(s.to_string_lossy()),
        _ => None,
    });
    let a = parts.next().unwrap_or_default();
    let b = parts.next().unwrap_or_default();
    if b.is_empty() {
        a.into_owned()
    } else {
        format!("{a}/{b}")
    }
}

pub(crate) fn sort_files_for_edge_locality(files: &mut [PathBuf]) {
    files.sort_by(|a, b| {
        edge_island_key(a)
            .cmp(&edge_island_key(b))
            .then_with(|| a.cmp(b))
    });
}

/// Take the next byte/file-budgeted batch starting at `start`. Always progresses (≥1 file).
pub(crate) fn take_edge_batch(
    files: &[PathBuf],
    start: usize,
    root: &Path,
    budget: EdgeBatchBudget,
) -> (usize, Vec<PathBuf>) {
    if start >= files.len() {
        return (start, Vec::new());
    }
    let mut batch = Vec::new();
    let mut bytes: u64 = 0;
    let mut i = start;
    while i < files.len() && batch.len() < budget.max_files {
        let p = &files[i];
        let abs = super::project_paths::ProjectPaths::new(root).to_abs(p);
        let sz = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(4 * 1024);
        if !batch.is_empty() && bytes.saturating_add(sz) > budget.max_bytes {
            break;
        }
        batch.push(p.clone());
        bytes = bytes.saturating_add(sz);
        i += 1;
        // One huge generated file still gets its own batch.
        if sz > budget.max_bytes {
            break;
        }
    }
    if batch.is_empty() {
        batch.push(files[start].clone());
        i = start + 1;
    }
    (i, batch)
}

pub(crate) fn get_bounded_edge_pool(edge_threads: usize) -> rayon::ThreadPool {
    let threads = edge_pool_threads(edge_threads);
    ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("butler-edge-{i}"))
        .stack_size(32 * 1024 * 1024)
        .build()
        .expect("rayon pool build failed")
}

#[cfg(test)]
mod edge_budget_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn island_key_uses_first_two_components() {
        assert_eq!(
            edge_island_key(Path::new("dom/base/nsFoo.cpp")),
            "dom/base"
        );
        assert_eq!(edge_island_key(Path::new("main.rs")), "main.rs");
        assert_eq!(
            edge_island_key(Path::new("torch/csrc/jit/runtime.cpp")),
            "torch/csrc"
        );
    }

    #[test]
    fn sort_locality_groups_islands() {
        let mut files = vec![
            PathBuf::from("z/pkg/a.rs"),
            PathBuf::from("a/pkg/b.rs"),
            PathBuf::from("a/pkg/c.rs"),
            PathBuf::from("z/other/d.rs"),
        ];
        sort_files_for_edge_locality(&mut files);
        assert_eq!(files[0], PathBuf::from("a/pkg/b.rs"));
        assert_eq!(files[1], PathBuf::from("a/pkg/c.rs"));
        assert_eq!(edge_island_key(&files[2]), "z/other");
    }

    #[test]
    fn take_edge_batch_respects_byte_budget() {
        let dir = std::env::temp_dir().join(format!(
            "butler_edge_batch_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut names = Vec::new();
        for i in 0..5 {
            let name = format!("f{i}.rs");
            let mut f = std::fs::File::create(dir.join(&name)).unwrap();
            // ~1 KiB each
            f.write_all(&vec![b'x'; 1024]).unwrap();
            names.push(PathBuf::from(name));
        }
        let budget = EdgeBatchBudget {
            max_files: 10,
            max_bytes: 2500, // ~2.5 files
        };
        let (next, batch) = take_edge_batch(&names, 0, &dir, budget);
        assert!(batch.len() >= 2 && batch.len() <= 3, "batch={:?}", batch.len());
        assert_eq!(next, batch.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edge_pool_threads_at_least_one() {
        assert!(edge_pool_threads(0) >= 1);
        assert!(edge_pool_threads(1) >= 1);
        assert!(edge_pool_threads(64) >= 1);
    }

    #[test]
    fn edge_mem_tier_at_least_available() {
        let avail = mem_budget_bytes();
        let tier = edge_mem_tier_bytes();
        assert!(
            tier >= avail,
            "graph-aware tier ({tier}) must not undercut MemAvailable ({avail})"
        );
    }

    #[test]
    fn edge_tier_caps_monotonic() {
        let (_, f2, b2) = edge_tier_caps(2 * GIB - 1);
        let (_, f4, b4) = edge_tier_caps(4 * GIB - 1);
        let (_, f8, b8) = edge_tier_caps(8 * GIB - 1);
        assert!(f2 <= f4 && f4 <= f8);
        assert!(b2 <= b4 && b4 <= b8);
    }
}
