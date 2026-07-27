//! `butler ui` — ensure local butler-server, open proof-of-life `/setup`.
//!
//! Stranger path: one command → browser on welcome page (not operator lab).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_BASE: &str = "http://127.0.0.1:8002";
const HEALTH_PATH: &str = "/mcp/health";
const SETUP_PATH: &str = "/setup?spawned=1";

#[derive(Debug, Clone)]
pub struct UiOptions {
    /// Base URL (no trailing slash), e.g. `http://127.0.0.1:8002`.
    pub base_url: String,
    /// Skip opening the browser (print URL only).
    pub no_open: bool,
    /// Do not spawn butler-server if health fails.
    pub no_spawn: bool,
    /// Stop an existing local butler-server and start this install's binary
    /// (so `/setup` HTML matches the binary you just installed).
    pub restart: bool,
    /// How long to wait for health after spawn (seconds).
    pub wait_secs: u64,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            base_url: resolve_default_base(),
            no_open: false,
            no_spawn: false,
            restart: false,
            wait_secs: 45,
        }
    }
}

fn resolve_default_base() -> String {
    std::env::var("BUTLER_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

fn normalize_base(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_string()
}

/// Run the launcher. Returns process exit code (0 = success).
pub fn run_ui(opts: UiOptions) -> i32 {
    let base = normalize_base(&opts.base_url);
    let health_url = format!("{base}{HEALTH_PATH}");
    let setup_url = format!("{base}{SETUP_PATH}");

    eprintln!("Butler UI launcher");
    eprintln!("  target: {base}");

    let already = health_ok(&health_url);
    if already && !opts.restart {
        eprintln!("  health: already up (use --restart after reinstall to load new /setup UI)");
    } else if opts.no_spawn && !already {
        eprintln!("  health: down (spawn disabled with --no-spawn)");
        eprintln!("  tip: start butler-server, then re-run butler ui");
        return 1;
    } else {
        if already && opts.restart {
            eprintln!("  restart: stopping existing butler-server on this host…");
            stop_local_butler_server();
            thread::sleep(Duration::from_millis(600));
        } else {
            eprintln!("  health: down — starting butler-server…");
        }
        match spawn_butler_server() {
            Ok(path) => eprintln!("  spawned: {}", path.display()),
            Err(e) => {
                eprintln!("  spawn failed: {e}");
                eprintln!("  tip: cargo build --release -p cli  # then re-run");
                eprintln!("       or: ~/.local/bin/butler-server");
                eprintln!("  if port busy: pkill -f butler-server  OR  stop Docker butler");
                return 1;
            }
        }
        if !wait_for_health(&health_url, Duration::from_secs(opts.wait_secs)) {
            eprintln!("  health: still down after {}s", opts.wait_secs);
            eprintln!("  tip: check logs / port 8002 already in use by another process");
            return 1;
        }
        eprintln!("  health: ok");
    }

    eprintln!("  setup:  {setup_url}");
    if opts.no_open {
        eprintln!("  (--no-open: not launching browser)");
    } else if let Err(e) = open_browser(&setup_url) {
        eprintln!("  browser: could not open ({e}) — open the URL manually");
    } else {
        eprintln!("  browser: opened (or requested)");
    }
    eprintln!();
    eprintln!("Leave butler-server running while agents use MCP.");
    eprintln!("Operator lab (export/harvest): {base}/ops");
    0
}

fn health_ok(health_url: &str) -> bool {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let Ok(resp) = client.get(health_url).send() else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    let Ok(v) = resp.json::<serde_json::Value>() else {
        // Some health paths return ok without JSON we care about.
        return true;
    };
    matches!(
        v.get("status").and_then(|s| s.as_str()),
        Some("ok") | Some("healthy") | None
    ) || v.get("status").is_some()
}

fn wait_for_health(health_url: &str, budget: Duration) -> bool {
    let start = Instant::now();
    let mut n = 0u32;
    while start.elapsed() < budget {
        if health_ok(health_url) {
            return true;
        }
        n += 1;
        if n == 1 || n % 5 == 0 {
            eprintln!("  waiting for health… ({n})");
        }
        thread::sleep(Duration::from_millis(400));
    }
    false
}

fn spawn_butler_server() -> Result<PathBuf, String> {
    let path = resolve_butler_server_bin()?;
    let mut cmd = Command::new(&path);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("BUTLER__SERVER__HOST", "127.0.0.1");
    // Do not wait on child — leaves server running after butler ui exits.
    cmd.spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", path.display()))?;
    Ok(path)
}

/// Best-effort stop of a host `butler-server` (not Docker). Used by `--restart`.
fn stop_local_butler_server() {
    // Prefer pkill by name; ignore errors if nothing running.
    let _ = Command::new("pkill")
        .args(["-f", "butler-server"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/IM", "butler-server.exe", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn resolve_butler_server_bin() -> Result<PathBuf, String> {
    crate::bin_paths::resolve_tool_bin("butler-server")
        .or_else(|| crate::bin_paths::resolve_tool_bin("server"))
        .ok_or_else(|| {
            "butler-server binary not found (sibling of butler(.exe), install dir, PATH, or target/release)"
                .into()
        })
}

fn open_browser(url: &str) -> Result<(), String> {
    // Prefer platform openers; no extra crate.
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        for cmd in ["xdg-open", "gio", "gnome-open", "kde-open"] {
            if crate::bin_paths::which_bin(cmd).is_some() {
                let r = Command::new(cmd)
                    .arg(url)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                if r.is_ok() {
                    return Ok(());
                }
            }
        }
        for browser in ["firefox", "chromium", "google-chrome", "chrome"] {
            if crate::bin_paths::which_bin(browser).is_some() {
                let r = Command::new(browser)
                    .arg(url)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                if r.is_ok() {
                    return Ok(());
                }
            }
        }
        Err("no xdg-open / browser found".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_base_strips_slash() {
        assert_eq!(normalize_base("http://127.0.0.1:8002/"), "http://127.0.0.1:8002");
    }

    #[test]
    fn default_base_is_local() {
        // May pick up env in CI; just ensure non-empty.
        let b = resolve_default_base();
        assert!(b.starts_with("http"));
    }
}
