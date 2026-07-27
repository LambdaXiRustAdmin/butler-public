//! Resolve sibling / install-dir binaries across Unix and Windows.
//!
//! Portable zip layout: `butler(.exe)`, `butler-server(.exe)`, `mcp(.exe)` in one folder.
//! On Windows, `is_file()` requires the `.exe` suffix.

use std::path::{Path, PathBuf};

/// Candidate filenames for a logical binary name (`butler-server` → also `butler-server.exe`).
pub fn bin_name_candidates(base: &str) -> Vec<String> {
    let base = base.trim().trim_end_matches(".exe");
    if base.is_empty() {
        return Vec::new();
    }
    // Prefer platform-native first so Linux doesn't pick a stray .exe.
    #[cfg(windows)]
    {
        vec![format!("{base}.exe"), base.to_string()]
    }
    #[cfg(not(windows))]
    {
        vec![base.to_string(), format!("{base}.exe")]
    }
}

/// First existing file among `dir/name` for each candidate of `base`.
pub fn find_in_dir(dir: &Path, base: &str) -> Option<PathBuf> {
    for name in bin_name_candidates(base) {
        let cand = dir.join(&name);
        if cand.is_file() {
            return Some(canonicalize_display(&cand));
        }
    }
    None
}

/// Search PATH for `base` / `base.exe`.
pub fn which_bin(base: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if let Some(p) = find_in_dir(&dir, base) {
            return Some(p);
        }
    }
    None
}

/// Sibling of the running executable (e.g. butler → butler-server in same folder).
pub fn sibling_of_current_exe(base: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    find_in_dir(dir, base)
}

/// User-local install dirs (`~/.local/bin`, Windows `%LOCALAPPDATA%\Butler`, etc.).
pub fn user_install_bin(base: &str) -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(&home).join(".local/bin");
        if let Some(f) = find_in_dir(&p, base) {
            return Some(f);
        }
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let p = PathBuf::from(&profile).join(".local").join("bin");
        if let Some(f) = find_in_dir(&p, base) {
            return Some(f);
        }
        // Optional portable install root used by docs / future installer.
        let butler_dir = PathBuf::from(&profile).join("Butler");
        if let Some(f) = find_in_dir(&butler_dir, base) {
            return Some(f);
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let p = PathBuf::from(local).join("Butler");
        if let Some(f) = find_in_dir(&p, base) {
            return Some(f);
        }
    }
    None
}

/// Dev tree relative to cwd (`target/release/...`).
pub fn find_in_dev_tree(base: &str) -> Option<PathBuf> {
    let rels = [
        format!("target/release/{base}"),
        format!("target/debug/{base}"),
        format!("cli/target/release/{base}"),
    ];
    for rel in rels {
        for name in bin_name_candidates(base) {
            // rel may already include base without exe; rebuild with candidate
            let parent = Path::new(&rel).parent().unwrap_or(Path::new("."));
            let cand = parent.join(&name);
            if cand.is_file() {
                return Some(canonicalize_display(&cand));
            }
        }
        // Also try exact rel + .exe
        let p = PathBuf::from(&rel);
        if p.is_file() {
            return Some(canonicalize_display(&p));
        }
        let p_exe = PathBuf::from(format!("{rel}.exe"));
        if p_exe.is_file() {
            return Some(canonicalize_display(&p_exe));
        }
    }
    None
}

/// Full resolution order for a tool binary.
pub fn resolve_tool_bin(base: &str) -> Option<PathBuf> {
    sibling_of_current_exe(base)
        .or_else(|| user_install_bin(base))
        .or_else(|| which_bin(base))
        .or_else(|| find_in_dev_tree(base))
}

fn canonicalize_display(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Path string for MCP JSON (absolute when possible).
pub fn path_for_mcp_snippet(p: &Path) -> String {
    let abs = canonicalize_display(p);
    abs.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_include_exe() {
        let c = bin_name_candidates("mcp");
        assert!(c.iter().any(|s| s == "mcp" || s == "mcp.exe"));
        assert!(c.iter().any(|s| s.ends_with("mcp") || s.ends_with("mcp.exe")));
        #[cfg(windows)]
        assert_eq!(c[0], "mcp.exe");
        #[cfg(not(windows))]
        assert_eq!(c[0], "mcp");
    }

    #[test]
    fn candidates_strip_existing_exe_suffix() {
        let c = bin_name_candidates("butler-server.exe");
        assert!(c.iter().any(|s| s.contains("butler-server")));
    }
}
