//! Warehouse language honesty — detect **false Complete** on unsupported product langs.
//!
//! Butler inventories only Tree-sitter-backed extensions (rs/py/ts/js/go/c/cpp/…).
//! A Java/Kotlin monorepo can still "Complete" on a handful of JS crumbs and serve
//! nonsense hubs. This module censuses on-disk code-like extensions and flags a
//! **lang void** when unsupported product code dominates scannable inventory.

use crate::snooper::path_policy::{is_bundled_vendor_dir_segment, is_infra_prune_dir_segment};
use crate::snooper::utils::normalize_path;
use jwalk::WalkDir as JWalkDir;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Extensions Butler parses today (must stay in sync with `should_scan_path` exts).
pub const SUPPORTED_SOURCE_EXTS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "svelte", "go", "c", "h", "cpp", "hpp", "cc", "cxx",
];

/// Common product languages Butler does **not** parse — dominant presence ⇒ lang void.
pub const UNSUPPORTED_PRODUCT_EXTS: &[&str] = &[
    "java", "kt", "kts", "cs", "rb", "php", "swift", "scala", "groovy", "m", "mm",
];

/// Disk census of code-like files under a project root (after skip/prune).
#[derive(Debug, Clone, Default)]
pub struct CodeExtCensus {
    pub supported: usize,
    /// Lowercase extension → file count (unsupported product only).
    pub unsupported: HashMap<String, usize>,
}

impl CodeExtCensus {
    pub fn unsupported_total(&self) -> usize {
        self.unsupported.values().sum()
    }

    pub fn dominant_unsupported(&self) -> Option<(String, usize)> {
        self.unsupported
            .iter()
            .max_by_key(|(_, n)| *n)
            .map(|(e, n)| (e.clone(), *n))
    }
}

/// Persistent diagnosis: warehouse is Complete for *scanned* crumbs, not product language.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WarehouseLangVoid {
    /// Dominant unscanned product extension (e.g. `java`).
    pub dominant_ext: String,
    pub unsupported_files: usize,
    pub supported_files: usize,
    /// Agent-facing one-liner.
    pub message: String,
}

impl WarehouseLangVoid {
    pub fn user_message(&self, project: &str) -> String {
        format!(
            "=== Butler Error ===\n\
             Warehouse lang void for '{}': on-disk product code is mostly **.{}** \
             ({} files) but Butler only scanned {} supported-language file(s). \
             Graph hubs/edges for this root are **not product-faithful** (often JS/tooling crumbs).\n\n\
             Supported today: Rust, Python, TypeScript/JavaScript, Go, C/C++.\n\
             How to fix:\n\
             - Point `project` at a sub-tree Butler can parse, or\n\
             - Use a dual-stack package root that is mostly supported langs, or\n\
             - Track Java/Kotlin support as a separate language Track (not available yet).",
            project,
            self.dominant_ext,
            self.unsupported_files,
            self.supported_files
        )
    }
}

fn is_supported_ext(ext: &str) -> bool {
    SUPPORTED_SOURCE_EXTS
        .iter()
        .any(|e| e.eq_ignore_ascii_case(ext))
}

fn is_unsupported_product_ext(ext: &str) -> bool {
    UNSUPPORTED_PRODUCT_EXTS
        .iter()
        .any(|e| e.eq_ignore_ascii_case(ext))
}

fn dir_name_pruned(name: &str) -> bool {
    let n = name.trim_end_matches('/');
    is_infra_prune_dir_segment(n) || is_bundled_vendor_dir_segment(n)
}

fn path_matches_skip(path_for_policy: &str, skip_patterns: &[String]) -> bool {
    let p = if path_for_policy.starts_with('/') {
        path_for_policy.trim_end_matches('/').to_string()
    } else if path_for_policy.is_empty() {
        return false;
    } else {
        format!("/{}", path_for_policy.trim_end_matches('/'))
    };
    skip_patterns.iter().any(|pat| {
        let seg = pat.trim_matches('/');
        if seg.is_empty() {
            return false;
        }
        let needle = format!("/{seg}");
        p.ends_with(&needle) || p.contains(&format!("{needle}/"))
    })
}

fn path_for_skip_policy(path: &Path, root: &Path) -> String {
    let path_str = normalize_path(&path.to_string_lossy());
    let root_str = normalize_path(&root.to_string_lossy());
    let root_trim = root_str.trim_end_matches('/');
    if path_str == root_trim {
        return String::new();
    }
    if let Some(rest) = path_str.strip_prefix(root_trim) {
        if rest.is_empty() {
            return String::new();
        }
        if rest.starts_with('/') {
            return rest.trim_start_matches('/').to_string();
        }
    }
    path_str
}

