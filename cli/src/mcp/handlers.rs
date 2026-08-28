//! Bridge handlers: proxy to real Butler /context, and the limited manifest/health served
//! by the MCP stdio/HTTP bridge itself (distinct from server's /mcp/manifest).
//! All input schemas come from the single source of truth in crate::mcp_schema.

use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use std::env;
use tokio::time::{sleep, Duration};

use super::protocol::friendly_mcp_error;
use crate::butler_instructions::BUTLER_HELP_TOOL_DESCRIPTION;
use crate::mcp_schema::butler_context_tool_schema;

#[derive(Serialize)]
pub struct McpManifest {
    pub name: String,
    pub description: String,
    pub version: String,
    pub base_url: String,
    pub tools: Vec<McpTool>,
}

#[derive(Serialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// Limited manifest for the bridge's HTTP proxy mode (always the basic 4 tools).
pub async fn mcp_manifest(butler_url: String) -> impl IntoResponse {
    crate::mcp_diag!(
        "mcp_manifest endpoint called - using butler_url={}",
        butler_url
    );
    let manifest = McpManifest {
        name: "butler-mcp".to_string(),
        description: "Butler MCP Server — precise code context via two input styles: keyword search or mod/line (target_file + target_line). For mod/line, returns the exact line text + call graph edges. Returns rich metadata.".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        base_url: butler_url.clone(),
        tools: vec![
            McpTool {
                name: "butler_context".to_string(),
                description: "Structural code context (Trace). project: absolute path preferred; if omitted, last successful project is used. Server auto-starts on localhost when down. Prefer who_calls when available.".to_string(),
                input_schema: butler_context_tool_schema(),
            },
            McpTool {
                name: "butler_help".to_string(),
                description: BUTLER_HELP_TOOL_DESCRIPTION.to_string(),
                input_schema: serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
            },
            McpTool {
                name: "butler_list_projects".to_string(),
                description: "Lists all projects (codebases) available on this Butler server. Small/local LLMs should call this first to discover valid values for the 'project' parameter.".to_string(),
                input_schema: serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
            },
            McpTool {
                name: "butler_select_project".to_string(),
                description: "Interactive project selector. Shows available projects and gives clear instructions for selecting an existing one or adding a new project. Recommended tool for managing which codebase to analyze.".to_string(),
                input_schema: serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
            },
        ],
    };
    (StatusCode::OK, Json(manifest))
}

pub async fn mcp_health() -> impl IntoResponse {
    crate::mcp_diag!("mcp_health endpoint called - validating server status");
    (
        StatusCode::OK,
        Json(serde_json::json!({"status": "healthy", "connected_to_butler": true})),
    )
}

fn is_graph_building_marker(content: &str) -> bool {
    let t = content.trim_start();
    // Accept both legacy and explicit cold banner.
    t.starts_with("=== Building Graph")
}

/// Soft wall: agent must confirm before more wait (do not poll through).
fn is_soft_wall(content: &str, backend: &serde_json::Value) -> bool {
    if content.contains("BUILDING_SOFT_WALL") {
        return true;
    }
    let advice = backend
        .get("structured")
        .and_then(|s| {
            s.get("wait_policy")
                .or_else(|| s.get("telemetry").and_then(|t| t.get("wait_policy")))
        })
        .and_then(|wp| wp.get("advice"))
        .and_then(|v| v.as_str());
    advice == Some("confirm_continue")
        || backend
            .get("structured")
            .and_then(|s| s.get("telemetry"))
            .and_then(|t| t.get("status"))
            .and_then(|v| v.as_str())
            == Some("BUILDING_SOFT_WALL")
}

/// Adaptive poll sleep from wait_policy (fallback 3s).
fn retry_after_from_backend(backend: &serde_json::Value) -> Duration {
    let ms = backend
        .get("structured")
        .and_then(|s| {
            s.get("wait_policy")
                .or_else(|| s.get("telemetry").and_then(|t| t.get("wait_policy")))
        })
        .and_then(|wp| wp.get("retry_after_ms"))
        .and_then(|v| v.as_u64())
        .unwrap_or(3_000)
        .clamp(500, 30_000);
    Duration::from_millis(ms)
}

/// Cold SLA usable partial — return immediately to the agent (do not poll away TOC).
///
/// Status BUILDING alone is **not** usable (empty shell / who_calls pack miss).
/// Require a TOC/skeleton/hubs/callers answer, or the explicit usable-partial banner.
fn is_usable_building_partial(content: &str, backend: &serde_json::Value) -> bool {
    if is_soft_wall(content, backend) {
        return true; // surface "are you sure?" — never auto-poll past soft wall
    }
    if has_answer_pack(backend) {
        return true;
    }
    content.contains("status: BUILDING") && content.contains("Usable while building")
}

