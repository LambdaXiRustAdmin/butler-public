//! Path translation helpers for Docker/host mount scenarios.

use code_graph::snooper::normalize_path;
use std::path::Path;

/// Used when Butler runs inside Docker while the LLM client operates on the host.
/// Rewrites paths like `$BUTLER_HOST_MOUNT/test_repos/fd` → `$BUTLER_CONTAINER_MOUNT/test_repos/fd`.
///
/// # Environment Variables
/// - `BUTLER_HOST_MOUNT`: The host-side mount point prefix (e.g., `/home/you/projects`)
/// - `BUTLER_CONTAINER_MOUNT`: The container-side mount point prefix (e.g., `/projects`)
///
/// # Returns
/// The translated path if both mount variables are set and the client path starts with
/// the host prefix. Otherwise, returns the original path unchanged.
pub fn translate_client_path(client_path: &str) -> String {
    let host_prefix = std::env::var("BUTLER_HOST_MOUNT").unwrap_or_default();
    let container_prefix = std::env::var("BUTLER_CONTAINER_MOUNT").unwrap_or_default();

    let client_path = normalize_path(client_path);
    if !host_prefix.is_empty()
        && !container_prefix.is_empty()
        && client_path.starts_with(&host_prefix)
    {
        normalize_path(&client_path.replacen(&host_prefix, &container_prefix, 1))
    } else {
        client_path
    }
}

/// Container→host mount rewrite for absolute paths.
/// Prefer [`code_graph::ProjectPaths::to_display`] when the project root is known
/// (repo-relative warehouse keys + display policy).
pub fn format_host_path(internal_path: &Path) -> String {
    let host_prefix = std::env::var("BUTLER_HOST_MOUNT").unwrap_or_default();
    let container_prefix = std::env::var("BUTLER_CONTAINER_MOUNT").unwrap_or_default();
    let path_str = normalize_path(&internal_path.to_string_lossy());
    if !host_prefix.is_empty()
        && !container_prefix.is_empty()
        && path_str.starts_with(&container_prefix)
    {
        return normalize_path(&path_str.replacen(&container_prefix, &host_prefix, 1));
    }
    path_str
}

/// Project-anchored display path (foundation path dialect).
/// Repo-relative warehouse key → agent-facing path (+ host mount when set).
pub fn format_project_path(project_root: &Path, warehouse_path: &Path) -> String {
    let s = code_graph::ProjectPaths::new(project_root).to_display(warehouse_path);
    // Second pass: rewrite absolute container paths that to_display may emit under mounts.
    format_host_path(Path::new(&s))
}
