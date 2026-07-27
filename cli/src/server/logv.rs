//! Per-request / diagnostic log gate for the Butler server.
//!
//! **Default: quiet.** High-QPS Trace memo paths must not burn the CPU on `println!`.
//! Enable with `BUTLER_VERBOSE=1` (or `true`/`yes`) or `BUTLER_LOG_VERBOSE=1`.
//!
//! Boot banners and `eprintln!` errors stay unconditional.

use std::sync::OnceLock;

static VERBOSE: OnceLock<bool> = OnceLock::new();

fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .map(|v| {
            let v = v.trim();
            v == "1"
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false)
}

/// Resolve once. Safe to call from any thread.
#[inline]
pub fn verbose() -> bool {
    *VERBOSE.get_or_init(|| env_truthy("BUTLER_VERBOSE") || env_truthy("BUTLER_LOG_VERBOSE"))
}

/// Call once at server boot.
pub fn log_boot_banner() {
    if verbose() {
        println!("📝 BUTLER_VERBOSE on — per-request TRACE / REQUEST / score_audit logs enabled");
    } else {
        println!(
            "📝 stdout request logs quiet (BUTLER_VERBOSE=1 for TRACE/REQUEST/audit)"
        );
    }
    // Always-on ring file (last N /context) — separate from verbose stdout noise.
    crate::server::request_ring::boot_banner();
}
