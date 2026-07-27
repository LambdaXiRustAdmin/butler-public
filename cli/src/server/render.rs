//! Presentation, formatting, and rendering helpers for Butler server responses.
//!
//! Extracted via Strangler Fig refactoring to keep the binary a thin Axum router.

use code_graph::{BlockInfo, CodeGraph, Id};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::server::discovery::{guess_lang_from_markers, has_any_marker};

/// Formats the results of `butler_search` as the exact Markdown required by the spec.
/// Enhanced for investigation use cases: when a graph is supplied we append a tiny
/// connectivity hint (direct callers/callees counts) so keyword search results
/// immediately surface "how connected is this symbol?" without forcing the user
/// to a separate surgical call. This helps close the gap vs. manual non-butler
/// analysis that naturally sees the full picture while grepping.
pub fn format_search_results_markdown(results: &[BlockInfo], graph: Option<&CodeGraph>) -> String {
    let mut out = String::with_capacity(2048);
    for (i, block) in results.iter().enumerate() {
        let sig = block
            .source
            .lines()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
            .split('{')
            .next()
            .unwrap_or(&block.source)
            .trim()
            .to_string();

        let mut conn = String::new();
        if let Some(g) = graph {
            let n_callers = g.callers(&block.id).len();
            let n_callees = g.children(&block.id).len();
            if n_callers > 0 || n_callees > 0 {
                conn = format!(" (callers:{}, callees:{})", n_callers, n_callees);
            }
        }

        out.push_str(&format!(
            "{}. `{}` — file: `{}`, line: {} (Score: {:.1}){}\n   Signature: `{}`\n",
            i + 1,
            block.name,
            block.file.display(),
            block.start_line,
            block.score,
            conn,
            sig
        ));
    }
    out
}

