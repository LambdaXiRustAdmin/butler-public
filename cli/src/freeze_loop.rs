//! Trace-native freeze → check (Shape A).
//!
//! Inspired by Enola’s pin→check *loop*, not their CLI/MCP/schema.
//! Butler **pin** stays `scope_paths`. These verbs are **freeze** / **check**.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FreezeShot {
    pub format: u32,
    pub created_unix: u64,
    pub butler_version: String,
    pub project: String,
    pub symbol: String,
    #[serde(default)]
    pub scope_paths: Vec<String>,
    pub comparable: bool,
    #[serde(default)]
    pub building: bool,
    #[serde(default)]
    pub blast_domain: Option<String>,
    #[serde(default)]
    pub seed_file: Option<String>,
    #[serde(default)]
    pub seed_line: Option<u64>,
    #[serde(default)]
    pub confidence: Option<String>,
    #[serde(default)]
    pub edges: Option<String>,
    #[serde(default)]
    pub basis: Option<String>,
    #[serde(default)]
    pub callers: Vec<String>,
    #[serde(default)]
    pub callees: Vec<String>,
    #[serde(default)]
    pub bridges: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FreezeDelta {
    pub comparable: bool,
    pub warn: Option<String>,
    pub star_moved: bool,
    pub lost_callers: Vec<String>,
    pub lost_callees: Vec<String>,
    pub lost_bridges: Vec<String>,
    pub high_conf_now_empty: bool,
}

impl FreezeDelta {
    pub fn hard_fail(&self, before: &FreezeShot) -> bool {
        if !self.comparable {
            return false;
        }
        if self.high_conf_now_empty {
            return true;
        }
        if !self.lost_bridges.is_empty() {
            return true;
        }
        let complete = before
            .edges
            .as_deref()
            .is_some_and(|e| e.starts_with("complete"));
        complete && (!self.lost_callers.is_empty() || !self.lost_callees.is_empty())
    }
}

pub fn freeze_dir(project: &Path) -> PathBuf {
    project.join(".butler").join("freeze")
}

pub fn freeze_path(project: &Path, symbol: &str) -> PathBuf {
    freeze_dir(project).join(format!("{}.json", sanitize_symbol(symbol)))
}

pub fn sanitize_symbol(symbol: &str) -> String {
    let s: String = symbol
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "seed".into()
    } else {
        s
    }
}

