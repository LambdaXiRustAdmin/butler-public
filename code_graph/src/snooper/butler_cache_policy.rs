//! Policy: where Butler may create `{root}/.butler/` warehouses.
//!
//! Intentional layout is **one `.butler` per project root** (e.g. the repo root of a
//! warmed project). Accidental roots are process CWD inside `src/…` or fixture dirs
//! under `examples/` / `tests/` — those used to litter the tree with graph caches.
//!
//! Override (tests / intentional fixture warehouses):
//! `BUTLER_ALLOW_NESTED_CACHE=1`

use std::io;
use std::path::{Component, Path, PathBuf};

/// Env opt-in to write `.butler` under paths this policy would refuse.
pub const ALLOW_NESTED_ENV: &str = "BUTLER_ALLOW_NESTED_CACHE";

/// Segments that must not appear as a **prefix** of the project root path
/// (i.e. root nested *inside* these trees).
const FORBIDDEN_NEST_PARENTS: &[&str] = &["src", "examples", "tests", "test", "benches"];

/// If `root` is nested under a source/fixture tree, return a human reason.
///
/// Allows:
/// - `/projects/example-repo` (has `src/` *child*, root is not under `src/`)
/// - `/tmp/butler_probe_xyz`
///
/// Refuses:
/// - `…/code_graph/src/snooper`
/// - `…/examples/test_data`
/// - `…/tests/fixtures/foo`
pub fn butler_cache_write_forbidden_reason(root: &Path) -> Option<String> {
    if std::env::var_os(ALLOW_NESTED_ENV).is_some() {
        return None;
    }

    let comps: Vec<&str> = root
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();

    if comps.is_empty() {
        return None;
    }

    // Forbidden if any *non-final* component is a nest parent (root lives inside that tree).
    // Also forbid when the *final* component is examples/tests/test/benches (fixture dirs).
    // Final component `src` alone is allowed (rare package-at-src layout).
    let last = comps.len() - 1;
    for (i, c) in comps.iter().enumerate() {
        let name = c.as_ref();
        let is_forbidden_name = FORBIDDEN_NEST_PARENTS.iter().any(|f| *f == name);
        if !is_forbidden_name {
            continue;
        }
        if i < last {
            return Some(format!(
                "refusing .butler under nested path (component {name:?} is not project root); \
                 root={} — set {ALLOW_NESTED_ENV}=1 to override",
                root.display()
            ));
        }
        // Final segment: still refuse fixture-ish names.
        if matches!(name, "examples" | "tests" | "test" | "benches") {
            return Some(format!(
                "refusing .butler when project root is a fixture tree ({name}); \
                 root={} — set {ALLOW_NESTED_ENV}=1 to override",
                root.display()
            ));
        }
    }
    None
}

pub fn assert_butler_cache_writable(root: &Path) -> io::Result<()> {
    if let Some(reason) = butler_cache_write_forbidden_reason(root) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, reason));
    }
    Ok(())
}

/// Probe that `{root}/.butler/cache` is **actually** writable by this process.
///
/// Catches root-owned Docker litter (policy allows the path, open(2) still fails).
/// Without this, FullEdge can rebuild for minutes then fail to persist — next open
/// reloads stale edge_sem and thrash-rebuilds forever.
///
/// Checks (1) create a new probe file **and** (2) open existing `manifest.bin` /
/// `graph.bin` for write when present — dir 775 + root-owned 644 files is a common trap.
pub fn probe_butler_cache_dir_writable(root: &Path) -> io::Result<()> {
    assert_butler_cache_writable(root)?;
    let cache = root.join(".butler").join("cache");
    if !cache.is_dir() {
        // Parent create may still fail later; only probe when dir exists.
        return Ok(());
    }
    let deny = |e: &io::Error, path: &Path| -> io::Error {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cannot write Butler cache at {} ({e}) — often root-owned Docker files. \
                 Fix: chown -R \"$(whoami)\" \"{}/.butler\"  then re-warm. \
                 Until writable, Complete edges cannot persist and edge_sem bumps thrash FullEdge.",
                path.display(),
                root.display()
            ),
        )
    };
    let probe = cache.join(format!(
        ".butler_write_probe_{}",
        std::process::id()
    ));
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
        }
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            return Err(deny(&e, &cache));
        }
        Err(e) => return Err(e),
    }
    // Existing artifacts may be root:root 644 while the directory is user-writable.
    for name in ["manifest.bin", "graph.bin", "sources_fp.bin", "edges_part_00000.bin"] {
        let p = cache.join(name);
        if !p.is_file() {
            continue;
        }
        match std::fs::OpenOptions::new().write(true).open(&p) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                return Err(deny(&e, &p));
            }
            Err(_) => {
                // Other errors (e.g. busy) — ignore; save path will surface them.
            }
        }
    }
    Ok(())
}

/// Create `{root}/.butler` only when policy allows.
pub fn ensure_project_butler_dir(root: &Path) -> io::Result<PathBuf> {
    assert_butler_cache_writable(root)?;
    let dir = root.join(".butler");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// `{root}/.butler/cache` — creates parents when allowed.
pub fn ensure_project_butler_cache_dir(root: &Path) -> io::Result<PathBuf> {
    let butler = ensure_project_butler_dir(root)?;
    let cache = butler.join("cache");
    std::fs::create_dir_all(&cache)?;
    // After create, confirm this uid can write (existing root-owned dir is a trap).
    probe_butler_cache_dir_writable(root)?;
    Ok(cache)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn allows_repo_root_style_paths() {
        assert!(butler_cache_write_forbidden_reason(Path::new("/projects/example-repo")).is_none());
        assert!(butler_cache_write_forbidden_reason(Path::new("/tmp/butler_probe_1")).is_none());
        assert!(butler_cache_write_forbidden_reason(Path::new("/home/u/projects/click")).is_none());
    }

    #[test]
    fn refuses_nested_src_and_examples() {
        let snooper = PathBuf::from("/home/u/projects/example-repo/code_graph/src/snooper");
        assert!(butler_cache_write_forbidden_reason(&snooper).is_some());

        let fixture = PathBuf::from("/home/u/projects/example-repo/code_graph/examples/test_data");
        assert!(butler_cache_write_forbidden_reason(&fixture).is_some());

        let tests = PathBuf::from("/home/u/projects/foo/tests/data");
        assert!(butler_cache_write_forbidden_reason(&tests).is_some());
    }

    #[test]
    fn env_override_allows_nested() {
        std::env::set_var(ALLOW_NESTED_ENV, "1");
        let nested = PathBuf::from("/x/src/y");
        assert!(butler_cache_write_forbidden_reason(&nested).is_none());
        std::env::remove_var(ALLOW_NESTED_ENV);
        assert!(butler_cache_write_forbidden_reason(&nested).is_some());
    }

    #[test]
    fn probe_writable_on_tmp_ok() {
        let dir = std::env::temp_dir().join(format!("butler_probe_write_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".butler/cache")).unwrap();
        assert!(probe_butler_cache_dir_writable(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
