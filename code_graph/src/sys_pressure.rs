//! Host memory / load pressure for admission control and thread caps.
//!
//! Prevents user-invoked OOM on stressed systems (e.g. leviathan open while an LLM
//! already holds most of RAM). Prefer degrade / defer over crash.
//!
//! Linux: `/proc/meminfo`, `/proc/self/status`. Non-Linux: conservative Green defaults.

/// 1 GiB in bytes.
const GIB: u64 = 1 << 30;
/// 1 MiB in bytes.
const MIB: u64 = 1 << 20;

/// Coarse host pressure tier (higher = more constrained).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PressureTier {
    /// Plenty of free RAM — full speed.
    Green = 0,
    /// Tight but workable — throttle threads / prefer cache.
    Yellow = 1,
    /// Dangerous for cold full-tree work — min threads, scope or defer cold scans.
    Red = 2,
    /// Refuse new heavy work (cold multi‑k file scan / multi‑GiB cache install).
    Black = 3,
}

impl PressureTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Red => "red",
            Self::Black => "black",
        }
    }
}

/// Point-in-time host + process pressure sample.
#[derive(Debug, Clone, Copy)]
pub struct PressureSnapshot {
    pub mem_available: u64,
    pub mem_total: u64,
    pub self_rss: u64,
    pub tier: PressureTier,
}

impl PressureSnapshot {
    pub fn mem_available_mb(self) -> u64 {
        self.mem_available / MIB
    }

    pub fn self_rss_mb(self) -> u64 {
        self.self_rss / MIB
    }

    /// Compact log / progress line.
    pub fn summary_line(self) -> String {
        format!(
            "tier={} avail≈{}MiB rss≈{}MiB total≈{}MiB",
            self.tier.as_str(),
            self.mem_available_mb(),
            self.self_rss_mb(),
            self.mem_total / MIB
        )
    }
}

/// Whether a heavy warehouse open (cold scan or multi‑GiB cache load) may start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitDecision {
    /// Proceed (optionally with a hard thread cap).
    Allow { max_scan_threads: usize },
    /// Do not start; leave a retryable pressure hold for the agent.
    Defer { reason: String },
}

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

fn process_rss_bytes() -> Option<u64> {
    let ok = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in ok.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

/// Optional override from env (`BUTLER_PRESSURE_BLACK_MB`, etc.), else `default`.
fn env_mb(name: &str, default_mib: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default_mib)
        .saturating_mul(MIB)
}

/// Classify from absolute free-ish RAM (and total for small hosts).
///
/// Tuned for **co-tenant** hosts (LLM + Butler): defer only when truly critical.
/// Defaults (overridable via env):
/// - black: &lt; 256 MiB (`BUTLER_PRESSURE_BLACK_MB`)
/// - red:   &lt; 768 MiB (`BUTLER_PRESSURE_RED_MB`)
/// - yellow:&lt; 2 GiB   (`BUTLER_PRESSURE_YELLOW_MB`)
pub fn tier_from_available(avail: u64, total: u64) -> PressureTier {
    let black = env_mb("BUTLER_PRESSURE_BLACK_MB", 256);
    let red = env_mb("BUTLER_PRESSURE_RED_MB", 768);
    let yellow = env_mb("BUTLER_PRESSURE_YELLOW_MB", 2048);
    // Absolute floors first (agent/LLM co-tenancy).
    if avail < black {
        return PressureTier::Black;
    }
    if avail < red {
        return PressureTier::Red;
    }
    if avail < yellow {
        return PressureTier::Yellow;
    }
    // Tiny total machines: never claim Green when half the box is "free".
    if total > 0 && total < 8 * GIB && avail < total / 2 {
        return PressureTier::Yellow;
    }
    PressureTier::Green
}

/// Sample current host + process pressure.
pub fn snapshot() -> PressureSnapshot {
    let mem_available = mem_available_bytes().unwrap_or(8 * GIB);
    let mem_total = mem_total_bytes().unwrap_or(mem_available);
    let self_rss = process_rss_bytes().unwrap_or(0);
    let mut tier = tier_from_available(mem_available, mem_total);
    // Already holding a leviathan and free is *critically* thin → bump tier.
    // (Do not bump to Yellow merely because free < 2 GiB — that is normal after gecko install.)
    if self_rss >= 6 * GIB && mem_available < 512 * MIB && tier < PressureTier::Red {
        tier = PressureTier::Red;
    }
    if self_rss >= 8 * GIB && mem_available < 256 * MIB && tier < PressureTier::Black {
        tier = PressureTier::Black;
    }
    PressureSnapshot {
        mem_available,
        mem_total,
        self_rss,
        tier,
    }
}

