//! Ceremony collapse: last project + auto-start local butler-server.
//!
//! Hop A (Always-on / saved last state): agent should not manage port/warm/path.
//! Process may auto-start once if health is down; BUILDING remains honest after connect.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_BASE: &str = "http://127.0.0.1:8002";
const STATE_VERSION: u32 = 1;
const HEALTH_SUFFIX: &str = "/mcp/health";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct LastState {
    #[serde(default = "state_version")]
    pub version: u32,
    /// Absolute project root last successfully used for Trace/context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_project: Option<String>,
    /// Operator-pinned roots: watcher + RAM stay up on purpose (idle/budget sleep skips these).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hold_projects: Vec<String>,
    /// Local server base last used (default loopback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_base_url: Option<String>,
    #[serde(default)]
    pub updated_unix: u64,
}

fn state_version() -> u32 {
    STATE_VERSION
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `~/.config/butler/last_state.json` (or platform equivalent via directories crate).
pub fn last_state_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Butler", "butler")
        .map(|d| d.config_dir().join("last_state.json"))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                PathBuf::from(h)
                    .join(".config")
                    .join("butler")
                    .join("last_state.json")
            })
        })
}

pub fn load_last_state() -> LastState {
    let Some(path) = last_state_path() else {
        return LastState::default();
    };
    let Ok(bytes) = fs::read(&path) else {
        return LastState::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save_last_state(state: &LastState) {
    let Some(path) = last_state_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut s = state.clone();
    s.version = STATE_VERSION;
    s.updated_unix = now_unix();
    if let Ok(bytes) = serde_json::to_vec_pretty(&s) {
        let _ = fs::write(path, bytes);
    }
}

/// Absolute existing directory, or `None`.
pub fn normalize_hold_root(root: &str) -> Option<String> {
    let raw = root.trim();
    if raw.is_empty() {
        return None;
    }
    let path = Path::new(raw);
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !abs.is_dir() {
        return None;
    }
    Some(abs.to_string_lossy().into_owned())
}

/// Roots the operator pinned with `butler hold` (watcher stays up on purpose).
pub fn list_hold_roots() -> Vec<String> {
    let mut v = load_last_state().hold_projects;
    v.retain(|s| !s.trim().is_empty());
    v.sort();
    v.dedup();
    v
}

/// Pin `root` against warehouse idle/budget sleep. Returns the stored absolute path.
pub fn add_hold_root(root: &str) -> Result<String, String> {
    let abs = normalize_hold_root(root).ok_or_else(|| format!("hold: not a directory: {root}"))?;
    let mut st = load_last_state();
    if !st.hold_projects.iter().any(|r| r == &abs) {
        st.hold_projects.push(abs.clone());
    }
    save_last_state(&st);
    Ok(abs)
}

/// Drop one hold, or all holds when `root` is `None`. Returns remaining holds.
pub fn release_hold_root(root: Option<&str>) -> Result<Vec<String>, String> {
    let mut st = load_last_state();
    if let Some(raw) = root {
        let key = normalize_hold_root(raw).unwrap_or_else(|| raw.trim().to_string());
        st.hold_projects
            .retain(|r| r != &key && !r.ends_with(&key) && !key.ends_with(r));
    } else {
        st.hold_projects.clear();
    }
    save_last_state(&st);
    Ok(list_hold_roots())
}

/// Remember a successful Trace project + URL for next session.
pub fn remember_project(project: &str, base_url: &str) {
    let p = project.trim();
    if p.is_empty() || !Path::new(p).is_absolute() {
        return;
    }
    let mut st = load_last_state();
    st.last_project = Some(p.to_string());
    let base = normalize_base(base_url);
    if !base.is_empty() {
        st.last_base_url = Some(base);
    }
    save_last_state(&st);
}

pub fn normalize_base(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_string()
}

/// Default base: env BUTLER_URL → last_state → loopback 8002.
pub fn resolve_default_base() -> String {
    if let Ok(u) = std::env::var("BUTLER_URL") {
        let t = normalize_base(&u);
        if !t.is_empty() {
            return t;
        }
    }
    if let Some(u) = load_last_state().last_base_url {
        let t = normalize_base(&u);
        if !t.is_empty() {
            return t;
        }
    }
    DEFAULT_BASE.to_string()
}

/// Resolve project for a tool call: explicit arg → last_state if absolute path exists.
pub fn resolve_project_arg(explicit: Option<&str>) -> Option<String> {
    if let Some(p) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(p.to_string());
    }
    let last = load_last_state().last_project?;
    let path = Path::new(&last);
    if path.is_absolute() && path.is_dir() {
        Some(last)
    } else {
        None
    }
}

pub fn autostart_disabled() -> bool {
    matches!(
        std::env::var("BUTLER_NO_AUTOSTART").as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on")
    )
}

pub fn health_ok(base_url: &str) -> bool {
    let base = normalize_base(base_url);
    let health_url = format!("{base}{HEALTH_SUFFIX}");
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let Ok(resp) = client.get(&health_url).send() else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    true
}

fn wait_for_health(base_url: &str, budget: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < budget {
        if health_ok(base_url) {
            return true;
        }
        thread::sleep(Duration::from_millis(400));
    }
    false
}

/// Parse port from `http://host:port` (default 8002).
fn port_from_base(base: &str) -> u16 {
    let b = normalize_base(base);
    b.rsplit(':')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8002)
}