/// Count supported + unsupported product code files under `root` (jwalk + same-ish prune).
pub fn census_code_extensions(root: &Path, skip_patterns: &[String]) -> CodeExtCensus {
    let skip_for_prune = skip_patterns.to_vec();
    let root_owned = root.to_path_buf();
    let mut census = CodeExtCensus::default();

    for entry in JWalkDir::new(root)
        .skip_hidden(false)
        .process_read_dir(move |_depth, path, _state, children| {
            let parent_str = normalize_path(&path.to_string_lossy());
            children.retain(|e| {
                let Ok(ent) = e else {
                    return true;
                };
                if !ent.file_type.is_dir() {
                    return true;
                }
                let name = ent.file_name.to_string_lossy();
                if dir_name_pruned(&name) {
                    return false;
                }
                let child_str = format!("{}/{}", parent_str.trim_end_matches('/'), name);
                let policy = path_for_skip_policy(Path::new(&child_str), &root_owned);
                // Segment name exact + path skip patterns.
                let n = name.trim_end_matches('/');
                if skip_for_prune.iter().any(|pat| {
                    let pat = pat.trim_matches('/');
                    !pat.is_empty() && (n == pat || {
                        let p = if policy.starts_with('/') {
                            policy.clone()
                        } else {
                            format!("/{policy}")
                        };
                        let needle = format!("/{pat}");
                        p.ends_with(&needle) || p.contains(&format!("{needle}/"))
                    })
                }) {
                    return false;
                }
                true
            });
        })
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        let policy = path_for_skip_policy(&path, root);
        if path_matches_skip(&policy, skip_patterns) {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let ext_l = ext.to_ascii_lowercase();
        if is_supported_ext(&ext_l) {
            census.supported += 1;
        } else if is_unsupported_product_ext(&ext_l) {
            *census.unsupported.entry(ext_l).or_insert(0) += 1;
        }
    }
    census
}

/// True when on-disk product language is mostly unscanned — false Complete risk.
///
/// Heuristic (spring-boot shape): many unsupported files and supported inventory is
/// tiny or dominated by the unsupported bulk (ratio ≥ 4×).
pub fn assess_lang_void(census: &CodeExtCensus) -> Option<WarehouseLangVoid> {
    let unsup = census.unsupported_total();
    let supported = census.supported;
    if unsup < 50 {
        return None;
    }
    let (dom, dom_n) = census.dominant_unsupported()?;
    // Dominant family should be a real bulk (not 50 random).
    if dom_n < 40 {
        return None;
    }
    // Supported is crumbs relative to product bulk.
    let dominated = unsup >= supported.saturating_mul(4).max(50);
    let crumbs = supported < 32 && unsup >= 50;
    if !(dominated || crumbs) {
        return None;
    }
    Some(WarehouseLangVoid {
        dominant_ext: dom.clone(),
        unsupported_files: unsup,
        supported_files: supported,
        message: format!(
            "lang_void: .{dom}×{unsup} on disk vs {supported} Butler-scanned file(s) — warehouse not product-faithful"
        ),
    })
}

/// Re-assess using census; prefer existing void if census agrees.
pub fn refresh_lang_void(
    root: &Path,
    skip_patterns: &[String],
    prior: Option<&WarehouseLangVoid>,
) -> Option<WarehouseLangVoid> {
    let census = census_code_extensions(root, skip_patterns);
    let fresh = assess_lang_void(&census);
    if fresh.is_some() {
        return fresh;
    }
    // Clear prior void if census no longer supports it (e.g. after real language support).
    let _ = prior;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_shaped_java_void() {
        let mut unsup = HashMap::new();
        unsup.insert("java".into(), 8673);
        unsup.insert("kt".into(), 446);
        let census = CodeExtCensus {
            supported: 3,
            unsupported: unsup,
        };
        let v = assess_lang_void(&census).expect("void");
        assert_eq!(v.dominant_ext, "java");
        assert!(v.unsupported_files > 8000);
        assert_eq!(v.supported_files, 3);
    }

    #[test]
    fn django_shaped_no_void() {
        let census = CodeExtCensus {
            supported: 942,
            unsupported: HashMap::new(),
        };
        assert!(assess_lang_void(&census).is_none());
    }

    #[test]
    fn mixed_polyglot_minor_java_no_void() {
        // microservices: some java, mostly go/py — should not void
        let mut unsup = HashMap::new();
        unsup.insert("java".into(), 40);
        let census = CodeExtCensus {
            supported: 48,
            unsupported: unsup,
        };
        assert!(assess_lang_void(&census).is_none());
    }

    #[test]
    fn user_message_mentions_ext() {
        let v = WarehouseLangVoid {
            dominant_ext: "java".into(),
            unsupported_files: 100,
            supported_files: 2,
            message: "x".into(),
        };
        let m = v.user_message("/projects/test_repos/spring-boot");
        assert!(m.contains("java"));
        assert!(m.contains("Warehouse lang void") && m.contains("java"));
    }
}
