//! JSON-RPC and MCP stdio protocol helpers, extracted using Strangler Fig pattern.
//! Keeps the bin thin and the protocol logic reusable/testable.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};

/// RooCode pipes stderr and treats any line *without* the substring "INFO" as an error
/// in the MCP Logs tab (and sets `server.error`). Keep the happy path silent.
/// Enable with `BUTLER_MCP_DEBUG=1` (lines are prefixed `INFO` so Roo stays green).
#[inline]
pub fn mcp_debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        matches!(
            std::env::var("BUTLER_MCP_DEBUG").as_deref(),
            Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
        )
    })
}

/// Diagnostic log to stderr. No-op unless `BUTLER_MCP_DEBUG=1`.
/// Always prefixes `INFO` so RooCode does not mark the server unhealthy.
#[macro_export]
macro_rules! mcp_diag {
    ($($arg:tt)*) => {{
        if $crate::mcp::protocol::mcp_debug_enabled() {
            eprintln!("INFO [MCP] {}", format!($($arg)*));
        }
    }};
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(flatten)]
    pub result_or_error: ResultOrError,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ResultOrError {
    Result { result: serde_json::Value },
    Error { error: JsonRpcError },
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Turns a technical error into a short, user-friendly message with an actionable tip.
pub fn friendly_mcp_error(message: &str, tip: Option<&str>) -> String {
    match tip {
        Some(t) => format!("{}\n\nTip: {}", message, t),
        None => message.to_string(),
    }
}

pub async fn read_json_rpc(
    reader: &mut TokioBufReader<tokio::io::Stdin>,
) -> Result<Option<JsonRpcRequest>, Box<dyn std::error::Error>> {
    loop {
        let mut line = String::new();
        // Read a single line terminated by a newline
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None); // EOF (Client disconnected)
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue; // Ignore rogue blank lines
        }

        crate::mcp_diag!("RAW IN {}", trimmed);

        // Parse the raw line directly into our JSON-RPC request object
        let req: JsonRpcRequest = serde_json::from_str(trimmed)?;
        return Ok(Some(req));
    }
}

pub async fn write_json_rpc(
    writer: &mut tokio::io::Stdout,
    response: &JsonRpcResponse,
) -> Result<(), Box<dyn std::error::Error>> {
    // Serialize the response and append a standard newline
    let mut body = serde_json::to_string(response)?;
    body.push('\n');

    // Write directly to stdout
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Send the standard MCP notification to tell clients that the tool list has changed
/// (used after the first successful `butler_orchestrate` to unlock the full suite).
pub async fn send_tools_list_changed(
    writer: &mut tokio::io::Stdout,
) -> Result<(), Box<dyn std::error::Error>> {
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed"
    });
    let mut body = serde_json::to_string(&notif)?;
    body.push('\n');
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    crate::mcp_diag!("Sent notifications/tools/list_changed (tool list unlocked)");
    Ok(())
}
