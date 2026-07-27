//! Project root anchor: repo-relative warehouse paths + boundary translation.
//!
//! **Invariant:** anything stored in CodeGraph (block.file, file_hashes keys, name_index)
//! is **repo-relative** under [`ProjectPaths::root`]. Absolute host/container paths are only
//! used at I/O and client-display edges via [`ProjectPaths::to_abs`] / [`to_display`].
//!
//! **Root** is the package/manifest directory (Cargo.toml / pyproject / …), not the Docker
//! mount and not a parent eval forest (`test_repos/`). Relative keys must never lose a
//! package directory that shares the project folder name (`…/typer` + `typer/main.py`).

use std::path::{Path, PathBuf};

use super::utils::normalize_path;

/// Per-project path anchor (one per warm graph root).
#[derive(Debug, Clone)]
pub struct ProjectPaths {
    /// Absolute (or best-effort) project root, normalized `/`.
    root: PathBuf,
    root_norm: String,
}

impl ProjectPaths {
    /// Build an anchor for `root` (project directory). Does not require the path to exist.
    pub fn new(root: impl AsRef<Path>) -> Self {
        let raw = root.as_ref();
        let abs = std::fs::canonicalize(raw).unwrap_or_else(|_| {
            if raw.is_absolute() {
                raw.to_path_buf()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(raw)
            }
        });
        let root_norm = normalize_path(&abs.to_string_lossy())
            .trim_end_matches('/')
            .to_string();
        Self {
            root: PathBuf::from(&root_norm),
            root_norm,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn root_str(&self) -> &str {
        &self.root_norm
    }

    /// Convert any path (abs, container, host, messy relative) → **repo-relative** form.
    ///
    /// Examples (root = `/projects/test_repos/emscripten`):
    /// - `emcc.py` → `emcc.py`
    /// - `/projects/test_repos/emscripten/tools/link.py` → `tools/link.py`
    /// - `test_repos/emscripten/emcc.py` → `emcc.py` (redundant outer prefix)
    ///
    /// Package dir == project name (root = `…/typer`):
    /// - `typer/main.py` → `typer/main.py` (**not** `main.py`)
    /// - `test_repos/typer/typer/main.py` → `typer/main.py`
    pub fn to_rel(&self, path: impl AsRef<Path>) -> PathBuf {
        let p = normalize_path(&path.as_ref().to_string_lossy());
        let p = p.trim_start_matches("./");
        if p.is_empty() || p == "." {
            return PathBuf::from("");
        }

        // Direct prefix strip (abs under root)
        let root = self.root_norm.as_str();
        if let Some(rest) = p.strip_prefix(root) {
            let rest = rest.trim_start_matches('/');
            return PathBuf::from(rest);
        }

        // Mount dual: same path under host vs container prefix
        if let Some(mapped) = self.map_mount_variant(&p) {
            if let Some(rest) = mapped.strip_prefix(root) {
                return PathBuf::from(rest.trim_start_matches('/'));
            }
            if let Some(rel) = self.strip_redundant_root_prefix(&mapped) {
                return PathBuf::from(rel);
            }
        }

        // Relative (or abs-without-full-root): strip only **redundant outer** prefixes
        // (test_repos/proj/…, hostish leaks). Never eat package dir == project name.
        if let Some(rel) = self.strip_redundant_root_prefix(&p) {
            return PathBuf::from(rel);
        }

        // Already relative to repo
        if !p.starts_with('/') {
            return PathBuf::from(p);
        }

        // Last resort: drop leading slash only
        PathBuf::from(p.trim_start_matches('/'))
    }

    /// Join repo-relative path to project root for filesystem I/O.
    pub fn to_abs(&self, rel: impl AsRef<Path>) -> PathBuf {
        let rel = self.to_rel(rel);
        if rel.as_os_str().is_empty() {
            return self.root.clone();
        }
        if rel.is_absolute() {
            // Still absolute after to_rel — join failed; try as-is
            return rel;
        }
        self.root.join(rel)
    }

    /// Path string for agents / structured reports (host mount rewrite when configured).
    pub fn to_display(&self, rel: impl AsRef<Path>) -> String {
        let rel = self.to_rel(rel);
        let abs = self.to_abs(&rel);
        let host = std::env::var("BUTLER_HOST_MOUNT").unwrap_or_default();
        let container = std::env::var("BUTLER_CONTAINER_MOUNT").unwrap_or_default();
        let s = normalize_path(&abs.to_string_lossy());
        if !host.is_empty() && !container.is_empty() && s.starts_with(&container) {
            return normalize_path(&s.replacen(&container, &host, 1));
        }
        // Prefer repo-relative for compact agent prompts when no mount map
        if !rel.as_os_str().is_empty() {
            return normalize_path(&rel.to_string_lossy());
        }
        s
    }

    /// True if both paths name the same warehouse file under this root.
    pub fn same_file(&self, a: impl AsRef<Path>, b: impl AsRef<Path>) -> bool {
        self.to_rel(a) == self.to_rel(b)
    }

    /// Normalize an in-memory path string for use as a graph key (file_hashes, scopes).
    pub fn key(&self, path: impl AsRef<Path>) -> String {
        normalize_path(&self.to_rel(path).to_string_lossy())
    }

    fn map_mount_variant(&self, path: &str) -> Option<String> {
        let host = std::env::var("BUTLER_HOST_MOUNT").ok().filter(|s| !s.is_empty())?;
        let container = std::env::var("BUTLER_CONTAINER_MOUNT")
            .ok()
            .filter(|s| !s.is_empty())?;
        let host = normalize_path(&host);
        let container = normalize_path(&container);
        if path.starts_with(&host) {
            Some(normalize_path(&path.replacen(&host, &container, 1)))
        } else if path.starts_with(&container) {
            Some(normalize_path(&path.replacen(&container, &host, 1)))
        } else {
            None
        }
    }

    /// Strip a **redundant outer** copy of the project root from `path`.
    ///
    /// - Multi-segment signatures (`test_repos/typer/…`) always strip when they are a prefix.
    /// - Single-segment (project folder name alone) only strips when the remainder looks like
    ///   a **layout** path (`src/`, `tools/`, …), not a **package** path (`typer/main.py`).
    ///
    /// **Prefix only** — never mid-path. `src/click/core.py` under project `click` is untouched.
    fn strip_redundant_root_prefix<'a>(&self, path: &'a str) -> Option<&'a str> {
        let root = self.root_norm.as_str();
        let parts: Vec<&str> = root.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }

        // Prefer longer signatures first (test_repos/typer before typer).
        for n in (1..=parts.len().min(3)).rev() {
            let sig = parts[parts.len() - n..].join("/");
            let marker_rel = format!("{sig}/");
            let marker_abs = format!("/{sig}/");

            let rest = if let Some(r) = path.strip_prefix(&marker_rel) {
                r
            } else if let Some(r) = path.strip_prefix(&marker_abs) {
                r
            } else if path == sig || path == format!("/{sig}") {
                return Some("");
            } else {
                continue;
            };

            // Multi-segment outer wrapper (test_repos/proj/…) — always safe to strip.
            if n >= 2 {
                return Some(rest);
            }

            // Single-segment = leaf project directory name only.
            // Safe when remainder is empty or a conventional **layout** root
            // (`src/`, `tools/`, …). NOT when remainder is a bare file — that would
            // turn package path `typer/main.py` into `main.py` (project name == package).
            if rest.is_empty() || Self::remainder_looks_like_layout(rest) {
                return Some(rest);
            }
            // e.g. path=typer/main.py, sig=typer → rest=main.py → reject strip.
        }
        None
    }

    /// True if `rest` (after stripping project name) is a top-level layout tree, not a package.
    fn remainder_looks_like_layout(rest: &str) -> bool {
        const LAYOUT: &[&str] = &[
            "src",
            "lib",
            "libs",
            "tools",
            "tool",
            "crates",
            "packages",
            "package",
            "include",
            "inc",
            "apps",
            "app",
            "bin",
            "cmd",
            "internal",
            "pkg",
            "python",
            "py",
            "js",
            "ts",
            "typescript",
            "rust",
            "go",
            "cpp",
            "c",
            "cxx",
            "native",
            "bindings",
            "binding",
            "modules",
            "module",
            "vendor",
            "third_party",
            "third-party",
            "examples",
            "tests",
            "test",
            "benches",
            "bench",
            "docs",
            "scripts",
            "ci",
            ".ci",
            "build",
            "cmake",
        ];
        let first = rest.split('/').find(|s| !s.is_empty()).unwrap_or("");
        if first.is_empty() {
            return true;
        }
        // "tools/link.py" → tools — OK to strip project-name prefix.
        // "main.py" alone → NOT layout (would be package-dir collision).
        LAYOUT.iter().any(|l| first.eq_ignore_ascii_case(l))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_rel_strips_absolute_root() {
        let pp = ProjectPaths {
            root: PathBuf::from("/projects/test_repos/emscripten"),
            root_norm: "/projects/test_repos/emscripten".into(),
        };
        assert_eq!(
            pp.to_rel("/projects/test_repos/emscripten/emcc.py"),
            PathBuf::from("emcc.py")
        );
        assert_eq!(
            pp.to_rel("/projects/test_repos/emscripten/tools/link.py"),
            PathBuf::from("tools/link.py")
        );
    }

    #[test]
    fn to_rel_strips_nested_relative_prefix() {
        let pp = ProjectPaths {
            root: PathBuf::from("/projects/test_repos/emscripten"),
            root_norm: "/projects/test_repos/emscripten".into(),
        };
        assert_eq!(
            pp.to_rel("test_repos/emscripten/emcc.py"),
            PathBuf::from("emcc.py")
        );
    }

    #[test]
    fn to_rel_does_not_eat_package_dir_named_like_project() {
        // click ships as src/click/… under project root …/click
        let pp = ProjectPaths {
            root: PathBuf::from("/projects/test_repos/click"),
            root_norm: "/projects/test_repos/click".into(),
        };
        assert_eq!(
            pp.to_rel("src/click/core.py"),
            PathBuf::from("src/click/core.py"),
            "must not strip mid-path /click/"
        );
        assert_eq!(
            pp.to_rel("/projects/test_repos/click/src/click/core.py"),
            PathBuf::from("src/click/core.py")
        );
        // Host-style relative still carrying test_repos/click prefix
        assert_eq!(
            pp.to_rel("test_repos/click/src/click/core.py"),
            PathBuf::from("src/click/core.py")
        );
    }

    #[test]
    fn to_rel_keeps_python_package_dir_equal_to_project_name() {
        // typer: root …/typer, package lives at typer/main.py
        let pp = ProjectPaths {
            root: PathBuf::from("/projects/test_repos/typer"),
            root_norm: "/projects/test_repos/typer".into(),
        };
        assert_eq!(
            pp.to_rel("typer/main.py"),
            PathBuf::from("typer/main.py"),
            "must not eat package dir when it matches project folder name"
        );
        assert_eq!(
            pp.to_rel("/projects/test_repos/typer/typer/main.py"),
            PathBuf::from("typer/main.py")
        );
        assert_eq!(
            pp.to_rel("test_repos/typer/typer/main.py"),
            PathBuf::from("typer/main.py")
        );
        assert_eq!(pp.to_display("typer/main.py"), "typer/main.py");
        assert_eq!(
            pp.to_abs("typer/main.py"),
            PathBuf::from("/projects/test_repos/typer/typer/main.py")
        );
    }

    #[test]
    fn same_file_across_path_forms() {
        let pp = ProjectPaths {
            root: PathBuf::from("/projects/test_repos/emscripten"),
            root_norm: "/projects/test_repos/emscripten".into(),
        };
        assert!(pp.same_file("emcc.py", "/projects/test_repos/emscripten/emcc.py"));
        assert!(pp.same_file("test_repos/emscripten/emcc.py", "emcc.py"));
    }

    #[test]
    fn to_rel_project_name_prefix_layout_only() {
        let pp = ProjectPaths {
            root: PathBuf::from("/projects/test_repos/emscripten"),
            root_norm: "/projects/test_repos/emscripten".into(),
        };
        // Redundant project-name prefix before a layout dir → strip
        assert_eq!(
            pp.to_rel("emscripten/tools/link.py"),
            PathBuf::from("tools/link.py")
        );
        assert_eq!(pp.to_display("emcc.py"), "emcc.py");
    }
}