fn spawn_butler_server(base_url: &str) -> Result<PathBuf, String> {
    let path = crate::bin_paths::resolve_tool_bin("butler-server")
        .or_else(|| crate::bin_paths::resolve_tool_bin("server"))
        .ok_or_else(|| {
            "butler-server binary not found (sibling of mcp/butler, ~/.local/bin, or target/release)"
                .to_string()
        })?;
    let port = port_from_base(base_url);
    let mut cmd = Command::new(&path);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("BUTLER__SERVER__HOST", "127.0.0.1")
        .env("BUTLER__SERVER__PORT", port.to_string());
    // Optional: warm last project + operator holds at boot so first Trace is not cold BUILDING.
    let st = load_last_state();
    let mut warm: Vec<String> = st.hold_projects.clone();
    if let Some(proj) = st.last_project {
        if Path::new(&proj).is_dir() && !warm.iter().any(|r| r == &proj) {
            warm.push(proj);
        }
    }
    if !warm.is_empty() {
        cmd.env("BUTLER_WARM_ROOTS", warm.join(":"));
    }
    cmd.spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", path.display()))?;
    Ok(path)
}

/// If local health is down and auto-start is allowed, spawn once and wait for ready.
///
/// Returns `(base_url, started_now)`. Does not hide BUILDING — only process reachability.
pub fn ensure_local_server(base_url: &str, wait_secs: u64) -> Result<(String, bool), String> {
    let base = {
        let b = normalize_base(base_url);
        if b.is_empty() {
            resolve_default_base()
        } else {
            b
        }
    };
    if health_ok(&base) {
        return Ok((base, false));
    }
    if autostart_disabled() {
        return Err(format!(
            "Butler not reachable at {base} (BUTLER_NO_AUTOSTART set)"
        ));
    }
    // Only auto-start for loopback — never remote.
    if !(base.contains("127.0.0.1") || base.contains("localhost")) {
        return Err(format!(
            "Butler not reachable at {base} (auto-start only for localhost)"
        ));
    }
    let path = spawn_butler_server(&base)?;
    let _ = path;
    if !wait_for_health(&base, Duration::from_secs(wait_secs.max(5))) {
        return Err(format!(
            "spawned butler-server but health still down at {base} after {wait_secs}s"
        ));
    }
    Ok((base, true))
}

/// Best-effort fire-and-forget warm of a project root (async HTTP not available here).
pub fn warm_project_blocking(base_url: &str, project: &str) {
    let base = normalize_base(base_url);
    let url = format!("{base}/warm");
    let body = serde_json::json!({ "roots": [project] });
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = client.post(url).json(&body).send();
}

/// Fill missing `project` from last_state; return whether we filled it.
pub fn inject_project_if_missing(params: &mut serde_json::Value) -> bool {
    let obj = match params.as_object_mut() {
        Some(o) => o,
        None => return false,
    };
    let has = obj
        .get("project")
        .or_else(|| obj.get("root"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty());
    if has {
        return false;
    }
    let Some(last) = resolve_project_arg(None) else {
        return false;
    };
    obj.insert("project".into(), serde_json::json!(last));
    true
}

/// After success: remember project from params.
pub fn remember_from_params(params: &serde_json::Value, base_url: &str) {
    let project = params
        .get("project")
        .or_else(|| params.get("root"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !project.is_empty() {
        remember_project(project, base_url);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_base_trims_slash() {
        assert_eq!(normalize_base("http://127.0.0.1:8002/"), "http://127.0.0.1:8002");
    }

    #[test]
    fn resolve_project_prefers_explicit() {
        assert_eq!(
            resolve_project_arg(Some("/tmp/foo")),
            Some("/tmp/foo".into())
        );
    }

    #[test]
    fn last_state_hold_projects_roundtrip() {
        let raw = r#"{"version":1,"hold_projects":["/tmp/a"],"updated_unix":1}"#;
        let st: LastState = serde_json::from_str(raw).unwrap();
        assert_eq!(st.hold_projects, vec!["/tmp/a".to_string()]);
        let empty: LastState = serde_json::from_str(r#"{"version":1}"#).unwrap();
        assert!(empty.hold_projects.is_empty());
    }

    #[test]
    fn normalize_hold_root_requires_dir() {
        let dir = std::env::temp_dir().join(format!("butler_hold_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let got = normalize_hold_root(&dir.to_string_lossy()).expect("dir");
        assert!(
            got.ends_with(dir.file_name().unwrap().to_string_lossy().as_ref())
                || Path::new(&got).is_dir()
        );
        assert!(normalize_hold_root("/no/such/butler_hold_dir_zzz").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inject_project_fills_when_missing() {
        // Only fills if last_state has a real dir — use temp.
        let dir = std::env::temp_dir().join(format!("butler_last_proj_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.to_string_lossy().to_string();
        remember_project(&path, "http://127.0.0.1:8002");
        let mut params = serde_json::json!({ "goal": "TraceBlastRadius", "target_symbol": "main" });
        assert!(inject_project_if_missing(&mut params));
        assert_eq!(params.get("project").and_then(|v| v.as_str()), Some(path.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
