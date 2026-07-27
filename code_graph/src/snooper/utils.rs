/// Normalizes a filesystem path string to use forward slashes (`/`).
/// This ensures consistent path representation across platforms (Windows `\` vs Unix `/`
/// and WSL boundaries) for reliable string matching inside the code graph (edges, cache keys,
/// scope filtering, etc.).
///
/// This is a pure string operation and does not perform filesystem canonicalization.
pub fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}