/// Renders a clean skeleton. Automatically applies "Semantic Zoom" for very large scopes
/// to protect smaller/local models from attention overload and excessive token usage.
/// - ≤ 150 blocks → Detailed view (files + symbols)
/// - > 150 blocks → High-level view (files only) + guidance
pub fn render_skeleton(
    blocks: &[&BlockInfo],
    graph: &CodeGraph,
    root: &std::path::Path,
    noise_cfg: &crate::server::filters::NoiseFilterConfig,
) -> String {
    if blocks.is_empty() {
        return "No items found in the requested scope.".to_string();
    }

    let block_count = blocks.len();
    let total_project_blocks = graph.nodes.len();
    let is_tiny_project = total_project_blocks < 50;

    // Semantic pruning: exclude test/doc/example noise from skeleton unless the *entire project*
    // is tiny (in which case every file is potentially relevant).
    // Delegates to filters::is_noise: exact noise *dir* names + real test *file* patterns.
    // Prefixes like test_repos are not noise (eval checkouts).
    let skeleton_blocks: Vec<&BlockInfo> = if is_tiny_project {
        blocks.to_vec()
    } else {
        blocks
            .iter()
            .copied()
            .filter(|b| !crate::server::filters::is_noise(b, root, noise_cfg))
            .collect()
    };
    let pruned_count = block_count - skeleton_blocks.len();

    let mut out = String::new();

    if block_count <= 150 {
        // === Zoom Level 1: Detailed (small scope) ===
        out.push_str("=== Scope Skeleton (Detailed) ===\n\n");
        out.push_str(&format!("Items in scope: {}\n\n", block_count));

        let mut by_file: HashMap<String, Vec<&BlockInfo>> = HashMap::new();
        for b in &skeleton_blocks {
            let file = b.file.to_string_lossy().to_string();
            by_file.entry(file).or_default().push(b);
        }

        for (file, file_blocks) in by_file {
            out.push_str(&format!("📁 `{}` ({} items)\n", file, file_blocks.len()));
            for b in file_blocks {
                out.push_str(&format!("   • `{}` (line {})\n", b.name, b.start_line));
            }
            out.push('\n');
        }
    } else {
        // === Zoom Level 2: High-level (large scope) ===
        out.push_str("=== Architecture Skeleton (Broad Scope) ===\n\n");
        out.push_str(&format!(
            "⚠️ Scope is large ({} items). Showing file structure only.\n",
            block_count
        ));
        out.push_str("Narrow your `scope_paths` for more detail.\n\n");

        let mut files: Vec<String> = skeleton_blocks
            .iter()
            .map(|b| b.file.to_string_lossy().to_string())
            .collect();
        files.sort();
        files.dedup();

        for file in files {
            out.push_str(&format!("📁 `{}`\n", file));
        }
    }

    // Scoped hubs — always shown, but only those inside the current Working Set
    // Vendored bloat (external/headers/deps) is heavily down-scored so user code dominates top hubs.
    // Noise (tests/docs/etc.) excluded for consistency with ArchitecturalSummary + centralized filters.
    if !graph.highly_connected_nodes.is_empty() {
        let mut scoped_hubs: Vec<&BlockInfo> = blocks
            .iter()
            .copied()
            .filter(|b| {
                graph.highly_connected_nodes.contains(&b.id)
                    && !crate::server::filters::is_noise(b, root, noise_cfg)
            })
            .collect();

        scoped_hubs.sort_by(|a, b| {
            let score_a = if crate::server::filters::is_vendored(&a.file) {
                a.score * 0.1
            } else {
                a.score
            };
            let score_b = if crate::server::filters::is_vendored(&b.file) {
                b.score * 0.1
            } else {
                b.score
            };
            score_b
                .partial_cmp(&score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scoped_hubs.truncate(5);

        if !scoped_hubs.is_empty() {
            out.push_str("\n🔥 Highly connected hubs IN THIS SCOPE:\n");
            for h in scoped_hubs {
                out.push_str(&format!("   • `{}`\n", h.name));
            }
        }
    }

    // Semantic pruning summary (always emitted so LLMs know the view was intentionally curated)
    out.push_str(&format!(
        "[... Pruned {} test/doc/example files for brevity. Use scope_paths to view them directly.]\n",
        pruned_count
    ));

    out
}

/// Render a simple 2-level file tree for the project (used by `butler_orchestrate`
/// ArchitecturalSummary when no/blank `scope_paths` provided).
///
/// Shows top-level files and directories (depth 1), plus their immediate children (depth 2).
/// Only includes source files tracked by the graph (respects .butlerignore etc).
/// This provides a clean high-level view instead of a huge skeleton or "no items".
pub fn render_2level_file_tree(blocks: &[&BlockInfo]) -> String {
    if blocks.is_empty() {
        return "**Project file tree (2 levels):**\n\n(no source files)\n".to_string();
    }

    // Block files often use "container" paths like /projects/... while the req root may be "."
    // Compute a robust relative by stripping the common path prefix shared by the files.
    let norm_files: Vec<String> = blocks
        .iter()
        .map(|b| b.file.to_string_lossy().replace('\\', "/"))
        .collect();

    // Find longest common dir prefix
    let mut common = norm_files[0].clone();
    for f in &norm_files[1..] {
        let mut c = String::new();
        for (ca, cb) in common.chars().zip(f.chars()) {
            if ca == cb {
                c.push(ca);
            } else {
                break;
            }
        }
        if let Some(pos) = c.rfind('/') {
            common = c[..=pos].to_string();
        } else {
            common.clear();
            break;
        }
    }
    if !common.ends_with('/') && !common.is_empty() {
        if let Some(pos) = common.rfind('/') {
            common = common[..=pos].to_string();
        }
    }

    let mut top_level_dirs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut top_level_files: BTreeSet<String> = BTreeSet::new();

    for file_norm in &norm_files {
        let rel = if !common.is_empty() && file_norm.starts_with(&common) {
            file_norm[common.len()..]
                .trim_start_matches('/')
                .to_string()
        } else {
            // fallback: use last two path segments
            let ps: Vec<&str> = file_norm.rsplit('/').take(2).collect();
            ps.into_iter().rev().collect::<Vec<_>>().join("/")
        };
        if rel.is_empty() {
            continue;
        }
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        if parts.len() == 1 {
            top_level_files.insert(parts[0].to_string());
        } else {
            let dir = parts[0].to_string();
            let child = parts[1].to_string();
            top_level_dirs.entry(dir).or_default().push(child);
        }
    }

    for children in top_level_dirs.values_mut() {
        children.sort();
        children.dedup();
        if children.len() > 8 {
            children.truncate(7);
            children.push("...".to_string());
        }
    }

    let mut out = String::new();
    out.push_str("**Project file tree (2 levels):**\n\n");
    out.push_str(".\n");

    let mut tops: Vec<String> = top_level_files.iter().cloned().collect();
    tops.extend(top_level_dirs.keys().cloned());
    tops.sort();
    tops.dedup();

    for (i, top) in tops.iter().enumerate() {
        let last = i == tops.len() - 1;
        let branch = if last { "└── " } else { "├── " };
        if top_level_files.contains(top) {
            out.push_str(&format!("{}{}\n", branch, top));
        } else if let Some(kids) = top_level_dirs.get(top) {
            out.push_str(&format!("{}{}/\n", branch, top));
            let indent = if last { "    " } else { "│   " };
            for (j, kid) in kids.iter().enumerate() {
                let klast = j == kids.len() - 1;
                let kbranch = if klast { "└── " } else { "├── " };
                let display = if kid.contains('.') {
                    kid.clone()
                } else {
                    format!("{}/", kid)
                };
                out.push_str(&format!("{}{}{}\n", indent, kbranch, display));
            }
        }
    }

    out.push_str("\n(Provide `scope_paths` such as [\"src/\"] or [\"cli/\", \"code_graph/src/\"] for a focused ArchitecturalSummary.)\n");
    out
}

fn mermaid_edge_label(relation: Option<&str>) -> String {
    match relation {
        Some(r) if !r.is_empty() => format!("-->|{r}|"),
        _ => "-->".to_string(),
    }
}

/// Mermaid from the **same** packed CallerCallee lists as dense/structured (dossier).
/// Caps display at 12/side for URL size; notes omitted counts from the packer.
pub fn build_trace_mermaid_packed(
    target: &BlockInfo,
    callers: &[crate::server::dto::CallerCallee],
    callees: &[crate::server::dto::CallerCallee],
    callers_omitted: usize,
    callees_omitted: usize,
) -> String {
    let mut m = String::from("graph LR\n");
    let t_id = "target";
    let t_label = format!(
        "{} ({})",
        sanitize_mermaid_label(&target.name),
        file_basename(&target.file)
    );
    m.push_str(&format!("    {}[\"{}\"]\n", t_id, t_label));

    const SHOW: usize = 12;
    for (c_idx, cc) in callers.iter().take(SHOW).enumerate() {
        let id = format!("c{c_idx}");
        let label = format!(
            "{} ({})",
            sanitize_mermaid_label(&cc.name),
            file_basename(std::path::Path::new(&cc.file))
        );
        let edge = mermaid_edge_label(cc.relation.as_deref());
        m.push_str(&format!("    {}[\"{}\"] {} {}\n", id, label, edge, t_id));
    }
    let more_c = callers.len().saturating_sub(SHOW) + callers_omitted;
    if more_c > 0 {
        m.push_str(&format!(
            "    cnote[\"...and {more_c} more callers\"]\n"
        ));
    }

    for (e_idx, cc) in callees.iter().take(SHOW).enumerate() {
        let id = format!("e{e_idx}");
        let label = format!(
            "{} ({})",
            sanitize_mermaid_label(&cc.name),
            file_basename(std::path::Path::new(&cc.file))
        );
        let edge = mermaid_edge_label(cc.relation.as_deref());
        m.push_str(&format!("    {} {} {}[\"{}\"]\n", t_id, edge, id, label));
    }
    let more_e = callees.len().saturating_sub(SHOW) + callees_omitted;
    if more_e > 0 {
        m.push_str(&format!(
            "    enote[\"...and {more_e} more callees\"]\n"
        ));
    }
    m
}

/// Build a rich, safe Mermaid flowchart from depth-id blast data (legacy / tests).
/// Prefer [`build_trace_mermaid_packed`] for orchestrate so dense and mermaid match.
#[allow(dead_code)]
pub fn build_trace_mermaid(
    graph: &CodeGraph,
    target: &BlockInfo,
    callers_by_depth: &[Vec<Id>],
    callees_by_depth: &[Vec<Id>],
) -> String {
    let mut m = String::from("graph LR\n");

    let t_id = "target";
    let t_label = format!(
        "{} ({})",
        sanitize_mermaid_label(&target.name),
        file_basename(&target.file)
    );
    m.push_str(&format!("    {}[\"{}\"]\n", t_id, t_label));

    let mut c_idx = 0usize;
    for level in callers_by_depth {
        for node_id in level {
            if c_idx >= 12 {
                break;
            }
            let Some(b) = graph.get_block(node_id.clone()) else {
                continue;
            };
            let id = format!("c{}", c_idx);
            let label = format!(
                "{} ({})",
                sanitize_mermaid_label(&b.name),
                file_basename(&b.file)
            );
            m.push_str(&format!("    {}[\"{}\"] --> {}\n", id, label, t_id));
            c_idx += 1;
        }
        if c_idx >= 12 {
            break;
        }
    }
    let total_c: usize = callers_by_depth.iter().map(|v| v.len()).sum();
    if total_c > c_idx {
        m.push_str(&format!(
            "    cnote[\"...and {} more callers\"]\n",
            total_c - c_idx
        ));
    }

    let mut e_idx = 0usize;
    for level in callees_by_depth {
        for node_id in level {
            if e_idx >= 12 {
                break;
            }
            let Some(b) = graph.get_block(node_id.clone()) else {
                continue;
            };
            let id = format!("e{}", e_idx);
            let label = format!(
                "{} ({})",
                sanitize_mermaid_label(&b.name),
                file_basename(&b.file)
            );
            m.push_str(&format!("    {} --> {}[\"{}\"]\n", t_id, id, label));
            e_idx += 1;
        }
        if e_idx >= 12 {
            break;
        }
    }
    let total_e: usize = callees_by_depth.iter().map(|v| v.len()).sum();
    if total_e > e_idx {
        m.push_str(&format!(
            "    enote[\"...and {} more callees\"]\n",
            total_e - e_idx
        ));
    }

    m
}

fn file_basename(p: &std::path::Path) -> String {
    p.file_name()
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

fn sanitize_mermaid_label(s: &str) -> String {
    // Mermaid labels in ["text"] are forgiving, but sanitize quotes and newlines.
    s.replace('"', "'")
        .replace(['\n', '\r'], " ")
        .trim()
        .to_string()
}

/// Renders the shallow marker discovery listing (presentation layer).
pub fn render_shallow_marker_discovery(root: &std::path::Path) -> String {
    let mut out = String::new();
    let root_exists = root.exists();
    let root_is_dir = root.is_dir();

    // Loud fail when path is missing / unreadable (common Docker vs host path mistakes).
    if !root_exists || !root_is_dir {
        out.push_str("⚠ **Does not appear to be a valid project path** (missing or not a directory).\n\n");
        out.push_str(&format!("Resolved root: `{}`\n\n", root.display()));
        out.push_str("**What is `project`?** An **absolute** folder this Butler server can see, that contains real code\n");
        out.push_str("(e.g. `Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`) or a clear package tree.\n\n");
        out.push_str("- **Docker Butler:** use container paths like `/projects/test_repos/bat` (not `/home/...` unless mounted).\n");
        out.push_str("- **Host-native Butler:** use the real host path, e.g. `/home/<you>/projects/…`.\n");
        out.push_str("- Prefer the **project dropdown** on `/setup` when roots are already loaded.\n");
        if !root_exists {
            out.push_str("\nThis path does not exist on the server filesystem.\n");
        }
        return out;
    }

    out.push_str("**Project Discovery (shallow 2-level filesystem listing — no Tree-sitter scan performed):**\n\n");
    out.push_str(&format!("Resolved root: {}\n\n", root.display()));
    out.push_str("**Folders containing project markers are highlighted:**\n\n");

    // Extend: also surface the root itself when it has a marker (e.g. when legacy
    // butler_context is called with project: "." which resolves to a project root).
    // This reuses the marker helpers for consistency with previous shallow discovery.
    // Resolve "." to actual cwd for better name/full-path display in legacy discovery case.
    let effective_for_root = if root.to_string_lossy() == "." {
        std::env::current_dir().unwrap_or_else(|_| root.to_path_buf())
    } else {
        root.to_path_buf()
    };
    let mut marker_hits = 0usize;
    if has_any_marker(root) {
        marker_hits += 1;
        let lang = guess_lang_from_markers(root);
        let name = effective_for_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("current");
        out.push_str(&format!(
            "- **{} ({})** — {}\n",
            name,
            lang,
            effective_for_root.display()
        ));
    }

    let mut top: Vec<_> = if let Ok(rd) = std::fs::read_dir(root) {
        rd.filter_map(|e| e.ok()).take(40).collect()
    } else {
        vec![]
    };
    top.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());

    if top.is_empty() && marker_hits == 0 {
        out.push_str("(no entries / no project markers under this path)\n\n");
        out.push_str("⚠ **Does not appear to be a useful project root for Butler.**\n");
        out.push_str("Pick a folder that contains source + a project marker, or use a path the **server** can see.\n");
        return out;
    }

    for e in top {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if has_any_marker(&p) {
                marker_hits += 1;
                let lang = guess_lang_from_markers(&p);
                out.push_str(&format!("- **{} ({})** — {}\n", name, lang, p.display()));
                // level 2 under it
                if let Ok(srd) = std::fs::read_dir(&p) {
                    let mut subs: Vec<_> = srd.filter_map(|se| se.ok()).take(8).collect();
                    subs.sort_by_key(|se| se.file_name().to_string_lossy().to_lowercase());
                    for se in subs {
                        let sp = se.path();
                        let sname = se.file_name().to_string_lossy().to_string();
                        if sp.is_dir() && has_any_marker(&sp) {
                            marker_hits += 1;
                            let slang = guess_lang_from_markers(&sp);
                            out.push_str(&format!(
                                "  - **{} ({})** — {}\n",
                                sname,
                                slang,
                                sp.display()
                            ));
                        } else {
                            out.push_str(&format!("  - {}/\n", sname));
                        }
                    }
                }
            } else {
                out.push_str(&format!("- {}/\n", name));
                // check depth-2 subs for markers
                if let Ok(srd) = std::fs::read_dir(&p) {
                    for se in srd.filter_map(|se| se.ok()).take(6) {
                        let sp = se.path();
                        if sp.is_dir() && has_any_marker(&sp) {
                            marker_hits += 1;
                            let slang = guess_lang_from_markers(&sp);
                            let sname = se.file_name().to_string_lossy().to_string();
                            out.push_str(&format!(
                                "  - **{} ({})** — {}\n",
                                sname,
                                slang,
                                sp.display()
                            ));
                        }
                    }
                }
            }
        } else {
            out.push_str(&format!("- {}\n", name));
        }
    }

    if marker_hits == 0 {
        out.push_str("\n⚠ **No project markers found under this path** (no Cargo.toml / package.json / go.mod / …).\n");
        out.push_str("This may still be the wrong root, or empty. Prefer a repo root the server can see.\n");
        out.push_str("Docker: `/projects/…` · Host: `/home/<you>/projects/…` · Or pick from `/setup` dropdown.\n");
    } else {
        out.push_str("\nProvide one of the **highlighted** folders as your `project` (or inside `scope_paths`).\n\n");
        out.push_str("Example:\n```json\n{\n  \"mcp_tool_name\": \"butler_orchestrate\",\n  \"goal\": \"ArchitecturalSummary\",\n  \"project\": \"/path/to/your-repo\"\n}\n```\n");
        out.push_str("For TraceBlastRadius use e.g. \"goal\": \"TraceBlastRadius\", \"target_symbol\": \"some_func\".\n");
    }
    out
}

#[cfg(test)]
mod file_tree_tests {
    use super::render_2level_file_tree;
    use code_graph::snooper::model::{BlockInfo, Id};
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn blk(file: &str) -> BlockInfo {
        BlockInfo {
            id: Id::new(file, "function_item", "abcdef01"),
            name: "f".into(),
            file: PathBuf::from(file),
            kind: "function_item".into(),
            lang: "rs".into(),
            start_line: 1,
            end_line: 2,
            start_byte: 0,
            end_byte: 1,
            parent_id: None,
            children: vec![],
            content_hash: "h".into(),
            sig_hash: "s".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            score: 0.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        }
    }

    #[test]
    fn two_level_tree_mentions_src() {
        let a = blk("src/main.rs");
        let b = blk("src/lib.rs");
        let refs = vec![&a, &b];
        let s = render_2level_file_tree(&refs);
        assert!(s.contains("Project file tree"), "{s}");
        assert!(s.contains("src") || s.contains("main"), "{s}");
    }
}
