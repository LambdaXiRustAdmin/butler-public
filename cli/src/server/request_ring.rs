//! Always-on ring buffer of recent `/context` requests (debug blind-spot closer).
//!
//! **Not** gated on `BUTLER_VERBOSE` — hydrate/Trace timing and tool wiring need a
//! short trail even when stdout is quiet. Default capacity **32**; file rewritten
//! to the last N lines on each record (self-truncating, no unbounded growth).
//!
//! Env:
//! - `BUTLER_REQUEST_LOG` — path (default: `{tmp}/butler_requests.log` or
//!   `/tmp/butler_requests.log`)
//! - `BUTLER_REQUEST_LOG_CAP` — ring size (default 32, min 8, max 200)
//! - `BUTLER_REQUEST_LOG=0` / `off` / `false` — disable file + ring

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static RING: OnceLock<Mutex<RequestRing>> = OnceLock::new();

struct RequestRing {
    cap: usize,
    lines: VecDeque<String>,
    path: Option<PathBuf>,
    disabled: bool,
}

fn env_disabled() -> bool {
    match std::env::var("BUTLER_REQUEST_LOG") {
        Ok(v) => {
            let t = v.trim();
            t == "0"
                || t.eq_ignore_ascii_case("off")
                || t.eq_ignore_ascii_case("false")
                || t.eq_ignore_ascii_case("no")
        }
        Err(_) => false,
    }
}

fn resolve_path() -> Option<PathBuf> {
    if env_disabled() {
        return None;
    }
    if let Ok(p) = std::env::var("BUTLER_REQUEST_LOG") {
        let t = p.trim();
        if !t.is_empty()
            && t != "0"
            && !t.eq_ignore_ascii_case("off")
            && !t.eq_ignore_ascii_case("false")
        {
            return Some(PathBuf::from(t));
        }
    }
    // Prefer /tmp (container-writable); fall back to std temp.
    let p = Path::new("/tmp/butler_requests.log");
    if p.parent().is_some_and(|d| d.is_dir()) {
        Some(p.to_path_buf())
    } else {
        Some(std::env::temp_dir().join("butler_requests.log"))
    }
}

fn cap_from_env() -> usize {
    std::env::var("BUTLER_REQUEST_LOG_CAP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(32)
        .clamp(8, 200)
}

fn ring() -> &'static Mutex<RequestRing> {
    RING.get_or_init(|| {
        Mutex::new(RequestRing {
            cap: cap_from_env(),
            lines: VecDeque::with_capacity(cap_from_env()),
            path: resolve_path(),
            disabled: env_disabled(),
        })
    })
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One-line summary of a finished `/context` call (always recorded unless disabled).
pub fn record_context(
    duration_ms: u64,
    project: &str,
    goal: Option<&str>,
    symbol: Option<&str>,
    tool: Option<&str>,
    mode: Option<&str>,
    warning: Option<&str>,
    ok: bool,
) {
    let r = ring();
    let Ok(mut g) = r.lock() else {
        return;
    };
    if g.disabled {
        return;
    }
    let ts = now_unix_ms();
    let proj = truncate(project, 96);
    let goal = goal.unwrap_or("-");
    let sym = symbol.unwrap_or("-");
    let tool = tool.unwrap_or("-");
    let mode = mode.unwrap_or("-");
    let warn = warning.map(|w| truncate(w, 40)).unwrap_or_else(|| "-".into());
    let status = if ok { "ok" } else { "err" };
    let line = format!(
        "{ts} {status} {duration_ms}ms tool={tool} goal={goal} symbol={sym} mode={mode} warn={warn} project={proj}"
    );
    g.lines.push_back(line);
    while g.lines.len() > g.cap {
        g.lines.pop_front();
    }
    flush_file(&g);
}

fn truncate(s: &str, max: usize) -> String {
    let t = s.replace('\n', " ").replace('\r', " ");
    if t.chars().count() <= max {
        t
    } else {
        let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn flush_file(g: &RequestRing) {
    let Some(path) = g.path.as_ref() else {
        return;
    };
    // Rewrite whole file = natural truncate to last N lines (no grow forever).
    let body: String = g.lines.iter().fold(String::new(), |mut acc, l| {
        acc.push_str(l);
        acc.push('\n');
        acc
    });
    let tmp = path.with_extension("log.tmp");
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp)
    {
        let _ = f.write_all(body.as_bytes());
        let _ = f.flush();
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Path of the ring log (for boot banner / health).
pub fn log_path() -> Option<PathBuf> {
    ring().lock().ok().and_then(|g| g.path.clone())
}

pub fn cap() -> usize {
    ring().lock().map(|g| g.cap).unwrap_or(32)
}

/// Snapshot of ring lines (newest last) for `/mcp/health` or debug.
pub fn snapshot() -> Vec<String> {
    ring()
        .lock()
        .map(|g| g.lines.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn boot_banner() {
    if env_disabled() {
        println!("📝 request ring log disabled (BUTLER_REQUEST_LOG=0)");
        return;
    }
    let path = log_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "?".into());
    println!(
        "📝 request ring: last {} /context → {} (always-on; not BUTLER_VERBOSE)",
        cap(),
        path
    );
}