pub fn shot_from_context_json(
    project: &str,
    symbol: &str,
    scopes: &[String],
    v: &serde_json::Value,
) -> FreezeShot {
    let content = v.get("content").and_then(|c| c.as_str()).unwrap_or("");
    let building = content.contains("=== Building Graph") || content.contains("status: BUILDING");
    let st = v
        .get("structured")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let rec = st.get("receipt");
    let target = st.get("target");
    FreezeShot {
        format: FORMAT,
        created_unix: now_unix(),
        butler_version: env!("CARGO_PKG_VERSION").to_string(),
        project: project.to_string(),
        symbol: symbol.to_string(),
        scope_paths: scopes.to_vec(),
        comparable: !building,
        building,
        blast_domain: st
            .get("blast_domain")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        seed_file: target
            .and_then(|t| t.get("file"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        seed_line: target.and_then(|t| t.get("line")).and_then(|x| x.as_u64()),
        confidence: rec
            .and_then(|r| r.get("confidence"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        edges: rec
            .and_then(|r| r.get("edges"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        basis: rec
            .and_then(|r| r.get("basis"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        callers: names_from_neighbors(st.get("callers")),
        callees: names_from_neighbors(st.get("callees")),
        bridges: {
            let mut b = names_from_neighbors(st.get("bridge_callers"));
            b.extend(names_from_neighbors(st.get("bridge_callees")));
            b.sort();
            b.dedup();
            b
        },
    }
}

fn names_from_neighbors(v: Option<&serde_json::Value>) -> Vec<String> {
    let Some(arr) = v.and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out: BTreeSet<String> = BTreeSet::new();
    for row in arr {
        let hop = row.get("hop").and_then(|h| h.as_u64()).unwrap_or(1);
        if hop > 1 {
            continue;
        }
        if let Some(n) = row.get("name").and_then(|x| x.as_str()) {
            if !n.is_empty() {
                out.insert(n.to_string());
            }
        }
    }
    out.into_iter().collect()
}

pub fn diff_shots(before: &FreezeShot, after: &FreezeShot) -> FreezeDelta {
    if before.building || after.building || !before.comparable || !after.comparable {
        return FreezeDelta {
            comparable: false,
            warn: Some("incomparable (BUILDING or incomplete freeze) — not a pass".into()),
            ..FreezeDelta::default()
        };
    }
    if before.project != after.project || before.symbol != after.symbol {
        return FreezeDelta {
            comparable: false,
            warn: Some("incomparable (project/symbol mismatch)".into()),
            ..FreezeDelta::default()
        };
    }
    let lost_callers = set_lost(&before.callers, &after.callers);
    let lost_callees = set_lost(&before.callees, &after.callees);
    let lost_bridges = set_lost(&before.bridges, &after.bridges);
    let had_star = before.seed_file.is_some();
    let now_empty =
        after.seed_file.is_none() && after.callers.is_empty() && after.callees.is_empty();
    let high = before.confidence.as_deref().is_some_and(|c| c == "high");
    let complete = before
        .edges
        .as_deref()
        .is_some_and(|e| e.starts_with("complete"));
    FreezeDelta {
        comparable: true,
        warn: None,
        star_moved: match (&before.seed_file, &after.seed_file) {
            (Some(a), Some(b)) => a != b || before.seed_line != after.seed_line,
            _ => false,
        },
        lost_callers,
        lost_callees,
        lost_bridges,
        high_conf_now_empty: high && complete && had_star && now_empty,
    }
}

fn set_lost(before: &[String], after: &[String]) -> Vec<String> {
    let now: BTreeSet<&str> = after.iter().map(|s| s.as_str()).collect();
    before
        .iter()
        .filter(|s| !now.contains(s.as_str()))
        .cloned()
        .collect()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

type Error = Box<dyn std::error::Error + 'static>;

pub fn write_shot(path: &Path, shot: &FreezeShot) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let s = serde_json::to_string_pretty(shot)?;
    fs::write(path, s)?;
    Ok(())
}

pub fn read_shot(path: &Path) -> Result<FreezeShot, Error> {
    let raw = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let shot: FreezeShot = serde_json::from_str(&raw)?;
    if shot.format != FORMAT {
        return Err(format!("freeze format {} unsupported (want {FORMAT})", shot.format).into());
    }
    Ok(shot)
}

pub fn fetch_context(
    server: &str,
    project: &str,
    symbol: &str,
    scopes: &[String],
) -> Result<serde_json::Value, Error> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let url = format!("{}/context", server.trim_end_matches('/'));
    let body = serde_json::json!({
        "project": project,
        "symbol": symbol,
        "scope_paths": scopes,
        "mode": "trace",
        "detail": "short",
    });
    let resp = crate::config::apply_client_auth_blocking(client.post(&url).json(&body)).send()?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "POST {url} HTTP {status}: {}",
            text.chars().take(240).collect::<String>()
        )
        .into());
    }
    Ok(serde_json::from_str(&text)?)
}

pub fn run_freeze(
    root: &str,
    symbol: &str,
    scopes: Vec<String>,
    server: &str,
) -> Result<PathBuf, Error> {
    let project = std::fs::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root));
    let project_s = project.to_string_lossy().into_owned();
    let v = fetch_context(server, &project_s, symbol, &scopes)?;
    let shot = shot_from_context_json(&project_s, symbol, &scopes, &v);
    if shot.building {
        return Err(
            "warehouse BUILDING — freeze refused (retry when Trace is non-BUILDING)".into(),
        );
    }
    let path = freeze_path(&project, symbol);
    write_shot(&path, &shot)?;
    Ok(path)
}

pub fn run_check(
    root: &str,
    symbol: Option<&str>,
    server: &str,
) -> Result<(FreezeShot, FreezeShot, FreezeDelta, PathBuf), Error> {
    let project = std::fs::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root));
    let dir = freeze_dir(&project);
    let path = match symbol {
        Some(s) => freeze_path(&project, s),
        None => {
            let mut jsons: Vec<_> = fs::read_dir(&dir)
                .map_err(|e| format!("no freeze dir {}: {e}", dir.display()))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
                .collect();
            jsons.sort();
            jsons
                .into_iter()
                .next()
                .ok_or_else(|| format!("no freeze json in {}", dir.display()))?
        }
    };
    let before = read_shot(&path)?;
    let project_s = project.to_string_lossy().into_owned();
    let v = fetch_context(server, &project_s, &before.symbol, &before.scope_paths)?;
    let after = shot_from_context_json(&project_s, &before.symbol, &before.scope_paths, &v);
    let delta = diff_shots(&before, &after);
    Ok((before, after, delta, path))
}

