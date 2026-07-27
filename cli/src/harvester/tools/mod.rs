//! Harvester tools — must be rock-solid: the LLM only spends money well if
//! disk reads and greps return truthful, useful results under the harvest root.

use super::source::Source;
use regex::Regex;
use std::path::{Path, PathBuf};

pub type ToolResult = serde_json::Value;

pub struct ToolRegistry {
    source: Option<Source>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { source: None }
    }

    pub fn with_source(source: Source) -> Self {
        Self {
            source: Some(source),
        }
    }

    fn repo_root(&self) -> PathBuf {
        self.source
            .as_ref()
            .map(|s| s.repo.clone())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Resolve a path relative to the harvest repo root (steak: no cwd lottery).
    pub fn resolve_path(&self, path_str: &str) -> PathBuf {
        let p = Path::new(path_str);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.repo_root().join(p)
        }
    }

    pub fn dispatch(&self, action: &str, args: &serde_json::Value) -> ToolResult {
        match action {
            "read_file" => self.read_file(args),
            "grep" => self.grep(args),
            "butler_orchestrate" => self.butler_orchestrate(args),
            "emit_batch" => self.emit_batch(args),
            "rollback_frontier" => self.rollback(args),
            "get_codegraph_nodes" => self.get_codegraph_nodes(args),
            "get_neighborhood_card" => self.get_neighborhood_card(args),
            _ => serde_json::json!({"error": "unknown action", "action": action}),
        }
    }

    fn read_file(&self, args: &serde_json::Value) -> ToolResult {
        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if path_str.is_empty() {
            return serde_json::json!({"error": "path required"});
        }
        let max = args.get("max_lines").and_then(|v| v.as_u64()).unwrap_or(80) as usize;
        let max = max.clamp(1, 200);
        let start = args
            .get("start")
            .or_else(|| args.get("start_line"))
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;
        let start = start.max(1);

        let path = self.resolve_path(path_str);
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let total = lines.len();
                let from = start.saturating_sub(1).min(total);
                let slice: Vec<&str> = lines.iter().skip(from).take(max).copied().collect();
                serde_json::json!({
                    "ok": true,
                    "path": path.to_string_lossy(),
                    "requested": path_str,
                    "start_line": start,
                    "lines_returned": slice.len(),
                    "total_lines": total,
                    "content": slice.join("\n"),
                })
            }
            Err(e) => serde_json::json!({
                "ok": false,
                "path": path.to_string_lossy(),
                "requested": path_str,
                "error": format!("not found or unreadable: {e}"),
            }),
        }
    }

    fn grep(&self, args: &serde_json::Value) -> ToolResult {
        let pat = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("src");
        if pat.is_empty() {
            return serde_json::json!({
                "ok": false,
                "pattern": pat,
                "scope": scope,
                "matches": [],
                "error": "empty pattern",
            });
        }

        let root = self.repo_root();
        let base = if Path::new(scope).is_absolute() {
            PathBuf::from(scope)
        } else {
            root.join(scope)
        };
        if !base.exists() {
            return serde_json::json!({
                "ok": false,
                "pattern": pat,
                "scope": scope,
                "resolved_scope": base.to_string_lossy(),
                "matches": [],
                "error": "scope path does not exist under harvest root",
            });
        }

        // Support simple alternation: "a|b|c" → regex OR; else literal, then soft regex.
        let re = match build_search_regex(pat) {
            Ok(r) => r,
            Err(e) => {
                return serde_json::json!({
                    "ok": false,
                    "pattern": pat,
                    "scope": scope,
                    "matches": [],
                    "error": format!("bad pattern: {e}"),
                });
            }
        };

        let max_files = 80usize;
        let max_matches = 30usize;
        let mut matches = Vec::new();
        let mut files_scanned = 0usize;
        walk_grep(
            &base,
            &root,
            &re,
            &mut matches,
            &mut files_scanned,
            max_files,
            max_matches,
        );

        serde_json::json!({
            "ok": true,
            "pattern": pat,
            "scope": scope,
            "resolved_scope": base.to_string_lossy(),
            "files_scanned": files_scanned,
            "match_count": matches.len(),
            "truncated": matches.len() >= max_matches || files_scanned >= max_files,
            "matches": matches,
        })
    }

    fn butler_orchestrate(&self, args: &serde_json::Value) -> ToolResult {
        let focus = args
            .get("target_symbol")
            .or_else(|| args.get("symbol"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if focus.is_empty() {
            return serde_json::json!({
                "ok": false,
                "error": "target_symbol required — pass a name from a card, not a blank hub dump",
            });
        }
        if let Some(ref s) = self.source {
            if let Some(g) = s.load_code_graph() {
                let mut hits = Vec::new();
                for b in g.nodes.values() {
                    if b.name == focus || b.name.contains(focus) || b.id.as_str().contains(focus) {
                        let mut callers = Vec::new();
                        let mut callees = Vec::new();
                        if let Some(outs) = g.edges.get(&b.id) {
                            for o in outs.iter().take(10) {
                                if let Some(nb) = g.nodes.get(o) {
                                    callees.push(serde_json::json!({
                                        "id": o.as_str(),
                                        "name": nb.name,
                                        "kind": nb.kind,
                                    }));
                                }
                            }
                        }
                        if let Some(ins) = g.reverse.get(&b.id) {
                            for i in ins.iter().take(10) {
                                if let Some(nb) = g.nodes.get(i) {
                                    callers.push(serde_json::json!({
                                        "id": i.as_str(),
                                        "name": nb.name,
                                        "kind": nb.kind,
                                    }));
                                }
                            }
                        }
                        hits.push(serde_json::json!({
                            "id": b.id.as_str(),
                            "name": b.name,
                            "kind": b.kind,
                            "file": b.file.to_string_lossy(),
                            "in_degree": callers.len(),
                            "out_degree": callees.len(),
                            "callers": callers,
                            "callees": callees,
                        }));
                        if hits.len() >= 5 {
                            break;
                        }
                    }
                }
                return serde_json::json!({
                    "ok": true,
                    "goal": args.get("goal"),
                    "focus": focus,
                    "hits": hits,
                });
            }
        }
        serde_json::json!({"ok": false, "error": "no code graph", "focus": focus, "hits": []})
    }

    fn get_neighborhood_card(&self, args: &serde_json::Value) -> ToolResult {
        let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            return serde_json::json!({"ok": false, "error": "id required"});
        }
        if let Some(ref s) = self.source {
            if let Some(g) = s.load_code_graph() {
                if let Some(card) = super::cards::build_card(g, id, query, "tool_request") {
                    return serde_json::json!({"ok": true, "card": card});
                }
            }
        }
        serde_json::json!({"ok": false, "error": "card not found", "id": id})
    }

    fn emit_batch(&self, args: &serde_json::Value) -> ToolResult {
        serde_json::json!({
            "emitted": true,
            "nodes": args.get("nodes").cloned().unwrap_or(serde_json::json!([]))
        })
    }

    fn rollback(&self, args: &serde_json::Value) -> ToolResult {
        serde_json::json!({"rolled_back": true, "target": args.get("target_node_id")})
    }

    fn get_codegraph_nodes(&self, args: &serde_json::Value) -> ToolResult {
        // Prefer ids listed by the model (from cards); never random first-5 of whole graph.
        let want: Vec<String> = args
            .get("ids")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        if let Some(ref s) = self.source {
            if let Some(g) = s.load_code_graph() {
                let mut nodes = Vec::new();
                if !want.is_empty() {
                    for id in want.iter().take(12) {
                        if let Some(b) = g.nodes.values().find(|b| b.id.as_str() == id) {
                            let snip = if b.source.len() > 400 {
                                format!("{}...", &b.source[..floor_char(&b.source, 400)])
                            } else {
                                b.source.clone()
                            };
                            nodes.push(serde_json::json!({
                                "id": b.id.as_str(),
                                "name": b.name,
                                "kind": b.kind,
                                "lang": b.lang,
                                "file": b.file.to_string_lossy(),
                                "snippet": snip,
                            }));
                        }
                    }
                } else {
                    return serde_json::json!({
                        "ok": false,
                        "error": "pass args.ids from neighborhood cards — refusing global sample",
                        "nodes": [],
                    });
                }
                return serde_json::json!({"ok": true, "nodes": nodes});
            }
        }
        serde_json::json!({"ok": false, "nodes": [], "error": "no graph"})
    }
}

