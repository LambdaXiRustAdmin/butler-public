//! Scope path normalization: keep **file** scopes exact; dirs get trailing slash.

use crate::vprintln;
use code_graph::snooper::normalize_path;
use std::path::{Path, PathBuf};

/// Source extensions Butler scans (mirrors `scanner::should_scan_path`).
const KNOWN_SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "svelte", "go", "c", "h", "cpp", "hpp", "cc", "cxx",
];

fn has_known_source_extension(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| KNOWN_SOURCE_EXTENSIONS.contains(&ext))
}

fn ensure_dir_trailing_slash(path: &str) -> String {
    let p = normalize_path(path);
    if p.is_empty() || p.ends_with('/') {
        p
    } else {
        format!("{p}/")
    }
}

/// Collapse `.` and `..` segments (and drop leading `./`) for repo-relative scopes.
///
/// Agents pass `./src/` or `src/../src/`; without this, root-anchored matching misses
/// the same tree as `src/` (adversarial B4 soft).
pub fn collapse_scope_dotdot(path: &str) -> String {
    let raw = normalize_path(path);
    let abs = raw.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in raw.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    let joined = stack.join("/");
    if abs {
        format!("/{joined}")
    } else {
        joined
    }
}

fn resolve_scope_on_disk(project_root: &Path, scope: &str) -> PathBuf {
    let normalized = normalize_path(scope);
    if Path::new(&normalized).is_absolute() {
        PathBuf::from(normalized)
    } else {
        project_root.join(&normalized)
    }
}

/// True when this scope entry is a **scannable source file** (keep exact — do not widen).
/// Non-source files on disk (README, Makefile) are *not* kept — they heal to parent.
pub fn is_source_file_scope(scope: &str, project_root: &Path) -> bool {
    if has_known_source_extension(scope) {
        return true;
    }
    // Extensionless path that is still a real source file on disk is rare; only
    // treat as file-scope when the path ends with a known source suffix after resolve.
    let resolved = resolve_scope_on_disk(project_root, scope);
    resolved.is_file() && has_known_source_extension(&resolved.to_string_lossy())
}

/// Normalize `scope_paths` without destroying file binding.
///
/// - **Source file** scopes (`emcc.py`, `src/foo.rs`) stay as-is (normalized separators).
/// - **Directories** get a trailing `/` for prefix matching.
/// - Non-source disk files (README, Makefile) still heal to parent dir (legacy).
///
/// Returns `(original, normalized)` pairs when the string changed.
pub fn heal_scope_paths(
    scope_paths: &mut Option<Vec<String>>,
    project_root: &Path,
) -> Vec<(String, String)> {
    let Some(paths) = scope_paths.as_mut() else {
        return vec![];
    };

    let mut mutations = Vec::new();
    for scope in paths.iter_mut() {
        let original = scope.clone();
        // Normalize separators, then collapse ./ and .. before file/dir heal.
        let normalized = collapse_scope_dotdot(&original);

        if is_source_file_scope(&normalized, project_root) {
            // Bind to the file: LLMs pass `emcc.py` + target_symbol `main` for a reason.
            if *scope != normalized {
                mutations.push((original.clone(), normalized.clone()));
                *scope = normalized;
            }
            continue;
        }

        let resolved = resolve_scope_on_disk(project_root, &normalized);
        if resolved.is_file() {
            // Non-source file on disk → parent dir (can't parse README as blocks).
            let parent = resolved
                .parent()
                .map(|p| {
                    let rel = p
                        .strip_prefix(project_root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .to_string();
                    let rel = if rel.is_empty() { ".".to_string() } else { rel };
                    ensure_dir_trailing_slash(&normalize_path(&rel))
                })
                .unwrap_or_else(|| "./".to_string());
            if parent != *scope {
                mutations.push((original.clone(), parent.clone()));
                *scope = parent;
            }
            continue;
        }

        // Directory (or unknown path): trailing slash prefix scope.
        let dir = ensure_dir_trailing_slash(&normalized);
        if *scope != dir {
            mutations.push((original.clone(), dir.clone()));
            *scope = dir;
        }
    }
    for (from, to) in &mutations {
        vprintln!("scope_paths: [\"{}\"] → normalized to [\"{}\"]", from, to);
    }
    mutations
}

pub fn format_scope_paths_for_error(scope_paths: &Option<Vec<String>>) -> String {
    match scope_paths {
        Some(v) if !v.is_empty() => v.join(", "),
        _ => "(none)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "butler-scope-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn keeps_source_file_scope_exact() {
        let root = temp_root();
        let mut scopes = Some(vec!["emcc.py".to_string()]);
        let mutations = heal_scope_paths(&mut scopes, &root);
        assert!(mutations.is_empty() || mutations.iter().all(|(_, t)| t == "emcc.py"));
        assert_eq!(scopes.unwrap(), vec!["emcc.py"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn keeps_nested_source_file_scope() {
        let root = temp_root();
        let mut scopes = Some(vec!["pyo3-ffi/src/object.rs".to_string()]);
        let _ = heal_scope_paths(&mut scopes, &root);
        assert_eq!(scopes.unwrap(), vec!["pyo3-ffi/src/object.rs"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn directory_gets_trailing_slash() {
        let root = temp_root();
        fs::create_dir_all(root.join("cli/src")).unwrap();

        let mut scopes = Some(vec!["cli/src".to_string()]);
        let mutations = heal_scope_paths(&mut scopes, &root);
        assert!(!mutations.is_empty() || scopes.as_ref().unwrap()[0].ends_with('/'));
        assert_eq!(scopes.unwrap(), vec!["cli/src/"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collapses_dot_and_dotdot_scopes() {
        assert_eq!(collapse_scope_dotdot("./src/"), "src");
        assert_eq!(collapse_scope_dotdot("src/../src/"), "src");
        assert_eq!(collapse_scope_dotdot("a/b/../c"), "a/c");
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        let mut scopes = Some(vec!["./src/".to_string(), "src/../src".to_string()]);
        let _ = heal_scope_paths(&mut scopes, &root);
        assert_eq!(scopes.unwrap(), vec!["src/", "src/"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn non_source_file_heals_to_parent() {
        let root = temp_root();
        fs::write(root.join("README"), "hello").unwrap();

        let mut scopes = Some(vec!["README".to_string()]);
        let _ = heal_scope_paths(&mut scopes, &root);
        assert_eq!(scopes.unwrap(), vec!["./"]);
        let _ = fs::remove_dir_all(&root);
    }
}
