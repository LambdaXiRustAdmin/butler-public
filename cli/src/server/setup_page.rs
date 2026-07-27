//! Welcome / first-run proof-of-life HTML (`GET /setup`).
//!
//! Deliberately separate from the operator lab (`dashboard.rs` → `/` `/ops`).
//! No harvester, export, or training controls here.

use axum::response::Html;
use cli::bin_paths::{path_for_mcp_snippet, resolve_tool_bin};

/// First-run / proof-of-life page.
pub async fn render_setup() -> Html<String> {
    Html(fill_setup_placeholders(include_str!("setup.html")))
}

fn fill_setup_placeholders(html: &str) -> String {
    let port = std::env::var("BUTLER__SERVER__PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(8002);
    let butler_url = format!("http://127.0.0.1:{port}");
    let mcp_bin = resolve_mcp_bin_path();
    // Escape backslashes for JSON-in-HTML on Windows paths.
    let mcp_bin_js = mcp_bin.replace('\\', "\\\\");
    html.replace("{{PORT}}", &port.to_string())
        .replace("{{BUTLER_URL}}", &butler_url)
        .replace("{{BASE_URL}}", &butler_url)
        .replace("{{MCP_BIN}}", &mcp_bin_js)
}

/// Best-effort absolute path to the MCP stdio bridge binary (Windows-aware `.exe`).
fn resolve_mcp_bin_path() -> String {
    resolve_tool_bin("mcp")
        .or_else(|| resolve_tool_bin("butler-mcp"))
        .map(|p| path_for_mcp_snippet(&p))
        .unwrap_or_else(|| {
            #[cfg(windows)]
            {
                "mcp.exe".into()
            }
            #[cfg(not(windows))]
            {
                "mcp".into()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_html_has_placeholders_and_no_harvester() {
        let raw = include_str!("setup.html");
        assert!(raw.contains("{{MCP_BIN}}"));
        assert!(raw.contains("{{BUTLER_URL}}"));
        assert!(raw.contains("install-log"));
        assert!(raw.contains("btn-copy"));
        assert!(raw.contains("btn-map"));
        assert!(raw.contains("project-select"));
        assert!(raw.contains("project-path"));
        assert!(raw.contains("btn-trace"));
        assert!(raw.contains("pin-select"));
        assert!(raw.contains("populatePinDropdown"));
        assert!(raw.contains("map-scope-select"));
        assert!(raw.contains("populateMapScopeDropdown"));
        assert!(raw.contains("Skeleton map") || raw.contains("skeleton"));
        assert!(!raw.to_ascii_lowercase().contains("run harvester"));
        assert!(!raw.contains("Build Training Bundle"));
        let filled = fill_setup_placeholders(raw);
        assert!(!filled.contains("{{PORT}}"));
        assert!(filled.contains("127.0.0.1"));
    }
}