fn request_wants_symbol_pack(params: &serde_json::Value) -> bool {
    params
        .get("target_symbol")
        .or_else(|| params.get("symbol"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
}

fn backend_is_building(content: &str, backend: &serde_json::Value) -> bool {
    if is_graph_building_marker(content) {
        return true;
    }
    matches!(
        backend.get("warning").and_then(|v| v.as_str()),
        Some("graph_building") | Some("building")
    ) || backend.get("mode").and_then(|v| v.as_str()) == Some("building")
        || backend
            .get("structured")
            .and_then(|s| {
                s.get("telemetry")
                    .and_then(|t| t.get("status"))
                    .or_else(|| s.get("wait_policy").and_then(|w| w.get("status")))
            })
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.starts_with("BUILDING"))
}

/// Real Trace/Arch payload (not wait_policy-only BUILDING).
fn has_answer_pack(backend: &serde_json::Value) -> bool {
    let Some(st) = backend.get("structured") else {
        return false;
    };
    let nonempty = |k: &str| {
        st.get(k)
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty())
    };
    nonempty("callers")
        || nonempty("callees")
        || nonempty("skeleton")
        || nonempty("hubs")
        || st.get("target").is_some_and(|t| t.is_object())
}

/// who_calls / Trace: keep polling empty BUILDING. Arch with TOC is usable now.
fn should_poll_building(
    params: &serde_json::Value,
    content: &str,
    backend: &serde_json::Value,
) -> bool {
    if is_soft_wall(content, backend) {
        return false;
    }
    if !backend_is_building(content, backend) {
        return false;
    }
    if has_answer_pack(backend) {
        return false;
    }
    request_wants_symbol_pack(params) || !is_usable_building_partial(content, backend)
}

fn has_structured_payload(backend: &serde_json::Value) -> bool {
    backend
        .get("structured")
        .map(|s| !s.is_null())
        .unwrap_or(false)
}

/// Map Butler `POST /context` JSON into MCP CallToolResult shape.
/// `content` = human summary text; `structuredContent` = native orchestrate object.
pub fn backend_context_to_mcp_result(backend: &serde_json::Value) -> serde_json::Value {
    let text = backend
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    let mut result = serde_json::json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": false
    });

    if let Some(st) = backend.get("structured") {
        if !st.is_null() {
            result["structuredContent"] = st.clone();
        }
    }

    result
}

