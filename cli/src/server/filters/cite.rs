//! Cite pack, CallerCallee stamping, blank scope / word-boundary helpers.
use crate::server::dto::CallerCallee;
use code_graph::BlockInfo;

pub fn contains_word_boundary(text: &str, word: &str) -> bool {
    if word.is_empty() || !text.contains(word) {
        return false;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let bytes = text.as_bytes();
    let mut start = 0usize;
    while let Some(pos) = text[start..].find(word) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_word(bytes[abs - 1] as char);
        let after_pos = abs + word.len();
        let after_ok = after_pos == bytes.len() || !is_word(bytes[after_pos] as char);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

/// Vendored/external bloat heuristic (down-score hubs under vendor/deps/third_party etc).
///
/// **Segment-exact** via [`code_graph::is_bundled_vendor_dir_segment`] (built-in
/// bundled-vendor skip list). Never substring `vendor` / bare `deps`.
/// Extracted from duplicate closures in render.rs and orchestrate.rs.
pub fn is_vendored(p: &std::path::PathBuf) -> bool {
    let s = p.to_string_lossy().replace('\\', "/");
    s.split('/')
        .filter(|seg| !seg.is_empty())
        .any(code_graph::is_bundled_vendor_dir_segment)
}

/// Blank scope heuristic: treats None, empty, or trivial paths (".", "/", etc) as "no explicit scope".
/// Extracted to eliminate duplication between context_engine.rs (fn) and orchestrate.rs (inline).
pub fn is_blank_scope(scope_paths: &Option<Vec<String>>) -> bool {
    scope_paths.as_ref().map_or(true, |v| {
        v.is_empty()
            || v.iter().all(|s| {
                let t = s.trim();
                t.is_empty() || t == "." || t == "./" || t == ".\\" || t == "/"
            })
    })
}

/// Stamp lang + cluster workbench on a CallerCallee from a graph block.
///
/// `hop` is 1-based distance from the Trace seed (1 = direct edge). Prefer
/// [`caller_callee_from_block_at_hop`] when the BFS level is known.
pub fn caller_callee_from_block(
    b: &BlockInfo,
    pp: &code_graph::ProjectPaths,
) -> CallerCallee {
    caller_callee_from_block_at_hop(b, pp, 1)
}

/// Like [`caller_callee_from_block`] with an explicit hop (1 = direct, 2 = L2, …).
pub fn caller_callee_from_block_at_hop(
    b: &BlockInfo,
    pp: &code_graph::ProjectPaths,
    hop: u8,
) -> CallerCallee {
    let cluster = code_graph::cluster_for_block(b);
    CallerCallee {
        name: b.name.clone(),
        file: pp.to_display(&b.file),
        line: b.start_line,
        hop: hop.max(crate::server::dto::default_hop()),
        lang: Some(code_graph::normalize_lang_label(&b.lang)),
        cluster: Some(cluster.badge().to_string()),
        relation: None,
        cite: cite_snippet_from_source(&b.source),
        why: None,
    }
}

/// Cite pack: ≤6 lines / ~400 chars from block source (silence when stripped/empty).
pub fn cite_snippet_from_source(source: &str) -> Option<String> {
    let t = source.trim();
    if t.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut lines = 0usize;
    for line in t.lines() {
        if lines >= 6 || out.len() >= 400 {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        // Keep lines short for agent→user quotes
        let clipped: String = line.chars().take(120).collect();
        out.push_str(&clipped);
        if line.chars().count() > 120 {
            out.push('…');
        }
        lines += 1;
    }
    if out.is_empty() {
        None
    } else {
        if t.lines().count() > lines || source.len() > out.len() + 8 {
            out.push_str("\n…");
        }
        Some(out)
    }
}

/// Resolve a display/host/container/rel path to a readable absolute file under root.
///
/// **Bug class fixed:** `to_display` rewrites container abs → host abs for agents.
/// Inside Docker that host path does not exist; we must map back via
/// [`ProjectPaths::to_rel`] then [`to_abs`].
pub fn resolve_cite_abs_path(
    project_root: &std::path::Path,
    display_or_rel_file: &str,
) -> Option<std::path::PathBuf> {
    let pp = code_graph::ProjectPaths::new(project_root);
    let candidates = [
        pp.to_abs(std::path::Path::new(display_or_rel_file)),
        // display path may already be absolute host form
        std::path::PathBuf::from(display_or_rel_file),
        // last-N segment short_path form: try under root as-is
        pp.root().join(display_or_rel_file.trim_start_matches('/')),
    ];
    for abs in candidates {
        if abs.is_file() {
            return Some(abs);
        }
    }
    // short_path style: "components/Admin/EditUser.tsx" under a monorepo frontend/
    let short = display_or_rel_file.replace('\\', "/");
    let segs: Vec<&str> = short.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() >= 2 && segs.len() <= 4 {
        for prefix in ["frontend/src", "src", "app", "packages"] {
            let try_p = pp.root().join(prefix).join(segs.join("/"));
            if try_p.is_file() {
                return Some(try_p);
            }
        }
        // walk shallow for basename match under root (bounded)
        let base = *segs.last()?;
        if let Ok(walk) = std::fs::read_dir(pp.root()) {
            for ent in walk.flatten().take(32) {
                let p = ent.path();
                if p.is_dir() {
                    let cand = p.join("src").join(segs.join("/"));
                    if cand.is_file() {
                        return Some(cand);
                    }
                    let cand2 = p.join(segs.join("/"));
                    if cand2.is_file() {
                        return Some(cand2);
                    }
                }
            }
        }
        let _ = base;
    }
    None
}

/// Disk fallback for slim warehouses: read ~6 lines starting at `start_line` (1-based).
pub fn cite_snippet_from_disk(
    project_root: &std::path::Path,
    display_or_rel_file: &str,
    start_line: usize,
) -> Option<String> {
    if start_line == 0 {
        return None;
    }
    let abs = resolve_cite_abs_path(project_root, display_or_rel_file)?;
    let text = std::fs::read_to_string(&abs).ok()?;
    let start = start_line.saturating_sub(1);
    let mut out = String::new();
    let mut n = 0usize;
    for line in text.lines().skip(start) {
        if n >= 6 || out.len() >= 400 {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        let clipped: String = line.chars().take(120).collect();
        out.push_str(&clipped);
        if line.chars().count() > 120 {
            out.push('…');
        }
        n += 1;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Fill empty cites on top neighbors from disk (slim Complete warehouses).
pub fn fill_cites_from_disk(
    items: &mut [CallerCallee],
    project_root: &std::path::Path,
    max: usize,
) {
    for c in items.iter_mut().take(max) {
        if c.cite.as_ref().is_some_and(|s| !s.trim().is_empty()) {
            continue;
        }
        if let Some(s) = cite_snippet_from_disk(project_root, &c.file, c.line) {
            c.cite = Some(s);
        }
    }
}

#[cfg(test)]
mod cite_pack_tests {
    use super::*;

    #[test]
    fn cite_snippet_caps_lines_and_width() {
        let src = "fn a() {\n    let x = 1;\n    let y = 2;\n    let z = 3;\n    let w = 4;\n    let v = 5;\n    let u = 6;\n    let t = 7;\n}\n";
        let c = cite_snippet_from_source(src).expect("cite");
        assert!(c.lines().count() <= 7, "{c}"); // 6 + optional …
        assert!(c.contains("fn a()"));
    }

    #[test]
    fn cite_snippet_empty_on_blank() {
        assert!(cite_snippet_from_source("   \n  ").is_none());
    }

    #[test]
    fn resolve_cite_abs_prefers_repo_relative_under_root() {
        let root = std::env::temp_dir().join(format!(
            "butler_cite_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let rel = root.join("frontend/src/components/Admin/EditUser.tsx");
        std::fs::create_dir_all(rel.parent().unwrap()).unwrap();
        std::fs::write(&rel, "export function EditUser() {\n  return null\n}\n").unwrap();
        // warehouse relative
        let a = resolve_cite_abs_path(&root, "frontend/src/components/Admin/EditUser.tsx")
            .expect("rel");
        assert_eq!(a, rel);
        // short_path form (last 3 segments) must still resolve
        let b = resolve_cite_abs_path(&root, "components/Admin/EditUser.tsx").expect("short");
        assert_eq!(b, rel);
        let _ = std::fs::remove_dir_all(&root);
    }
}