pub fn print_delta(before: &FreezeShot, delta: &FreezeDelta) {
    println!(
        "check: {}  freeze={}  ★ {}",
        if delta.comparable {
            "comparable"
        } else {
            "incomparable"
        },
        before.symbol,
        before.seed_file.as_deref().unwrap_or("?")
    );
    if let Some(w) = &delta.warn {
        println!("  warn: {w}");
    }
    if delta.star_moved {
        println!("  star moved");
    }
    for (label, names) in [
        ("lost callers", &delta.lost_callers),
        ("lost callees", &delta.lost_callees),
        ("lost bridges", &delta.lost_bridges),
    ] {
        if !names.is_empty() {
            println!("  {label}: {}", names.join(", "));
        }
    }
    if delta.high_conf_now_empty {
        println!("  high-conf Trace now empty");
    }
    if delta.hard_fail(before) {
        println!("HARD (new structural loss vs freeze)");
    } else if delta.comparable {
        println!("ok (no hard loss)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(callers: &[&str], bridges: &[&str], conf: &str, edges: &str) -> FreezeShot {
        FreezeShot {
            format: FORMAT,
            created_unix: 0,
            butler_version: "t".into(),
            project: "/p".into(),
            symbol: "Foo".into(),
            scope_paths: vec!["src/".into()],
            comparable: true,
            building: false,
            blast_domain: Some("call".into()),
            seed_file: Some("a.rs".into()),
            seed_line: Some(1),
            confidence: Some(conf.into()),
            edges: Some(edges.into()),
            basis: Some("bare-name".into()),
            callers: callers.iter().map(|s| (*s).to_string()).collect(),
            callees: vec![],
            bridges: bridges.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn noop_is_not_hard() {
        let a = shot(&["with_posture"], &[], "high", "complete");
        let d = diff_shots(&a, &a);
        assert!(d.comparable);
        assert!(!d.hard_fail(&a));
        assert!(d.lost_callers.is_empty());
    }

    #[test]
    fn lost_caller_hard_when_complete() {
        let before = shot(
            &["with_posture", "rebuild_fragments"],
            &[],
            "high",
            "complete",
        );
        let mut after = before.clone();
        after.callers = vec!["with_posture".into()];
        let d = diff_shots(&before, &after);
        assert_eq!(d.lost_callers, vec!["rebuild_fragments".to_string()]);
        assert!(d.hard_fail(&before));
    }

    #[test]
    fn empty_stays_empty_not_hard() {
        let before = shot(&[], &[], "high", "complete");
        let after = before.clone();
        let d = diff_shots(&before, &after);
        assert!(!d.hard_fail(&before));
        assert!(!d.high_conf_now_empty);
    }

    #[test]
    fn building_incomparable() {
        let mut before = shot(&["a"], &[], "high", "complete");
        before.building = true;
        before.comparable = false;
        let after = shot(&["a"], &[], "high", "complete");
        let d = diff_shots(&before, &after);
        assert!(!d.comparable);
        assert!(!d.hard_fail(&before));
    }

    #[test]
    fn lost_bridge_is_hard() {
        let before = shot(&["a"], &["export:foo"], "high", "complete");
        let mut after = before.clone();
        after.bridges.clear();
        let d = diff_shots(&before, &after);
        assert_eq!(d.lost_bridges, vec!["export:foo".to_string()]);
        assert!(d.hard_fail(&before));
    }

    #[test]
    fn shot_from_json_skips_hop2() {
        let v = serde_json::json!({
            "content": "ok",
            "structured": {
                "target": {"name": "Foo", "file": "a.rs", "line": 3},
                "receipt": {"confidence": "high", "edges": "complete", "basis": "bare-name"},
                "callers": [
                    {"name": "direct", "hop": 1},
                    {"name": "far", "hop": 2}
                ],
                "callees": [],
                "bridge_callers": [],
                "bridge_callees": []
            }
        });
        let s = shot_from_context_json("/p", "Foo", &[], &v);
        assert_eq!(s.callers, vec!["direct".to_string()]);
        assert!(s.comparable);
        assert!(!s.building);
    }
}
