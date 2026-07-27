//! Consolidated MCP bridge: stdio JSON-RPC transport + thin HTTP proxy.
//! Real logic moved here from bin (Perfectionist Path). Bin is now a thin wrapper.
//! Uses single source of truth schemas from crate::mcp_schema.
//!
//! **M1 peel:** product tools/list + tools/call → [`dispatch`].
//! **M2 peel:** expert harvest tools → [`harvest_dispatch`] (not default product belt).
//!
//! Stranger / Alpha: `butler_ask` only. Harvest requires expert_mode at process start.

pub mod handlers;
pub mod protocol;
mod dispatch;
mod harvest_dispatch;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::{
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use serde_json::Value;
use tokio::net::TcpListener;

use crate::butler_instructions::BUTLER_ORCHESTRATE_INSTRUCTIONS;
use handlers::{mcp_health, mcp_manifest};
use dispatch::{build_tools_list, dispatch_tool_call};
use protocol::{
    read_json_rpc, send_tools_list_changed, write_json_rpc, JsonRpcError, JsonRpcResponse,
    ResultOrError,
};

pub use crate::butler_ask::looks_like_symbol_token;

pub async fn run_stdio_mode(
    butler_url: String,
    orchestrator_has_run: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    crate::mcp_diag!("MCP Bridge ready (waiting for client initialize request)");

    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();

    loop {
        match read_json_rpc(&mut reader).await {
            Ok(Some(req)) => {
                crate::mcp_diag!("Received: method={}", req.method);

                if req.id.is_none() {
                    crate::mcp_diag!("Ignoring notification (no id)");
                    continue;
                }

                let mut should_send_list_changed = false;

                let response = match req.method.as_str() {
                    "initialize" => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req.id,
                        result_or_error: ResultOrError::Result {
                            result: serde_json::json!({
                                "protocolVersion": "2024-11-05",
                                "capabilities": {
                                    "tools": { "listChanged": true },
                                    "prompts": { "listChanged": false },
                                    // Advertise empty resources so clients probe happy-path, not Method Not Found.
                                    "resources": { "listChanged": false, "subscribe": false }
                                },
                                "serverInfo": {
                                    "name": "butler-mcp",
                                    "version": env!("CARGO_PKG_VERSION")
                                }
                            }),
                        },
                    },
                    "tools/list" => {
                        // expert_mode fixed at process start — product belt never mutates mid-session.
                        let expert = orchestrator_has_run.load(Ordering::SeqCst);
                        JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id: req.id,
                            result_or_error: ResultOrError::Result {
                                result: serde_json::json!({ "tools": build_tools_list(expert) }),
                            },
                        }
                    }
                    "tools/call" => {
                        crate::mcp_diag!("Handling tools/call - validating proxy to butler_context");
                        let params = req.params.as_ref();
                        let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str()).unwrap_or("");
                        let arguments = params
                            .and_then(|p| p.get("arguments"))
                            .cloned()
                            .unwrap_or_else(|| params.cloned().unwrap_or(Value::Object(Default::default())));

                        let (result, send_flag) = dispatch_tool_call(name, arguments, params, &butler_url, &orchestrator_has_run).await;
                        should_send_list_changed = send_flag;

                        JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id: req.id,
                            result_or_error: ResultOrError::Result { result },
                        }
                    }
                    "prompts/list" => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req.id,
                        result_or_error: ResultOrError::Result {
                            result: serde_json::json!({
                                "prompts": [{
                                    "name": "butler_context_instructions",
                                    "description": "Official usage guide and best practices for the butler_context tool. Call this prompt first to learn the most effective ways to retrieve precise code context (keyword style vs surgical tracing).",
                                    "arguments": [{
                                        "name": "focus",
                                        "description": "Which usage style to emphasize in the guide: 'keyword', 'surgical', or 'both' (default)",
                                        "required": false
                                    }]
                                }]
                            }),
                        },
                    },
                    // RooCode (and other clients) probe resources even when we do not advertise
                    // the capability. Return empty lists — Method Not Found can mark the server broken.
                    "resources/list" => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req.id,
                        result_or_error: ResultOrError::Result {
                            result: serde_json::json!({ "resources": [] }),
                        },
                    },
                    "resources/templates/list" => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req.id,
                        result_or_error: ResultOrError::Result {
                            result: serde_json::json!({ "resourceTemplates": [] }),
                        },
                    },
                    "ping" => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req.id,
                        result_or_error: ResultOrError::Result {
                            result: serde_json::json!({}),
                        },
                    },
                    "prompts/get" => {
                        crate::mcp_diag!("Handling prompts/get");
                        let prompt_name = req.params
                            .as_ref()
                            .and_then(|p| p.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        if prompt_name != "butler_context_instructions" {
                            JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                id: req.id,
                                result_or_error: ResultOrError::Error {
                                    error: JsonRpcError {
                                        code: -32602,
                                        message: format!("Unknown prompt: '{}'. Only 'butler_context_instructions' is supported.", prompt_name),
                                        data: None,
                                    },
                                },
                            }
                        } else {
                            let args = req.params
                                .as_ref()
                                .and_then(|p| p.get("arguments"))
                                .and_then(|v| v.as_object())
                                .cloned()
                                .unwrap_or_default();
                            let focus = args.get("focus").and_then(|v| v.as_str()).unwrap_or("both");
                            let intro = match focus {
                                "keyword" => "Legacy butler_context keyword style (deprecated — prefer butler_orchestrate).",
                                "surgical" => "Legacy butler_context surgical / target_file + target_line style (deprecated — prefer butler_orchestrate).",
                                _ => "Primary guide for butler_orchestrate (exploration, traces, architectural summaries)."
                            };
                            let full_guide = format!(
                                "You are an expert at using the Butler code context tool.\n\n{}\n\n---\n\n{}",
                                intro, BUTLER_ORCHESTRATE_INSTRUCTIONS
                            );
                            JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                id: req.id,
                                result_or_error: ResultOrError::Result {
                                    result: serde_json::json!({
                                        "prompt": {
                                            "name": "butler_context_instructions",
                                            "description": "Official usage guide for butler_orchestrate (and legacy butler_context)",
                                            "messages": [{
                                                "role": "user",
                                                "content": { "type": "text", "text": full_guide }
                                            }]
                                        }
                                    }),
                                },
                            }
                        }
                    }
                    _ => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req.id,
                        result_or_error: ResultOrError::Error {
                            error: JsonRpcError {
                                code: -32601,
                                message: format!(
                                    "Unsupported method: '{}'. This MCP server supports: initialize, tools/list, tools/call, prompts/list, prompts/get, resources/list, resources/templates/list, ping.",
                                    req.method
                                ),
                                data: None,
                            },
                        },
                    },
                };

                if let Err(e) = write_json_rpc(&mut stdout, &response).await {
                    crate::mcp_diag!("Write error: {}", e);
                }
                if should_send_list_changed {
                    if let Err(e) = send_tools_list_changed(&mut stdout).await {
                        crate::mcp_diag!(
                            "Failed to send notifications/tools/list_changed: {}",
                            e
                        );
                    }
                }
            }
            Ok(None) => break,
            Err(e) => crate::mcp_diag!("Parse error: {}", e),
        }
    }
    Ok(())
}