/// Proxy a tool call (or direct context) to the real Butler backend at /context.
/// Internally polls on "Building Graph" progress messages so the LLM/MCP client
/// only ever receives the final response. Shapes success/error into canonical MCP
/// `{ content, structuredContent?, isError }`.
pub async fn handle_butler_context(
    params: serde_json::Value,
    butler_url: &str,
) -> serde_json::Value {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    for attempt in 0..15 {
        match crate::config::apply_client_auth(
            client.post(format!("{}/context", butler_url)).json(&params),
        )
        .send()
        .await
        {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                let backend: serde_json::Value = serde_json::from_str(&body)
                    .unwrap_or_else(|_| serde_json::json!({ "content": body }));

                let content = backend
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");

                // Soft wall: never auto-poll past "are you sure?".
                if is_soft_wall(content, &backend) {
                    return backend_context_to_mcp_result(&backend);
                }

                // Symbol Trace / empty-shell BUILDING: poll until a pack (or retries exhaust).
                if should_poll_building(&params, content, &backend) {
                    let wait = retry_after_from_backend(&backend);
                    crate::mcp_diag!(
                        "Graph building (attempt {}/15), adaptive sleep {}ms...",
                        attempt + 1,
                        wait.as_millis()
                    );
                    sleep(wait).await;
                    continue;
                }

                if is_usable_building_partial(content, &backend) {
                    return backend_context_to_mcp_result(&backend);
                }

                if has_structured_payload(&backend) {
                    return backend_context_to_mcp_result(&backend);
                }

                if is_graph_building_marker(content) {
                    let wait = retry_after_from_backend(&backend);
                    crate::mcp_diag!(
                        "Graph building (attempt {}/15), adaptive sleep {}ms...",
                        attempt + 1,
                        wait.as_millis()
                    );
                    sleep(wait).await;
                    continue;
                }

                return backend_context_to_mcp_result(&backend);
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                let short_body: String = body.chars().take(300).collect();
                let tip = match status.as_u16() {
                    401 | 403 => Some(
                        "Server has password auth. Set BUTLER_PASSWORD (or BUTLER_API_TOKEN) on the MCP client to match server.password.",
                    ),
                    404 => Some("Check that the Butler server is running the latest version (it should expose /context)."),
                    500..=599 => Some("Check the Butler server logs for the root cause."),
                    _ => None,
                };
                let msg = friendly_mcp_error(
                    &format!("Butler backend returned HTTP {}: {}", status, short_body),
                    tip,
                );
                return serde_json::json!({
                    "content": [ { "type": "text", "text": msg } ],
                    "isError": true
                });
            }
            Err(e) => {
                let err_str = e.to_string();
                let (message, tip): (String, Option<&str>) = if err_str
                    .contains("connection refused")
                    || err_str.contains("Connect")
                {
                    (format!("Cannot connect to the Butler backend at {}.", butler_url), Some("Start the server locally with `cargo run -p cli --bin butler-server`, or set BUTLER_URL=http://butler:8002 if using Docker Compose."))
                } else if err_str.contains("timeout") {
                    ("Request to the Butler backend timed out.".to_string(), Some("Try increasing max_tokens or reducing depth. You can also check if the server is overloaded."))
                } else {
                    (
                        format!("Network error talking to Butler: {}", err_str),
                        Some("Verify that BUTLER_URL is correct and the server is reachable."),
                    )
                };
                let friendly = friendly_mcp_error(&message, tip);
                return serde_json::json!({
                    "content": [ { "type": "text", "text": friendly } ],
                    "isError": true
                });
            }
        }
    }
    // Exhausted retries
    let friendly = friendly_mcp_error(
        "Butler backend still building graph after 15 retries (~45s).",
        Some("The server is indexing a large project (e.g. heavy FFI like pyo3); wait a minute or use a smaller scope_paths."),
    );
    serde_json::json!({
        "content": [ { "type": "text", "text": friendly } ],
        "isError": true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_building_partial_detected() {
        let content = "=== Building Graph (cold) (12%) ===\n\n=== Usable while building ===\nstatus: BUILDING\ntoc: …\n";
        let backend = serde_json::json!({
            "content": content,
            "structured": { "telemetry": { "status": "BUILDING", "usable_while_building": true } }
        });
        assert!(is_usable_building_partial(content, &backend));
        let soft = "=== Usable while building ===\nstatus: BUILDING_SOFT_WALL\n";
        let soft_be = serde_json::json!({
            "content": soft,
            "structured": { "wait_policy": { "status": "BUILDING_SOFT_WALL", "advice": "confirm_continue", "retry_after_ms": 15000 } }
        });
        assert!(is_soft_wall(soft, &soft_be));
        assert!(is_usable_building_partial(soft, &soft_be));
        assert_eq!(retry_after_from_backend(&soft_be).as_millis(), 15_000);
        assert!(is_graph_building_marker(content));
    }

    #[test]
    fn symbol_trace_empty_building_polls() {
        let params = serde_json::json!({ "target_symbol": "try_copy_from_holding" });
        let content = "=== Building Graph (working (cold)) (0%) ===\nstatus: BUILDING\n";
        let backend = serde_json::json!({
            "content": content,
            "warning": "graph_building",
            "mode": "building",
            "structured": { "wait_policy": { "status": "BUILDING", "retry_after_ms": 1500 } }
        });
        assert!(should_poll_building(&params, content, &backend));
        assert!(!has_answer_pack(&backend));
    }

    #[test]
    fn arch_toc_building_does_not_poll() {
        let params = serde_json::json!({ "goal": "ArchitecturalSummary" });
        let content = "=== Building Graph (12%) ===\n\n=== Usable while building ===\nstatus: BUILDING\n";
        let backend = serde_json::json!({
            "content": content,
            "structured": {
                "skeleton": ["src/"],
                "telemetry": { "status": "BUILDING", "usable_while_building": true }
            }
        });
        assert!(!should_poll_building(&params, content, &backend));
        assert!(has_answer_pack(&backend));
        assert!(is_usable_building_partial(content, &backend));
    }

    #[test]
    fn structured_payload_skips_building_poll_even_when_content_has_status_prefix() {
        let backend = serde_json::json!({
            "content": "=== Building Graph (100%) ===\nArchitectural summary: 5 skeleton paths, 3 hubs.",
            "structured": {
                "skeleton": ["src/a.rs"],
                "hubs": [{ "name": "App", "file": "app.rs", "line": 1, "score": 99.0 }]
            }
        });
        assert!(has_structured_payload(&backend));
        assert!(is_graph_building_marker(
            backend["content"].as_str().unwrap()
        ));
        assert!(has_answer_pack(&backend));
        assert!(!should_poll_building(
            &serde_json::json!({}),
            backend["content"].as_str().unwrap(),
            &backend
        ));
    }

    #[test]
    fn mcp_result_carries_native_structured_content() {
        let backend = serde_json::json!({
            "content": "Trace for Console: 9 callers, 0 callees (10 highly relevant blocks).",
            "structured": {
                "target": { "name": "Console", "file": "rich/console.py", "line": 581 },
                "callers": [{ "name": "foo", "file": "a.py", "line": 1 }],
                "callees": []
            }
        });
        let result = backend_context_to_mcp_result(&backend);
        assert_eq!(
            result["structuredContent"]["target"]["name"].as_str(),
            Some("Console")
        );
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(!text.starts_with('{'));
        assert!(text.contains("Console"));
    }
}