/// Phase-1 Tree-sitter pool size under current pressure.
///
/// `requested` is the historical “75% cores” ask; pressure may cut harder.
pub fn scan_thread_cap(requested: usize) -> usize {
    let p = snapshot();
    let req = requested.max(1);
    // Looser than first cut: co-tenant boxes still need progress under Yellow/Red.
    let cap = match p.tier {
        PressureTier::Green => req,
        PressureTier::Yellow => req.min(8).max(1),
        PressureTier::Red => req.min(4).max(1),
        PressureTier::Black => 1,
    };
    // ~1/6 of available / ~48 MiB peak per worker (was 1/4 ÷ 80 MiB — too stingy mid-load).
    let by_ram = ((p.mem_available / 6) / (48 * MIB)).max(1) as usize;
    cap.min(by_ram).max(1)
}

/// Estimate warehouse disk footprint for admission (graph + symbols + name_index).
pub fn estimate_cache_bytes(project_root: &std::path::Path) -> u64 {
    let d = project_root.join(".butler/cache");
    let mut sum = 0u64;
    for name in ["graph.bin", "symbols.bin", "name_index.bin", "edges.bin"] {
        if let Ok(meta) = std::fs::metadata(d.join(name)) {
            sum = sum.saturating_add(meta.len());
        }
    }
    sum
}

/// Admit cold full-tree scan or multi‑GiB cache install.
///
/// Policy (co-tenant friendly):
/// - **Disk cache load** is preferred over cold scan — only defer on **Black**.
/// - **Cold full-tree scan** defers on Black, and on Red only when free is truly thin.
/// - Yellow/Green always allow (thread caps still apply).
pub fn admit_warehouse_open(
    project_root: &std::path::Path,
    cold_full_scan: bool,
) -> AdmitDecision {
    let p = snapshot();
    let max_scan_threads = scan_thread_cap(
        (std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4) as f64
            * 0.75)
            .round() as usize,
    );

    // Only Black blocks *all* heavy opens (true OOM cliff).
    if p.tier == PressureTier::Black {
        return AdmitDecision::Defer {
            reason: format!(
                "host memory pressure ({}) — deferred warehouse open. Free RAM or stop co-tenants, then retry. Prefer scope_paths on next open.",
                p.summary_line()
            ),
        };
    }

    let cache_bytes = estimate_cache_bytes(project_root);

    if !cold_full_scan && cache_bytes > 0 {
        // Cache load is the *safe* path vs 32k-file cold scan. Allow under Red/Yellow.
        // (Black already returned.) Optional: warn in logs when free ≪ cache size.
        if p.mem_available < cache_bytes / 4 && p.tier >= PressureTier::Red {
            println!(
                "⚠️  Cache load under tight free RAM ({} cache≈{:.1} GiB) — proceeding; expect swap",
                p.summary_line(),
                cache_bytes as f64 / GIB as f64
            );
        }
        return AdmitDecision::Allow { max_scan_threads };
    }

    if cold_full_scan {
        // Cold full-tree is the expensive path — defer on Red when free < ~512 MiB
        // (not all Red; 768–1024 MiB Red can still crawl with 1–4 threads).
        if p.tier >= PressureTier::Red && p.mem_available < 512 * MIB {
            return AdmitDecision::Defer {
                reason: format!(
                    "host memory pressure ({}) — deferred cold full-tree scan. Retry when freer, or ensure a valid .butler/cache and reopen. Prefer scope_paths once scanning.",
                    p.summary_line()
                ),
            };
        }
        if p.tier >= PressureTier::Yellow {
            return AdmitDecision::Allow {
                max_scan_threads: max_scan_threads.min(4),
            };
        }
    }

    AdmitDecision::Allow { max_scan_threads }
}

/// True if a deferred open may be retried now (tier improved enough).
pub fn may_retry_deferred_open(project_root: &std::path::Path, cold_full_scan: bool) -> bool {
    !matches!(
        admit_warehouse_open(project_root, cold_full_scan),
        AdmitDecision::Defer { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_thresholds_ordered() {
        // Defaults: yellow <2GiB, red <768MiB, black <256MiB (no env overrides in tests).
        assert_eq!(tier_from_available(8 * GIB, 32 * GIB), PressureTier::Green);
        assert_eq!(tier_from_available(3 * GIB, 32 * GIB), PressureTier::Green);
        assert_eq!(tier_from_available(1500 * MIB, 32 * GIB), PressureTier::Yellow);
        assert_eq!(tier_from_available(500 * MIB, 32 * GIB), PressureTier::Red);
        assert_eq!(tier_from_available(100 * MIB, 32 * GIB), PressureTier::Black);
        assert!(PressureTier::Black > PressureTier::Red);
        assert!(PressureTier::Red > PressureTier::Yellow);
    }

    #[test]
    fn scan_thread_cap_at_least_one() {
        let n = scan_thread_cap(16);
        assert!(n >= 1);
        assert!(n <= 16);
    }

    #[test]
    fn snapshot_does_not_panic() {
        let s = snapshot();
        assert!(s.mem_total > 0 || s.mem_available > 0 || cfg!(not(target_os = "linux")));
        let _ = s.summary_line();
    }
}
