//! Path inventory policy: which directory segments are hard-pruned or treated as vendor noise.
//!
//! ## Bundled-vendor directory segments
//!
//! Built-in names below are a **known-vendored-tree skip list** (not a security allowlist):
//! scan hard-prunes them so call edges and `name_index` stay on product code.
//!
//! Covers top-level dumps (`vendor/`, `third_party/`) and common in-package copies
//! (`_vendor/`, `_click/`). Product private packages (`_dynamo`, …) are intentionally absent.
//!
//! **Extend without code changes** (Butler CLI/server):
//! ```toml
//! [analysis]
//! # Merged with the built-in list; segment-exact directory names only.
//! extra_bundled_vendor_segments = ["_bundled", "thirdparty"]
//! ```
//! Or add `my_vendor/` to `analysis.skip_directories` / `noise_path_components`.

/// Built-in bundled-vendor directory segments (segment-exact, case-insensitive match).
///
/// Hard-pruned at scan when a directory's name equals one of these. Also used for
/// ranking demotion ([`is_bundled_vendor_dir_segment`]) when a path still appears
/// (e.g. stale cache).
pub const BUNDLED_VENDOR_DIR_SEGMENTS: &[&str] = &[
    "vendor",
    "vendored",
    "_vendor",
    "_click",
    "third_party",
    "third-party",
    "external",
    "deps",
    "ext",
    "node_modules",
    "site-packages",
];

/// Always-pruned infrastructure dirs (build artifacts, VCS, tool caches).
/// Not part of the user-facing bundled-vendor list; always applied at scan.
pub const INFRA_PRUNE_DIR_SEGMENTS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    ".cache",
    "__pycache__",
    ".butler",
];

/// True if `seg` is a built-in bundled-vendor directory name (segment-exact).
pub fn is_bundled_vendor_dir_segment(seg: &str) -> bool {
    let s = seg.trim_matches('/');
    if s.is_empty() {
        return false;
    }
    BUNDLED_VENDOR_DIR_SEGMENTS
        .iter()
        .any(|v| v.eq_ignore_ascii_case(s))
}

/// True if `seg` is always-pruned infrastructure (target, .git, …).
pub fn is_infra_prune_dir_segment(seg: &str) -> bool {
    let s = seg.trim_matches('/');
    if s.is_empty() {
        return false;
    }
    INFRA_PRUNE_DIR_SEGMENTS
        .iter()
        .any(|v| v.eq_ignore_ascii_case(s))
}

/// `seg/` patterns for `skip_directories` / first-use `ignore_paths`.
pub fn bundled_vendor_skip_patterns() -> Vec<String> {
    BUNDLED_VENDOR_DIR_SEGMENTS
        .iter()
        .map(|s| format!("{s}/"))
        .collect()
}

/// Owned segment names (for config merge / noise lists).
pub fn bundled_vendor_dir_segments_owned() -> Vec<String> {
    BUNDLED_VENDOR_DIR_SEGMENTS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_package_and_top_level_vendor_segments() {
        assert!(is_bundled_vendor_dir_segment("_click"));
        assert!(is_bundled_vendor_dir_segment("_vendor"));
        assert!(is_bundled_vendor_dir_segment("third_party"));
        assert!(is_bundled_vendor_dir_segment("Vendor")); // case
        assert!(!is_bundled_vendor_dir_segment("_dynamo"));
        assert!(!is_bundled_vendor_dir_segment("my_vendor_tool"));
        assert!(!is_bundled_vendor_dir_segment("dependencies"));
    }

    #[test]
    fn infra_prune_separate_from_product() {
        assert!(is_infra_prune_dir_segment("target"));
        assert!(is_infra_prune_dir_segment(".git"));
        assert!(!is_infra_prune_dir_segment("src"));
    }
}