/// Build a search regex: `a|b` as alternation of literals; otherwise escape literal.
/// Literals are matched as substrings (same spirit as ripgrep fixed-string), not as
/// open-ended prefixes of larger words when the pattern ends with a word char —
/// we use `(?i)` off and leave word boundaries to the user if needed.
fn build_search_regex(pat: &str) -> Result<Regex, String> {
    if pat.contains('|') && !pat.contains('(') {
        let parts: Vec<String> = pat
            .split('|')
            .map(|p| regex::escape(p.trim()))
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            return Err("empty alternation".into());
        }
        let joined = parts.join("|");
        return Regex::new(&joined).map_err(|e| e.to_string());
    }
    // Prefer literal match (steak: predictable).
    let lit = regex::escape(pat);
    Regex::new(&lit).map_err(|e| e.to_string())
}

fn floor_char(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn walk_grep(
    dir: &Path,
    repo_root: &Path,
    re: &Regex,
    matches: &mut Vec<serde_json::Value>,
    files_scanned: &mut usize,
    max_files: usize,
    max_matches: usize,
) {
    if matches.len() >= max_matches || *files_scanned >= max_files {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if matches.len() >= max_matches || *files_scanned >= max_files {
            break;
        }
        let p = e.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.')
                || name == "target"
                || name == "node_modules"
                || name == ".git"
                || name == "__pycache__"
            {
                continue;
            }
            walk_grep(
                &p,
                repo_root,
                re,
                matches,
                files_scanned,
                max_files,
                max_matches,
            );
        } else if p.is_file() {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !["rs", "py", "ts", "tsx", "js", "jsx", "go", "c", "h", "cpp", "md", "toml"]
                .contains(&ext)
            {
                continue;
            }
            *files_scanned += 1;
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let rel = p
                .strip_prefix(repo_root)
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_else(|_| p.to_string_lossy().into_owned());
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    matches.push(serde_json::json!({
                        "path": rel,
                        "abs_path": p.to_string_lossy(),
                        "line": i + 1,
                        "text": line.chars().take(220).collect::<String>(),
                    }));
                    if matches.len() >= max_matches {
                        return;
                    }
                }
            }
        }
    }
}