/// Thin HTTP proxy mode (streamable-http for some clients). Forwards /context and serves
/// a simple manifest + health. Binds hardcoded 0.0.0.0:8002 (preserve exact prior behavior).
pub async fn run_http_proxy(butler_url: String) -> Result<(), Box<dyn std::error::Error>> {
    let butler_url_for_router = butler_url.clone();
    let app = Router::new()
        .route(
            "/mcp/manifest",
            get(move || mcp_manifest(butler_url_for_router.clone())),
        )
        .route("/mcp/health", get(mcp_health))
        .route(
            "/context",
            post(move |body: Json<Value>| {
                let url = butler_url.clone();
                async move {
                    crate::mcp_diag!("HTTP /context proxy called with body");
                    let client = reqwest::Client::new();
                    let resp = crate::config::apply_client_auth(
                        client.post(format!("{}/context", url)).json(&body.0),
                    )
                    .send()
                    .await
                    .unwrap_or_else(|e| {
                        crate::mcp_diag!("Proxy error: {}", e);
                        panic!("proxy failed: {}", e);
                    });
                    let bytes = resp.bytes().await.unwrap_or_default();
                    crate::mcp_diag!("Proxy response bytes len: {}", bytes.len());
                    (StatusCode::OK, bytes)
                }
            }),
        )
        .with_state(());

    let listener = TcpListener::bind("0.0.0.0:8002").await?;
    crate::mcp_diag!("Butler MCP Bridge listening on http://0.0.0.0:8002 (streamable-http)");
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Butler MCP Bridge - supports stdio + streamable-http"
)]
struct Args {
    #[arg(long, default_value_t = false)]
    stdio: bool,
    /// Backend Butler HTTP base URL. Falls back to `BUTLER_URL` env, then localhost:8002.
    #[arg(long, default_value = "")]
    url: String,
}

fn resolve_butler_url(cli_url: &str) -> String {
    let from_cli = cli_url.trim();
    if !from_cli.is_empty() {
        return from_cli.to_string();
    }
    std::env::var("BUTLER_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://localhost:8002".to_string())
}

#[tokio::main]
pub async fn run_main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let butler_url = resolve_butler_url(&args.url);
    let settings = crate::config::ButlerSettings::new();
    let orchestrator_has_run = Arc::new(AtomicBool::new(settings.agent.expert_mode));

    crate::mcp_diag!(
        "Butler MCP Bridge starting. stdio={}, url={}",
        args.stdio, butler_url
    );

    if args.stdio {
        run_stdio_mode(butler_url, orchestrator_has_run).await
    } else {
        run_http_proxy(butler_url).await
    }
}

#[cfg(test)]
mod ask_remap_tests {
    use crate::butler_ask::{looks_like_symbol_token, remap_butler_ask_args};

    #[test]
    fn ask_symbol_only_routes_to_trace() {
        let args = remap_butler_ask_args(serde_json::json!({
            "project": "/projects/test_repos/redis",
            "symbol": "addReply"
        }));
        assert_eq!(args["goal"], "TraceBlastRadius");
        assert_eq!(args["target_symbol"], "addReply");
        assert_eq!(args["prompt"], "addReply");
        assert_eq!(args["detail"], "compact");
        assert_eq!(args["mcp_tool_name"], "butler_orchestrate");
    }

    #[test]
    fn ask_mode_arch_with_scope() {
        let args = remap_butler_ask_args(serde_json::json!({
            "project": "/p",
            "mode": "arch",
            "scope_paths": ["src/"]
        }));
        assert_eq!(args["goal"], "ArchitecturalSummary");
    }

    #[test]
    fn ask_mode_find() {
        let args = remap_butler_ask_args(serde_json::json!({
            "project": "/p",
            "symbol": "Command",
            "mode": "find"
        }));
        assert_eq!(args["goal"], "FindImplementation");
        assert_eq!(args["target_symbol"], "Command");
    }

    #[test]
    fn looks_like_symbol_accepts_qualified() {
        assert!(looks_like_symbol_token("mozilla::Mutex"));
        assert!(looks_like_symbol_token("createClient"));
        assert!(!looks_like_symbol_token("how does locking work in gecko"));
    }
}