/// Format tool JSON for the next LLM turn — full enough to be useful, capped for cost.
pub fn format_tool_result_for_llm(action: &str, result: &ToolResult) -> String {
    let s = serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string());
    const MAX: usize = 3500;
    if s.len() <= MAX {
        format!("Tool {action} result:\n{s}")
    } else {
        format!(
            "Tool {action} result (truncated):\n{}…\n[truncated {} chars; ask a narrower path/pattern]",
            &s[..floor_char(&s, MAX)],
            s.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_joins_repo() {
        let tmp = tempfile_dir();
        let src = Source::new(tmp.clone(), None);
        let reg = ToolRegistry::with_source(src);
        let p = reg.resolve_path("src/main.rs");
        assert!(p.ends_with("src/main.rs"));
        assert!(p.starts_with(&tmp));
    }

    #[test]
    fn read_file_repo_relative() {
        let tmp = tempfile_dir();
        let f = tmp.join("hello.rs");
        std::fs::write(&f, "fn main() {\n  println!(\"hi\");\n}\n").unwrap();
        let reg = ToolRegistry::with_source(Source::new(tmp, None));
        let r = reg.dispatch(
            "read_file",
            &serde_json::json!({"path": "hello.rs", "max_lines": 10}),
        );
        assert_eq!(r["ok"], true);
        assert!(r["content"].as_str().unwrap().contains("fn main"));
    }

    #[test]
    fn grep_alternation_finds_fn_main() {
        let tmp = tempfile_dir();
        let src = tmp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.rs"), "fn main() {}\nstruct Config {}\n").unwrap();
        let reg = ToolRegistry::with_source(Source::new(tmp, None));
        let r = reg.dispatch(
            "grep",
            &serde_json::json!({"pattern": "fn main|struct Config", "scope": "src"}),
        );
        assert_eq!(r["ok"], true, "{r}");
        assert!(r["match_count"].as_u64().unwrap() >= 2, "{r}");
    }

    #[test]
    fn build_regex_alternation() {
        let re = build_search_regex("fn main|pub struct").unwrap();
        assert!(re.is_match("fn main() {}"));
        assert!(re.is_match("    pub struct Foo {"));
        // literal alternation is substring: "fn main" is contained in "fn mains"
        assert!(re.is_match("fn mains()"));
    }

    fn tempfile_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "butler_harv_tools_{}_{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
